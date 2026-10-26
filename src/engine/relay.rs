//! The media bytes, fetched by the app itself and handed to ffmpeg (picture) and
//! mpv (sound) through a small HTTP server on localhost.
//!
//! The site serves a bounded range request at full speed, but holds an
//! open-ended one for a large file (what ffmpeg sends) to about twice its
//! bitrate, so a decoder reading straight from it never gets ahead and catches
//! up only slowly after a slow moment. And an open-ended download goes on as far
//! as the reader's socket takes, whatever the player wanted. The relay fetches
//! in chunks instead and keeps a set amount buffered past where the player
//! reads. That also
//! - measures the connection (see `abr::Meter`),
//! - holds video back while the sound runs low (`hold`), and
//! - leaves no download open during a pause for the site to cut off.
//!
//! Only single files the site serves in parts go through here; HLS playlists
//! name their segments with links of their own.

use super::abr::Meter;
use crate::platform;
use crate::provider::Header;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Media time to keep fetched past the player, and the memory that may take at
/// most. mpv keeps a buffer of its own, so the sound needs less here.
const AHEAD: f64 = 30.0;
const SOUND_AHEAD: f64 = 10.0;
const MAX_AHEAD: u64 = 6 << 20;
const MIN_AHEAD: u64 = 192 << 10;
/// Once the buffer is full, fetching resumes when it has dropped below this
/// share of it: refills in a few large pieces measure the line better than a
/// trickle of small ones, whose time goes mostly to the round trip.
const REFILL_AT: f64 = 0.75;
/// Bytes kept behind the player, in case it reads a little way back.
const BEHIND: u64 = 256 << 10;
/// A request gets at least this much past where it starts, however full the
/// player (see `Source::set_player_full`): it may read an index elsewhere.
const FIRST_SERVE: u64 = 256 << 10;
/// The first chunk after a jump is small (the player may only want a header from
/// there); the next ones double up to about `CHUNK_SECS` of media.
const FIRST_CHUNK: u64 = 32 << 10;
const CHUNK_SECS: f64 = 6.0;
const MIN_CHUNK: u64 = 64 << 10;
const MAX_CHUNK: u64 = 4 << 20;
/// A fetch that is no longer wanted (or must make way) with at most this much
/// to come is read to its end rather than broken off, keeping its connection.
const DRAIN: u64 = 64 << 10;
/// A response must start within this.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
/// Wait between attempts after a failed fetch, at most.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

pub struct Relay {
    port: u16,
    next_id: AtomicU64,
    sources: Arc<Mutex<HashMap<u64, Weak<Source>>>>,
    agent: ureq::Agent,
    meter: Arc<Meter>,
    hold: Arc<AtomicBool>,
}

impl Relay {
    /// `hold`: while set, no new video is fetched (the sound needs the line).
    pub fn start(meter: Arc<Meter>, hold: Arc<AtomicBool>) -> std::io::Result<Relay> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let sources: Arc<Mutex<HashMap<u64, Weak<Source>>>> = Arc::default();
        let lookup = sources.clone();
        std::thread::Builder::new().name("relay".into()).spawn(move || {
            for conn in listener.incoming().flatten() {
                let lookup = lookup.clone();
                let _ = std::thread::Builder::new().name("relay-conn".into()).spawn(move || serve(conn, &lookup));
            }
        })?;
        Ok(Relay { port, next_id: AtomicU64::new(1), sources, agent: platform::stream_agent(ANSWER_TIMEOUT), meter, hold })
    }

    /// Starts fetching `url` (a file of `size` bytes, media of `kbps`) for one
    /// player: the sound (`sound`, never held back) or a video decoder. Fetching
    /// stops when the returned source is dropped.
    pub fn open(&self, url: &str, headers: &[Header], kbps: u32, size: u64, sound: bool) -> Arc<Source> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let source = Arc::new(Source {
            remote: url.to_string(),
            headers: headers.to_vec(),
            local: format!("http://127.0.0.1:{}/{id}", self.port),
            byte_rate: (kbps.max(32) as f64 * 1000.0 / 8.0).max(1.0),
            sound,
            state: Mutex::new(State {
                len: size,
                start: 0,
                data: VecDeque::new(),
                read: 0,
                epoch: 0,
                conn: 0,
                chunk: FIRST_CHUNK,
                failed: None,
                wanted: false,
                filling: true,
                last_data: Instant::now(),
            }),
            wake: Condvar::new(),
            closed: AtomicBool::new(false),
            standby: AtomicBool::new(false),
            player_full: AtomicBool::new(false),
        });
        let mut sources = self.sources.lock().unwrap();
        sources.retain(|_, s| s.strong_count() > 0);
        sources.insert(id, Arc::downgrade(&source));
        let (fetcher, agent, meter, hold) = (source.clone(), self.agent.clone(), self.meter.clone(), self.hold.clone());
        let _ = std::thread::Builder::new().name("relay-fetch".into()).spawn(move || fetch_loop(&fetcher, &agent, &meter, &hold));
        source
    }
}

