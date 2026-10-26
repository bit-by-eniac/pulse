//! Automatic quality: how fast the connection is, and which streams fit in it.
//!
//! The estimate comes from the video bytes the relay fetches (plus the sound
//! downloaded at the same time), as two moving averages that
//! forget at different speeds. The lower of the two counts, so a slowdown shows at
//! once and a speed-up only once it lasts.

use crate::provider::{AudioStream, VideoStream};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Smallest stretch of downloading that makes a sample; shorter ones mostly
/// measure the round trip.
const MIN_SAMPLE: Duration = Duration::from_millis(250);
const MIN_SAMPLE_BYTES: u64 = 16 * 1024;
/// A long download yields a sample this often, so a slowdown shows mid-chunk.
const SAMPLE_EVERY: Duration = Duration::from_secs(1);
/// A drop to under half the estimate over this much recent downloading (within
/// `RECENT`) counts at once (see `estimate_now`).
const RECENT: Duration = Duration::from_secs(3);
const RECENT_ENOUGH: f64 = 2.0;
/// Share of the estimate that streams are planned with, the rest is headroom.
const SAFETY: f64 = 0.8;
/// Before anything is measured, video starts at this (bit/s) and goes up or down
/// from there within seconds.
pub const UNKNOWN_START: f64 = 300_000.0;

struct Ewma {
    alpha: f64,
    value: f64,
    weight: f64,
}

impl Ewma {
    fn new(half_life: f64) -> Self {
        Self { alpha: 0.5f64.powf(1.0 / half_life), value: 0.0, weight: 0.0 }
    }

    fn add(&mut self, weight: f64, value: f64) {
        let a = self.alpha.powf(weight);
        self.value = value * (1.0 - a) + a * self.value;
        self.weight += weight;
    }

    /// Corrected for starting at zero.
    fn get(&self) -> f64 {
        self.value / (1.0 - self.alpha.powf(self.weight))
    }
}

struct Inner {
    fast: Ewma,
    slow: Ewma,
    /// Fetches in flight. A sample covers the time at least one was (several
    /// short fetches add up to one sample), and the bytes they got.
    active: u32,
    busy_since: Option<Instant>,
    busy: Duration,
    bytes: u64,
    /// What mpv downloads right now (bit/s), while it does.
    audio: Option<f64>,
    /// The samples of the last `RECENT`: when, seconds, bits.
    recent: VecDeque<(Instant, f64, f64)>,
}

/// The connection's speed, measured from what passes through it.
pub struct Meter(Mutex<Inner>);

impl Default for Meter {
    fn default() -> Self {
        Meter(Mutex::new(Inner {
            fast: Ewma::new(2.0),
            slow: Ewma::new(6.0),
            active: 0,
            busy_since: None,
            busy: Duration::ZERO,
            bytes: 0,
            audio: None,
            recent: VecDeque::new(),
        }))
    }
}

impl Inner {
    fn busy(&self, now: Instant) -> Duration {
        self.busy + self.busy_since.map_or(Duration::ZERO, |s| now.duration_since(s))
    }

    /// Takes a sample once there is enough to go on (`force`: a long stretch
    /// without data counts too).
    fn sample(&mut self, now: Instant, force: bool) {
        let busy = self.busy(now);
        if busy < MIN_SAMPLE || self.bytes < MIN_SAMPLE_BYTES && !(force && busy >= SAMPLE_EVERY) {
            return;
        }
        let secs = busy.as_secs_f64();
        // The sound shares the line; count it in, it is the line that is measured.
        let rate = self.bytes as f64 * 8.0 / secs + self.audio.unwrap_or(0.0);
        self.fast.add(secs, rate);
        self.slow.add(secs, rate);
        self.recent.retain(|&(at, _, _)| now.duration_since(at) < RECENT);
        self.recent.push_back((now, secs, rate * secs));
        self.busy = Duration::ZERO;
        self.busy_since = self.busy_since.map(|_| now);
        self.bytes = 0;
    }
}

impl Meter {
    pub fn fetch_started(&self) {
        let mut m = self.0.lock().unwrap();
        m.active += 1;
        if m.busy_since.is_none() {
            m.busy_since = Some(Instant::now());
        }
    }

    pub fn received(&self, bytes: usize) {
        let mut m = self.0.lock().unwrap();
        m.bytes += bytes as u64;
        let now = Instant::now();
        if m.busy(now) >= SAMPLE_EVERY {
            m.sample(now, false);
        }
    }

