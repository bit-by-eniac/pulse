//! Video frames: an `ffmpeg` child decodes the chosen stream, scales it to the
//! size it will be shown at and writes raw RGBA frames to a pipe. A thread
//! releases each frame when the playback clock (driven by the audio) reaches it.
//!
//! A new size or stream needs a new decoder, which takes a second or two to
//! connect and seek. Until it has a frame ready the old one keeps showing: each
//! session publishes under its own number, and the newest one to publish wins.

use super::clock::Clock;
use super::relay::{Relay, Source};
use super::Waker;
use crate::platform;
use crate::provider::Header;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FPS: f64 = 30.0;
/// Frame buffers kept for reuse (8 MB each at 1080p). Two cover the decoder and
/// the UI taking turns.
const MAX_SPARE: usize = 2;

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// One pixel per element, RGBA bytes in memory order. Whole `u32`s keep the
    /// buffer aligned, so the UI can hand it to the GPU without copying it.
    pub pixels: Vec<u32>,
}

/// Hands the newest frame from the decoder thread to the UI thread.
#[derive(Default)]
pub struct FrameSlot {
    inner: Mutex<SlotInner>,
}

#[derive(Default)]
struct SlotInner {
    latest: Option<Frame>,
    spare: Vec<Vec<u32>>,
    /// Session whose frames are shown; older sessions are ignored from then on.
    owner: u64,
    /// No decoder will run for a while: buffers handed back are freed, not kept.
    released: bool,
}

impl SlotInner {
    fn keep(&mut self, buf: Vec<u32>) {
        if !self.released && self.spare.len() < MAX_SPARE {
            self.spare.push(buf);
        }
    }
}

impl FrameSlot {
    pub fn take(&self) -> Option<Frame> {
        self.inner.lock().unwrap().latest.take()
    }

    /// Returns a buffer so the decoder can reuse it instead of allocating.
    pub fn recycle(&self, buf: Vec<u32>) {
        self.inner.lock().unwrap().keep(buf);
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().latest = None;
    }

    /// Frees the frame and the spare buffers (8 MB each at 1080p), for when video
    /// stops for a while: audio mode, hidden, the queue ran out. The next decoder
    /// allocates afresh.
    pub fn release(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.latest = None;
        inner.spare = Vec::new();
        inner.released = true;
    }

    /// Shows `frame` unless a newer session already shows its frames.
    fn publish(&self, session: u64, frame: Frame) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if session < inner.owner {
            inner.keep(frame.pixels);
            return false;
        }
        inner.owner = session;
        inner.latest = Some(frame);
        true
    }

    fn buffer(&self, len: usize) -> Vec<u32> {
        let mut inner = self.inner.lock().unwrap();
        while let Some(mut buf) = inner.spare.pop() {
            if buf.len() == len {
                return std::mem::take(&mut buf);
            }
        }
        vec![0; len]
    }
}

pub struct Params<'a> {
    pub ffmpeg: &'a Path,
    pub url: &'a str,
    pub headers: &'a [Header],
    pub start: f64,
    pub live: bool,
    pub width: u32,
    pub height: u32,
    /// The stream's codec family ("avc1", "vp9", "av01", ...).
    pub codec: &'a str,
    pub kbps: u32,
    /// Fetches the file (of this many bytes) for ffmpeg, see `relay`; `None`:
    /// ffmpeg reads the link itself.
    pub relay: Option<(&'a Relay, u64)>,
}

pub struct VideoSession {
    child: Child,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    pub track_id: String,
    pub stream_height: u32,
    /// The stream decoded (its link) and its bitrate.
    pub stream_url: String,
    pub kbps: u32,
    pub width: u32,
    /// Where in the track its frames begin.
    pub start: f64,
    started: Instant,
    /// Milliseconds from start to the first decoded frame, plus one (0 = none yet).
    first_frame: Arc<AtomicU64>,
    /// Timestamp of the newest frame decoded (`f64` bits; NaN = none yet).
    newest: Arc<AtomicU64>,
    source: Option<Arc<Source>>,
}

