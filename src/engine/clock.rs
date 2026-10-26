//! The playback clock. mpv reports its position a few times per second; in
//! between, the clock interpolates with wall time so the seek bar and the video
//! frames move smoothly.

use std::sync::Mutex;
use std::time::Instant;

struct Inner {
    pos: f64,
    at: Instant,
    running: bool,
}

pub struct Clock(Mutex<Inner>);

impl Clock {
    pub fn new() -> Self {
        Self(Mutex::new(Inner { pos: 0.0, at: Instant::now(), running: false }))
    }

    pub fn now(&self) -> f64 {
        let c = self.0.lock().unwrap();
        if c.running {
            c.pos + c.at.elapsed().as_secs_f64()
        } else {
            c.pos
        }
    }

    pub fn set(&self, pos: f64) {
        let mut c = self.0.lock().unwrap();
        c.pos = pos;
        c.at = Instant::now();
    }

    pub fn set_running(&self, running: bool) {
        let mut c = self.0.lock().unwrap();
        if c.running == running {
            return;
        }
        if c.running {
            c.pos += c.at.elapsed().as_secs_f64();
        }
        c.at = Instant::now();
        c.running = running;
    }
}