    /// A fetch's answer came after `waited`: round trips (more of them on a new
    /// connection), which a bigger piece would not take longer for. When nothing
    /// else came meanwhile, the measured time starts now. A longer silence counts
    /// (see `check_stall`).
    pub fn answered(&self, waited: Duration) {
        let mut m = self.0.lock().unwrap();
        if m.active == 1 && m.bytes == 0 && waited < SAMPLE_EVERY * 2 {
            m.busy_since = Some(Instant::now());
        }
    }

    /// Bytes of another download (the sound's) that arrived while a measured one
    /// was in flight: they shared the line with it.
    pub fn shared(&self, bytes: usize) {
        let mut m = self.0.lock().unwrap();
        if m.active > 0 {
            m.bytes += bytes as u64;
        }
    }

    pub fn fetch_ended(&self) {
        let mut m = self.0.lock().unwrap();
        m.active = m.active.saturating_sub(1);
        if m.active == 0 {
            let now = Instant::now();
            m.busy = m.busy(now);
            m.busy_since = None;
            m.sample(now, false);
        }
    }

    /// While a fetch waits for its first bytes, or the line went quiet, nothing
    /// arrives to make a sample; count the silence.
    pub fn check_stall(&self) {
        let mut m = self.0.lock().unwrap();
        let now = Instant::now();
        if m.active > 0 && m.busy(now) >= SAMPLE_EVERY * 2 {
            m.sample(now, true);
        }
    }

    /// mpv's download rate (bit/s) while it downloads, `None` while it does not.
    pub fn audio(&self, rate: Option<f64>) {
        self.0.lock().unwrap().audio = rate;
    }

    /// Bits per second, once measured.
    pub fn estimate(&self) -> Option<f64> {
        let m = self.0.lock().unwrap();
        (m.fast.weight > 0.0).then(|| m.fast.get().min(m.slow.get()))
    }

    /// As `estimate`, but a sharp drop over the last few seconds counts at once
    /// rather than as the averages follow it.
    pub fn estimate_now(&self) -> Option<f64> {
        let e = self.estimate()?;
        let m = self.0.lock().unwrap();
        let fresh = m.recent.iter().filter(|&&(at, _, _)| at.elapsed() < RECENT);
        let (secs, bits) = fresh.fold((0.0, 0.0), |(s, b), &(_, secs, bits)| (s + secs, b + bits));
        // A second alone is too noisy: the sound's downloads come and go beside it.
        Some(if secs >= RECENT_ENOUGH && bits / secs < e * 0.5 { bits / secs } else { e })
    }
}

/// What the line can spare for video (bit/s) while `audio_kbps` plays and,
/// unless it is all downloaded, keeps downloading.
pub fn video_budget(estimate: Option<f64>, audio_kbps: u32, audio_done: bool) -> Option<f64> {
    let audio = if audio_done { 0.0 } else { audio_kbps as f64 * 1000.0 };
    estimate.map(|e| (e * SAFETY - audio).max(0.0))
}

/// The sound for a connection of `estimate`: the best that takes at most about
/// a third of it, so the picture has room too. Unknown: the best.
pub fn choose_audio(audio: &[AudioStream], estimate: Option<f64>) -> usize {
    let Some(e) = estimate else { return audio.len().saturating_sub(1) };
    audio.iter().rposition(|a| a.bitrate as f64 * 1000.0 <= e * 0.35).unwrap_or(0)
}