impl VideoSession {
    /// `session` must grow with every call. Frames start at `params.start`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        params: Params,
        session: u64,
        track_id: String,
        stream_height: u32,
        clock: Arc<Clock>,
        slot: Arc<FrameSlot>,
        waker: Waker,
    ) -> Result<VideoSession, String> {
        let mut cmd = Command::new(params.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        // Each decoder thread holds frames of its own. H.264 needs few to keep up;
        // VP9 and AV1 are heavier to decode.
        cmd.args(["-threads", if params.codec.starts_with("avc") { "2" } else { "4" }]);
        let source = params.relay.filter(|_| !params.live).map(|(relay, size)| relay.open(params.url, params.headers, params.kbps, size, false));
        let url = source.as_ref().map_or(params.url, |s| s.url());
        if source.is_some() {
            // The relay deals with the network and its outages; ffmpeg just waits.
            cmd.args(["-rw_timeout", "300000000"]);
            // ffmpeg reads on rather than seeking over a gap smaller than its socket's
            // receive buffer, which the OS grows to megabytes on a local connection:
            // on a slow line that is the whole way to the start point, downloaded.
            // A small buffer also lets the relay see how far ffmpeg has read. 5.0+
            // takes the gap directly as well.
            let small = super::relay::SOCKET_BUFFER.to_string();
            cmd.args(["-recv_buffer_size", &small]);
            if version_at_least(params.ffmpeg, 5) {
                cmd.args(["-short_seek_size", &small]);
            }
        } else {
            if !params.headers.is_empty() {
                let headers: String = params.headers.iter().map(|h| format!("{}: {}\r\n", h.name, h.value)).collect();
                cmd.args(["-headers", &headers]);
            }
            // A connection that went silent (Wi-Fi change, sleep) would block a read
            // forever; this way it fails, and gets resumed or the decoder replaced.
            cmd.args(["-rw_timeout", "10000000"]);
        }
        if params.live {
            // Live HLS arrives a segment at a time; let the reader queue compressed
            // packets (a few MB) so a slow segment does not stall the picture.
            cmd.args(["-thread_queue_size", "1024"]);
        } else {
            // As for audio: reconnecting helps long videos and stalls live HLS.
            cmd.args(["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"]);
        }
        if !params.live && params.start > 0.5 {
            cmd.args(["-ss", &format!("{:.3}", params.start)]);
        }
        cmd.args(["-i", url, "-an", "-sn", "-dn", "-vf"])
            .arg(format!("fps={FPS},scale={}:{}:flags=fast_bilinear", params.width, params.height))
            .args(["-pix_fmt", "rgba"]);
        // 7.0+ runs its stages on threads with queues between them, which is also
        // when `-thread_queue_size` is accepted for an output.
        if version_at_least(params.ffmpeg, 7) {
            // Frames are read at playback speed; without a limit ffmpeg 7+ queues
            // several finished frames (8 MB each at 1080p) ahead of the pipe.
            cmd.args(["-thread_queue_size", "1"]);
        }
        cmd.args(["-f", "rawvideo", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        platform::quiet(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("could not start ffmpeg: {e}"))?;
        platform::end_with_app(&child);
        slot.inner.lock().unwrap().released = false;
        let mut stdout = child.stdout.take().ok_or("no ffmpeg output")?;

        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let first_frame = Arc::new(AtomicU64::new(0));
        let newest = Arc::new(AtomicU64::new(f64::NAN.to_bits()));
        let started = Instant::now();
        let (w, h) = (params.width as usize, params.height as usize);
        let start = if params.live { 0.0 } else { params.start.max(0.0) };
        let live = params.live;
        let (stop_t, done_t, frames_t, first_t, newest_t) = (stop.clone(), done.clone(), frames.clone(), first_frame.clone(), newest.clone());
        std::thread::Builder::new()
            .name("video-frames".into())
            .spawn(move || {
                let mut index: u64 = 0;
                // For live streams there is no shared timeline: show frames as they come.
                let mut live_origin: Option<f64> = None;
                'frames: loop {
                    let mut buf = slot.buffer(w * h);
                    if stdout.read_exact(bytemuck::cast_slice_mut(&mut buf)).is_err() {
                        break;
                    }
                    if index == 0 {
                        first_t.store(started.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
                    }
                    let mut pts = start + index as f64 / FPS;
                    index += 1;
                    if live {
                        let origin = *live_origin.get_or_insert(clock.now() - pts);
                        pts += origin;
                    }
                    newest_t.store(pts.to_bits(), Ordering::Relaxed);
                    loop {
                        if stop_t.load(Ordering::Relaxed) {
                            break 'frames;
                        }
                        let delta = pts - clock.now();
                        if delta <= 0.005 {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs_f64(delta.min(0.02)));
                    }
                    // Decoding runs ahead of real time after a seek; skip what is already late.
                    if pts + 0.15 < clock.now() {
                        slot.recycle(buf);
                        continue;
                    }
                    if slot.publish(session, Frame { width: w, height: h, pixels: buf }) {
                        frames_t.fetch_add(1, Ordering::Relaxed);
                        waker();
                    }
                }
                done_t.store(true, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string())?;

        Ok(VideoSession {
            child,
            stop,
            done,
            frames,
            track_id,
            stream_height,
            stream_url: params.url.to_string(),
            kbps: params.kbps,
            width: params.width,
            start: params.start,
            started,
            first_frame,
            newest,
            source,
        })
    }

    /// How far the picture is behind the sound (seconds), once it has started.
    /// It grows while the decoder waits for data.
    pub fn behind(&self, now: f64) -> Option<f64> {
        let newest = f64::from_bits(self.newest.load(Ordering::Relaxed));
        (!newest.is_nan()).then_some(now - newest)
    }

    /// Media seconds fetched ahead of the decoder: `None` when ffmpeg reads the
    /// link itself, infinite once the rest of the file is in.
    pub fn buffered(&self) -> Option<f64> {
        self.source.as_ref().map(|s| s.ahead().unwrap_or(f64::INFINITY))
    }

    /// The relay holds as much ahead as it keeps (or the whole file).
    pub fn buffer_full(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.full())
    }

    /// How long the decoder has been waiting for data that does not come.
    pub fn starved_for(&self) -> Duration {
        self.source.as_ref().map_or(Duration::ZERO, |s| s.starved_for())
    }

    /// Lets a replacement have the line while this one has enough buffered.
    pub fn set_standby(&self, on: bool) {
        if let Some(source) = &self.source {
            source.set_standby(on);
        }
    }

    /// The site refused the stream.
    pub fn refused(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.failed())
    }

    /// How long ffmpeg took to deliver its first frame, once it has.
    pub fn startup(&self) -> Option<Duration> {
        match self.first_frame.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(Duration::from_millis(ms - 1)),
        }
    }

    pub fn age(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub fn frames_shown(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// Whether this ffmpeg's major version is at least `major`. Its banner is asked
/// once per ffmpeg.
fn version_at_least(ffmpeg: &Path, major: u32) -> bool {
    static KNOWN: Mutex<Option<(std::path::PathBuf, String)>> = Mutex::new(None);
    let mut known = KNOWN.lock().unwrap();
    if known.as_ref().is_none_or(|(path, _)| path != ffmpeg) {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-version"]).stdin(Stdio::null()).stderr(Stdio::null());
        platform::quiet(&mut cmd);
        let banner = cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        *known = Some((ffmpeg.to_path_buf(), banner));
    }
    known.as_ref().is_some_and(|(_, banner)| major_version_at_least(banner, major))
}

/// Reads "ffmpeg version 7.1.1 ...", "n7.1", "4.4.2-0ubuntu0.22.04.1" and git
/// builds ("N-118000-g...", "2026-10-01-git-..."), which count as new.
fn major_version_at_least(banner: &str, major: u32) -> bool {
    let Some(version) = banner.split_whitespace().skip_while(|w| *w != "version").nth(1) else { return false };
    let version = version.trim_start_matches('n');
    if version.starts_with("N-") || version.split('-').next().is_some_and(|y| y.len() == 4 && y.parse::<u32>().is_ok()) {
        return true;
    }
    version.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= major)
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        // The relay's fetcher holds the source too.
        if let Some(source) = &self.source {
            source.close();
        }
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_ffmpeg_versions() {
        let new = ["ffmpeg version 9.0.2 Copyright (c) 2000-2026", "ffmpeg version n7.1 Copyright", "ffmpeg version 7.1-full_build-www.gyan.dev Copyright", "ffmpeg version N-118000-gabc Copyright", "ffmpeg version 2026-10-01-git-abc-full_build-www.gyan.dev"];
        let old = ["ffmpeg version 4.4.2-0ubuntu0.22.04.1 Copyright", "ffmpeg version 6.1.1-3ubuntu5 Copyright", "ffmpeg version 5.1.6-0+deb12u1", "", "something else"];
        for b in new {
            assert!(super::major_version_at_least(b, 7), "{b}");
        }
        for b in old {
            assert!(!super::major_version_at_least(b, 7), "{b}");
        }
    }

    #[test]
    fn frees_buffers_once_released() {
        let slot = super::FrameSlot::default();
        slot.recycle(vec![0; 4]);
        assert_eq!(slot.inner.lock().unwrap().spare.len(), 1);
        slot.release();
        slot.recycle(vec![0; 4]);
        assert!(slot.inner.lock().unwrap().spare.is_empty());
    }
}