pub struct Source {
    remote: String,
    headers: Vec<Header>,
    local: String,
    /// Bytes per second of media.
    byte_rate: f64,
    sound: bool,
    state: Mutex<State>,
    wake: Condvar,
    closed: AtomicBool,
    /// Fetches nothing new for now (a replacement needs the line).
    standby: AtomicBool,
    /// The player has all it means to cache: it is handed nothing more for now.
    player_full: AtomicBool,
}

struct State {
    len: u64,
    /// `data` holds the stream from `start` on.
    start: u64,
    data: VecDeque<u8>,
    /// Where the player reads next.
    read: u64,
    /// Bumped when the player jumps elsewhere: a fetch for the old place stops.
    epoch: u64,
    /// The player's newest connection; older ones stop serving.
    conn: u64,
    chunk: u64,
    failed: Option<String>,
    /// The player has asked for something (fetching starts with its first request).
    wanted: bool,
    /// Fetching until the buffer is full (see `REFILL_AT`).
    filling: bool,
    last_data: Instant,
}

impl State {
    fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    fn complete(&self) -> bool {
        self.end() >= self.len
    }
}

impl Source {
    /// The address the player reads from.
    pub fn url(&self) -> &str {
        &self.local
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn target(&self) -> u64 {
        ((self.byte_rate * if self.sound { SOUND_AHEAD } else { AHEAD }) as u64).clamp(MIN_AHEAD, MAX_AHEAD)
    }

    /// Media seconds fetched past where the player reads; `None` once the rest of the
    /// file is in.
    pub fn ahead(&self) -> Option<f64> {
        let st = self.lock();
        (!st.complete()).then(|| st.end().saturating_sub(st.read) as f64 / self.byte_rate)
    }

    /// As much is fetched ahead as is kept (and not topped up yet), or all of it.
    pub fn full(&self) -> bool {
        let st = self.lock();
        st.complete() || !st.filling
    }

    /// How long no bytes have come in although some were wanted.
    pub fn starved_for(&self) -> Duration {
        let st = self.lock();
        if st.complete() || st.end() > st.read {
            Duration::ZERO
        } else {
            st.last_data.elapsed()
        }
    }

    /// The site refused the stream (an expired link, say).
    pub fn failed(&self) -> bool {
        self.lock().failed.is_some()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn set_standby(&self, on: bool) {
        if self.standby.swap(on, Ordering::Relaxed) != on {
            self.wake.notify_all();
        }
    }

    /// While the player has all it means to cache, it is handed nothing more, and
    /// so nothing further is fetched either. What a player does not take into its
    /// cache waits in the connection to it, in a buffer the OS grows by itself to
    /// megabytes, and on a slow line that download takes the line from the picture.
    pub fn set_player_full(&self, full: bool) {
        if self.player_full.swap(full, Ordering::Relaxed) != full {
            self.wake.notify_all();
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.close();
    }
}

fn fetch_loop(src: &Arc<Source>, agent: &ureq::Agent, meter: &Meter, hold: &AtomicBool) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let (from, to, epoch) = {
            let mut st = src.lock();
            loop {
                if src.closed.load(Ordering::Relaxed) || st.failed.is_some() {
                    return;
                }
                let ahead = st.end().saturating_sub(st.read);
                if ahead >= src.target() {
                    st.filling = false;
                } else if ahead < (src.target() as f64 * REFILL_AT) as u64 {
                    st.filling = true;
                }
                let held = (!src.sound && hold.load(Ordering::Relaxed)) || src.standby.load(Ordering::Relaxed);
                if st.wanted && !st.complete() && st.filling && !held {
                    break;
                }
                if held {
                    // Waiting on purpose is not starving.
                    st.last_data = Instant::now();
                }
                st = src.wake.wait_timeout(st, Duration::from_millis(200)).unwrap().0;
            }
            // Fetch no further than the buffer should reach, but not in tiny bits.
            let room = src.target().saturating_sub(st.end().saturating_sub(st.read)).max(MIN_CHUNK);
            let size = st.chunk.min(room).max(1);
            let to = st.len.min(st.end() + size) - 1;
            (st.end(), to, st.epoch)
        };
        match fetch(src, agent, meter, hold, from, to, epoch) {
            Ok(()) => {
                backoff = Duration::from_millis(500);
                let mut st = src.lock();
                if st.epoch == epoch {
                    let most = ((src.byte_rate * CHUNK_SECS) as u64).clamp(MIN_CHUNK, MAX_CHUNK);
                    st.chunk = (st.chunk * 2).min(most);
                }
            }
            Err(Fault::Refused(why)) => {
                src.lock().failed = Some(why);
                src.wake.notify_all();
                return;
            }
            Err(Fault::Moved | Fault::Held) => {}
            Err(Fault::Network) => {
                let st = src.lock();
                // Wait, unless the player jumped elsewhere meanwhile.
                let _ = src.wake.wait_timeout_while(st, backoff, |st| st.epoch == epoch && !src.closed.load(Ordering::Relaxed));
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

enum Fault {
    /// The site said no for good.
    Refused(String),
    /// The player went elsewhere; the bytes are not wanted any more.
    Moved,
    /// The line is needed elsewhere; what came is kept, the rest fetched later.
    Held,
    /// Try again.
    Network,
}

fn fetch(src: &Source, agent: &ureq::Agent, meter: &Meter, hold: &AtomicBool, from: u64, to: u64, epoch: u64) -> Result<(), Fault> {
    let mut req = agent.get(&src.remote);
    for h in &src.headers {
        req = req.header(&h.name, &h.value);
    }
    // Generous for the size at the speed measured so far; what arrives is kept anyway.
    let expected = (to + 1 - from) as f64 * 8.0 / meter.estimate().unwrap_or(250_000.0).max(20_000.0);
    let req = req
        .header("Range", &format!("bytes={from}-{to}"))
        .config()
        .timeout_recv_body(Some(Duration::from_secs_f64((expected * 4.0).clamp(20.0, 180.0))))
        .build();
    // The sound's fetches are small and come seldom: on their own they would
    // measure the round trip more than the line. They count alongside video's.
    if !src.sound {
        meter.fetch_started();
    }
    let result = (|| {
        let asked = Instant::now();
        let resp = req.call().map_err(|e| match e {
            ureq::Error::StatusCode(code @ (401..=404 | 410)) => Fault::Refused(format!("the site refused the stream ({code})")),
            _ => Fault::Network,
        })?;
        if !src.sound {
            meter.answered(asked.elapsed());
        }
        // A site that ignores the range sends the file from its start: take no
        // more than was asked. A ranged answer is read to its end, which hands the
        // connection back for the next fetch (a new one costs several round trips).
        let ranged = resp.status() == 206;
        if !ranged && from > 0 {
            return Err(Fault::Refused("the site does not serve parts of the stream".into()));
        }
        let mut body = resp.into_body().into_reader().take(if ranged { u64::MAX } else { to + 1 - from });
        let mut buf = vec![0u8; 16 << 10];
        let mut left = to + 1 - from;
        let mut moved = false;
        loop {
            let n = body.read(&mut buf).map_err(|_| Fault::Network)?;
            if n == 0 {
                return if moved { Err(Fault::Moved) } else { Ok(()) };
            }
            left = left.saturating_sub(n as u64);
            if src.sound {
                meter.shared(n);
            } else {
                meter.received(n);
            }
            if !moved {
                let mut st = src.lock();
                moved = st.epoch != epoch || src.closed.load(Ordering::Relaxed);
                if !moved {
                    st.data.extend(&buf[..n]);
                    st.last_data = Instant::now();
                    drop(st);
                    src.wake.notify_all();
                }
            }
            // Breaking off costs the connection, and a new one several round
            // trips: the last few bytes are cheaper to take.
            let held = !src.sound && (hold.load(Ordering::Relaxed) || src.standby.load(Ordering::Relaxed));
            if (moved || held) && left > DRAIN {
                return Err(if moved { Fault::Moved } else { Fault::Held });
            }
        }
    })();
    if !src.sound {
        meter.fetch_ended();
    }
    result
}

/// Copies `len` bytes from `data` at `at` (both within it).
fn copy_out(data: &VecDeque<u8>, at: usize, len: usize) -> Vec<u8> {
    let (a, b) = data.as_slices();
    let mut out = Vec::with_capacity(len);
    if at < a.len() {
        let first = (a.len() - at).min(len);
        out.extend_from_slice(&a[at..at + first]);
        out.extend_from_slice(&b[..len - first]);
    } else {
        out.extend_from_slice(&b[at - a.len()..at - a.len() + len]);
    }
    out
}

/// Bytes the OS may hold between the relay and a player, on each side. Small, so
/// that what the relay has handed over is about what the player has read: the
/// buffer it reports is the real one, and nothing more is fetched for a player
/// that has stopped reading. Players seek over gaps larger than this.
pub const SOCKET_BUFFER: i32 = 32 << 10;

/// Answers one request from a player: `GET /<id>` with an optional `Range: bytes=N-`.
fn serve(conn: TcpStream, sources: &Mutex<HashMap<u64, Weak<Source>>>) {
    let _ = conn.set_nodelay(true);
    platform::limit_send_buffer(&conn, SOCKET_BUFFER);
    let mut reader = BufReader::new(match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut out = conn;
    let mut request = String::new();
    let mut from = 0u64;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if request.is_empty() {
            request = line.trim().to_string();
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                from = value.trim().strip_prefix("bytes=").and_then(|r| r.split('-').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
            }
        }
    }
    let id = request.split_whitespace().nth(1).and_then(|p| p.trim_start_matches('/').parse::<u64>().ok());
    let Some(src) = id.and_then(|id| sources.lock().unwrap().get(&id).and_then(Weak::upgrade)) else {
        let _ = out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };

    // Take over from any earlier connection, at the new place.
    let (me, len) = {
        let mut st = src.lock();
        st.conn += 1;
        if from < st.start || from > st.end() {
            st.start = from;
            st.data.clear();
            st.epoch += 1;
            st.chunk = FIRST_CHUNK;
            st.filling = true;
        }
        st.read = from;
        st.wanted = true;
        st.last_data = Instant::now();
        src.wake.notify_all();
        (st.conn, st.len)
    };
    if src.failed() {
        let _ = out.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    if from >= len {
        let _ = out.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        return;
    }
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {from}-{}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        len - 1,
        len - from
    );
    if out.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut pos = from;
    while pos < len {
        let piece = {
            let mut st = src.lock();
            loop {
                if src.closed.load(Ordering::Relaxed) || st.conn != me || st.failed.is_some() || pos < st.start {
                    return;
                }
                let held = src.player_full.load(Ordering::Relaxed) && pos >= from + FIRST_SERVE;
                if pos < st.end() && !held {
                    let at = (pos - st.start) as usize;
                    let n = ((st.end() - pos) as usize).min(64 << 10);
                    break copy_out(&st.data, at, n);
                }
                st = src.wake.wait_timeout(st, Duration::from_millis(250)).unwrap().0;
            }
        };
        if out.write_all(&piece).is_err() {
            return;
        }
        pos += piece.len() as u64;
        let mut st = src.lock();
        if st.conn == me {
            st.read = pos;
            let behind = pos.saturating_sub(st.start);
            if behind > BEHIND * 2 {
                let drop = (behind - BEHIND) as usize;
                st.data.drain(..drop);
                st.start += drop as u64;
            }
            drop(st);
            src.wake.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_across_the_ring() {
        let mut d: VecDeque<u8> = VecDeque::with_capacity(8);
        d.extend([9, 9, 9, 9, 9, 9]);
        d.drain(..5);
        d.extend([1, 2, 3, 4, 5, 6]);
        assert_eq!(copy_out(&d, 0, 4), [9, 1, 2, 3]);
        assert_eq!(copy_out(&d, 2, 5), [2, 3, 4, 5, 6]);
    }
}