/// The video stream to decode. `fixed`: the height the user picked. Otherwise the
/// smallest height that fills `target_h` (at most 1080p), or lower when `budget`
/// (bit/s for video; `None`: not measured yet) cannot carry it. A taller picture
/// beats a cheaper codec; of two streams of a height, the one cheaper to decode is
/// taken when it fits.
pub fn choose_video(streams: &[VideoStream], fixed: Option<u32>, target_h: u32, budget: Option<f64>) -> usize {
    let wanted = fixed.unwrap_or_else(|| target_h.min(1080));
    let top = streams.iter().map(|s| s.height).find(|&h| h >= wanted).or_else(|| streams.last().map(|s| s.height)).unwrap_or(0);
    let fits = |s: &VideoStream, b: f64| s.bitrate as f64 * 1000.0 <= b;
    let cheapest_first = |h: u32| {
        let mut at: Vec<usize> = (0..streams.len()).filter(|&i| streams[i].height == h).collect();
        at.sort_by_key(|&i| !streams[i].codec.starts_with("avc"));
        at
    };
    if fixed.is_some() {
        let at = cheapest_first(top);
        return match budget {
            Some(b) => at.iter().copied().find(|&i| fits(&streams[i], b)).or_else(|| at.iter().copied().min_by_key(|&i| streams[i].bitrate)),
            None => at.first().copied(),
        }
        .unwrap_or(0);
    }
    let budget = budget.unwrap_or(UNKNOWN_START);
    let mut heights: Vec<u32> = streams.iter().map(|s| s.height).filter(|&h| h <= top).collect();
    heights.dedup();
    for &h in heights.iter().rev() {
        if let Some(i) = cheapest_first(h).into_iter().find(|&i| fits(&streams[i], budget)) {
            return i;
        }
    }
    (0..streams.len()).min_by_key(|&i| streams[i].bitrate).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(height: u32, codec: &str, bitrate: u32) -> VideoStream {
        VideoStream { url: format!("{height}-{codec}"), headers: Vec::new(), width: height * 16 / 9, height, fps: 30.0, codec: codec.into(), segmented: false, bitrate, size: None }
    }

    fn audio(bitrate: u32) -> AudioStream {
        AudioStream { url: bitrate.to_string(), headers: Vec::new(), label: String::new(), bitrate, size: None }
    }

    #[test]
    fn picks_what_fits() {
        let ladder = [
            video(144, "avc1", 77),
            video(240, "vp9", 102),
            video(240, "avc1", 162),
            video(360, "vp9", 226),
            video(360, "avc1", 315),
            video(720, "vp9", 664),
            video(720, "avc1", 993),
            video(1080, "vp9", 1158),
            video(1080, "avc1", 3038),
        ];
        let pick = |fixed, target, budget| ladder[choose_video(&ladder, fixed, target, budget)].url.clone();
        // Plenty of room: the window decides, in the codec cheapest to decode.
        assert_eq!(pick(None, 720, Some(30e6)), "720-avc1");
        assert_eq!(pick(None, 2000, Some(30e6)), "1080-avc1");
        // Tight: the same height in VP9 rather than a smaller picture.
        assert_eq!(pick(None, 1080, Some(2e6)), "1080-vp9");
        assert_eq!(pick(None, 720, Some(700e3)), "720-vp9");
        assert_eq!(pick(None, 720, Some(250e3)), "360-vp9");
        // Nothing fits: the smallest.
        assert_eq!(pick(None, 720, Some(10e3)), "144-avc1");
        // Not measured yet: a modest start.
        assert_eq!(pick(None, 720, None), "360-vp9");
        // The user's height stays, in the codec that fits.
        assert_eq!(pick(Some(1080), 360, Some(30e6)), "1080-avc1");
        assert_eq!(pick(Some(1080), 360, Some(2e6)), "1080-vp9");
        assert_eq!(pick(Some(1080), 360, Some(100e3)), "1080-vp9");
        assert_eq!(pick(Some(1080), 360, None), "1080-avc1");
    }

    #[test]
    fn sound_leaves_room() {
        let ladder = [audio(46), audio(61), audio(129)];
        assert_eq!(choose_audio(&ladder, None), 2);
        assert_eq!(choose_audio(&ladder, Some(100e3)), 0);
        assert_eq!(choose_audio(&ladder, Some(200e3)), 1);
        assert_eq!(choose_audio(&ladder, Some(400e3)), 2);
        assert_eq!(video_budget(Some(1e6), 129, false), Some(671_000.0));
        assert_eq!(video_budget(Some(1e6), 129, true), Some(800_000.0));
    }

    #[test]
    fn sharp_drop_counts_at_once() {
        let meter = Meter::default();
        let at = |rates: &[f64]| {
            let mut m = meter.0.lock().unwrap();
            for _ in 0..10 {
                m.fast.add(1.0, 10e6);
                m.slow.add(1.0, 10e6);
            }
            m.recent = rates.iter().map(|&r| (Instant::now(), 1.0, r)).collect();
        };
        at(&[1e6, 1e6]);
        assert_eq!(meter.estimate_now(), Some(1e6));
        // One slow second, or an ordinary wobble, leaves the average.
        at(&[1e6]);
        assert!((meter.estimate_now().unwrap() - 10e6).abs() < 1.0);
        at(&[4e6, 8e6]);
        assert!((meter.estimate_now().unwrap() - 10e6).abs() < 1.0);
    }

    #[test]
    fn estimate_drops_fast_and_rises_slowly() {
        let mut fast = Ewma::new(2.0);
        let mut slow = Ewma::new(6.0);
        for _ in 0..20 {
            fast.add(1.0, 10e6);
            slow.add(1.0, 10e6);
        }
        assert!((fast.get().min(slow.get()) - 10e6).abs() < 1.0);
        for _ in 0..3 {
            fast.add(1.0, 1e6);
            slow.add(1.0, 1e6);
        }
        let low = fast.get().min(slow.get());
        assert!(low < 5e6, "{low}");
        for _ in 0..3 {
            fast.add(1.0, 10e6);
            slow.add(1.0, 10e6);
        }
        let back = fast.get().min(slow.get());
        assert!(back < 8e6, "{back}");
    }
}
