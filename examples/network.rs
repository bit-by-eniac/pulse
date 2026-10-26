//! Plays a track through a slow, changing network and measures what the listener
//! and viewer get: start-up time, audio stalls, frozen picture, video quality.
//!
//! cargo run --release --example network -- <schedule> [--audio] [--link URL] [--size WxH] [--out DIR] [--rtt MS] [--seek T=POS] [--pause T=SECS] [--quality H] [--speakers] [--mpv-arg ARG]
//!
//! `<schedule>` is link speeds with how long each lasts, e.g. `100k@90` or
//! `10M@40,500k@40,10M@40` (bits per second; `0` is an outage). The last speed
//! holds until the schedule's total time is up. Traffic goes through
//! `scripts/netsim.py` (needs python3): yt-dlp, mpv and ffmpeg all share the link.
//! `--seek 40=300` jumps to 5:00 at 40 s into the run, `--pause 40=20` pauses for
//! 20 s there; the summary says how long sound and picture took to come back.
//! `--quality 1080` fixes the video height (default Auto). `--speakers` plays
//! through the sound device with the OS media controls on, as the app does, but
//! at volume 0. `--mpv-arg` adds an mpv option (repeatable).
use pulse::config::{Config, Mode};
use pulse::engine::{self, Cmd, Status};
use pulse::provider::{ytdlp::YtDlp, Input, Provider};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn parse_rate(s: &str) -> f64 {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1e3),
        Some('M' | 'm') => (&s[..s.len() - 1], 1e6),
        _ => (s, 1.0),
    };
    num.parse::<f64>().expect("bad rate") * mult
}

fn fmt_rate(bps: f64) -> String {
    if bps >= 1e6 {
        format!("{:.1}M", bps / 1e6)
    } else {
        format!("{:.0}k", bps / 1e3)
    }
}

struct Args {
    schedule: Vec<(f64, f64)>,
    audio: bool,
    link: String,
    size: (u32, u32),
    out: std::path::PathBuf,
    rtt: f64,
    /// (at, action): seek to a position, or pause for a while.
    events: Vec<(f64, Event)>,
    quality: Option<u32>,
    speakers: bool,
    mpv_args: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum Event {
    Seek(f64),
    Pause(f64),
}

fn event(spec: &str, make: fn(f64) -> Event) -> (f64, Event) {
    let (at, value) = spec.split_once('=').expect("T=VALUE");
    (at.parse().expect("T"), make(value.parse().expect("VALUE")))
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let schedule = it.next().expect("usage: network <schedule> [--audio] [--link URL] [--size WxH] [--out DIR] [--rtt MS] [--seek T=POS] [--pause T=SECS] [--quality H] [--speakers] [--mpv-arg ARG]");
    let schedule = schedule
        .split(',')
        .map(|part| {
            let (rate, secs) = part.split_once('@').expect("rate@secs");
            (parse_rate(rate), secs.parse::<f64>().expect("secs"))
        })
        .collect();
    let mut a = Args {
        schedule,
        audio: false,
        link: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
        size: (1280, 720),
        out: std::env::temp_dir().join("pulse-network"),
        rtt: 60.0,
        events: Vec::new(),
        quality: None,
        speakers: false,
        mpv_args: Vec::new(),
    };
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--audio" => a.audio = true,
            "--link" => a.link = it.next().unwrap(),
            "--size" => {
                let s = it.next().unwrap();
                let (w, h) = s.split_once('x').unwrap();
                a.size = (w.parse().unwrap(), h.parse().unwrap());
            }
            "--out" => a.out = it.next().unwrap().into(),
            "--rtt" => a.rtt = it.next().unwrap().parse().unwrap(),
            "--seek" => a.events.push(event(&it.next().unwrap(), Event::Seek)),
            "--pause" => a.events.push(event(&it.next().unwrap(), Event::Pause)),
            "--quality" => a.quality = Some(it.next().unwrap().parse().unwrap()),
            "--speakers" => a.speakers = true,
            "--mpv-arg" => a.mpv_args.push(it.next().unwrap()),
            other => panic!("unknown option {other}"),
        }
    }
    a
}

/// Link speed at `t` seconds into the schedule.
fn rate_at(schedule: &[(f64, f64)], t: f64) -> f64 {
    let mut end = 0.0;
    for &(rate, secs) in schedule {
        end += secs;
        if t < end {
            return rate;
        }
    }
    schedule.last().map(|s| s.0).unwrap_or(0.0)
}

struct Proxy(std::process::Child);
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() {
    let a = args();
    std::fs::create_dir_all(&a.out).unwrap();
    let total: f64 = a.schedule.iter().map(|s| s.1).sum();
    let rate_file = a.out.join("rate.txt");
    std::fs::write(&rate_file, format!("{}", a.schedule[0].0)).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/netsim.py");
    let _proxy = Proxy(
        Command::new("python3")
            .arg(&script)
            .args(["--port", &port.to_string(), "--rtt-ms", &a.rtt.to_string(), "--rate-file"])
            .arg(&rate_file)
            .arg("--log")
            .arg(a.out.join("link.csv"))
            .arg("--conn-log")
            .arg(a.out.join("conns.csv"))
            .stdout(Stdio::null())
            .spawn()
            .expect("python3"),
    );
    let proxy = format!("http://127.0.0.1:{port}");
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // mpv and ffmpeg (and ureq) pick the proxy up from the environment.
    for key in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
        std::env::set_var(key, &proxy);
    }
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");

    let provider: Arc<dyn Provider> = Arc::new(YtDlp::new(vec!["--proxy".into(), proxy.clone()]));
    let Input::Track(track) = provider.parse_input(&a.link) else { panic!("not a video link") };
    let mut mpv_args: Vec<String> = if a.speakers { vec!["--volume=0".into()] } else { vec!["--ao=null".into(), "--media-controls=no".into(), "--input-media-keys=no".into()] };
    mpv_args.extend(a.mpv_args.iter().cloned());
    let cfg = Config {
        mpv_args,
        mode: if a.audio { Mode::Audio } else { Mode::Video },
        quality: a.quality,
        ..Default::default()
    };
    let eng = engine::start(provider, &cfg, Arc::new(|| {}));
    let started = Instant::now();
    eng.send(Cmd::PlayQueue { tracks: vec![track], index: 0 });
    eng.send(Cmd::VideoTarget(Some(a.size)));

    let mut timeline = std::fs::File::create(a.out.join("timeline.csv")).unwrap();
    writeln!(timeline, "t,limit_bps,status,pos,height,frames,max_gap_ms,estimate_bps,audio_kbps,audio_ahead,video_ahead,video_behind,held").unwrap();
    let schedule_desc: Vec<String> = a.schedule.iter().map(|(r, s)| format!("{}@{s}", fmt_rate(*r))).collect();
    println!("schedule {} ({}s), {} {}x{}", schedule_desc.join(","), total, if a.audio { "audio" } else { "video" }, a.size.0, a.size.1);

    let mut limit = a.schedule[0].0;
    let mut audio_start: Option<f64> = None;
    let mut video_start: Option<f64> = None;
    let (mut stalls, mut stall_time, mut in_stall) = (0u32, 0.0f64, false);
    let mut last_frame: Option<Instant> = None;
    let (mut frames, mut frames_sec, mut gap_sec) = (0u64, 0u64, 0.0f64);
    let (mut frozen, mut longest_gap) = (0.0f64, 0.0f64);
    let mut heights: Vec<(u32, f64)> = Vec::new();
    let mut switches = 0u32;
    let mut last_height: Option<u32> = None;
    let mut playing_time = 0.0f64;
    let mut next_report = 1.0;
    let mut last_tick = Instant::now();
    let mut last_size = (0, 0);
    let mut notices: Vec<String> = Vec::new();
    let mut audio_rates: Vec<(u32, f64)> = Vec::new();
    let mut events = a.events.clone();
    events.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut events = std::collections::VecDeque::from(events);
    // After an event: when it happened, and when sound and picture were back.
    let mut recover: Vec<(String, f64, Option<f64>, Option<f64>)> = Vec::new();
    let mut resume_at: Option<f64> = None;

    loop {
        let t = started.elapsed().as_secs_f64();
        if t >= total {
            break;
        }
        if events.front().is_some_and(|e| t >= e.0) {
            let (at, ev) = events.pop_front().unwrap();
            match ev {
                Event::Seek(pos) => {
                    eng.send(Cmd::SeekTo(pos));
                    recover.push((format!("seek@{at:.0}"), t, None, None));
                    // The picture counts as frozen only from when the sound is back.
                    last_frame = None;
                }
                Event::Pause(secs) => {
                    eng.send(Cmd::TogglePause);
                    resume_at = Some(t + secs);
                }
            }
        }
        if resume_at.is_some_and(|r| t >= r) {
            resume_at = None;
            eng.send(Cmd::TogglePause);
            recover.push((format!("resume@{t:.0}"), t, None, None));
        }
        let want = rate_at(&a.schedule, t);
        if want != limit {
            limit = want;
            std::fs::write(&rate_file, format!("{limit}")).unwrap();
        }
        let dt = last_tick.elapsed().as_secs_f64();
        last_tick = Instant::now();
        let s = eng.state();
        if let Some(n) = &s.notice {
            if notices.last() != Some(&n.message) {
                println!("  notice at {t:.1}s: {}", n.message);
                notices.push(n.message.clone());
            }
        }
        if let Some(r) = recover.last_mut() {
            if r.2.is_none() && s.status == Status::Playing && t > r.1 + 0.3 {
                r.2 = Some(t - r.1);
            }
        }
        match s.status {
            Status::Playing => {
                audio_start.get_or_insert(t);
                in_stall = false;
                playing_time += dt;
            }
            // Waiting for the sound after a seek or pause is counted apart.
            Status::Buffering if audio_start.is_some() && recover.last().is_none_or(|r| r.2.is_some()) => {
                if !in_stall {
                    stalls += 1;
                    in_stall = true;
                }
                stall_time += dt;
            }
            _ => {}
        }
        if let Some(h) = s.active_height {
            if last_height != Some(h) {
                if last_height.is_some() {
                    switches += 1;
                }
                last_height = Some(h);
            }
            match heights.iter_mut().find(|(x, _)| *x == h) {
                Some(e) => e.1 += dt,
                None => heights.push((h, dt)),
            }
        }
        if s.net.audio_kbps > 0 && s.status == Status::Playing {
            match audio_rates.iter_mut().find(|(k, _)| *k == s.net.audio_kbps) {
                Some(e) => e.1 += dt,
                None => audio_rates.push((s.net.audio_kbps, dt)),
            }
        }
        if let Some(f) = eng.frames().take() {
            let now = Instant::now();
            video_start.get_or_insert(t);
            if let Some(r) = recover.last_mut() {
                if r.3.is_none() && t > r.1 + 0.3 {
                    r.3 = Some(t - r.1);
                }
            }
            if let Some(prev) = last_frame {
                let gap = now.duration_since(prev).as_secs_f64();
                gap_sec = gap_sec.max(gap);
                longest_gap = longest_gap.max(gap);
                if gap > 0.25 {
                    frozen += gap;
                }
            }
            last_frame = Some(now);
            frames += 1;
            frames_sec += 1;
            last_size = (f.width, f.height);
            eng.frames().recycle(f.pixels);
        } else if s.status != Status::Playing || recover.last().is_some_and(|r| r.3.is_none()) {
            // The picture is meant to stand still while the sound does, and is
            // counted apart after a seek or pause.
            if last_frame.is_some() {
                last_frame = Some(Instant::now());
            }
        }
        if t >= next_report {
            let pos = eng.position();
            let n = &s.net;
            let secs = |v: Option<f64>| v.map(|v| format!("{v:.0}s")).unwrap_or("all".into());
            println!(
                "{t:5.0}s link {:>6} {:?} pos {pos:6.1} video {:>5} {}x{} {frames_sec:2} fps, max gap {:4.0} ms | est {:>6} audio {}k ahead a {} v {} behind {}{}",
                fmt_rate(limit),
                s.status,
                s.active_height.map(|h| format!("{h}p")).unwrap_or("-".into()),
                last_size.0,
                last_size.1,
                gap_sec * 1000.0,
                n.estimate.map(fmt_rate).unwrap_or("-".into()),
                n.audio_kbps,
                secs(n.audio_ahead),
                secs(n.video_ahead),
                n.video_behind.map(|b| format!("{b:.1}s")).unwrap_or("-".into()),
                if n.video_held { " HELD" } else { "" }
            );
            writeln!(
                timeline,
                "{t:.1},{limit},{:?},{pos:.2},{},{frames_sec},{:.0},{:.0},{},{},{},{},{}",
                s.status,
                s.active_height.unwrap_or(0),
                gap_sec * 1000.0,
                n.estimate.unwrap_or(0.0),
                n.audio_kbps,
                n.audio_ahead.map(|v| format!("{v:.1}")).unwrap_or_default(),
                n.video_ahead.map(|v| format!("{v:.1}")).unwrap_or_default(),
                n.video_behind.map(|v| format!("{v:.2}")).unwrap_or_default(),
                n.video_held as u8
            )
            .unwrap();
            frames_sec = 0;
            gap_sec = 0.0;
            next_report += 1.0;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    // A picture that froze and never came back counts too.
    if let Some(prev) = last_frame {
        let gap = prev.elapsed().as_secs_f64();
        if gap > 0.25 && eng.state().status == Status::Playing {
            frozen += gap;
            longest_gap = longest_gap.max(gap);
        }
    }
    let pos = eng.position();
    eng.send(Cmd::Shutdown);
    std::thread::sleep(Duration::from_millis(600));

    let watched = video_start.map(|v| total - v).unwrap_or(0.0);
    heights.sort_by_key(|h| h.0);
    let quality: Vec<String> = heights.iter().map(|(h, secs)| format!("{h}p:{:.0}s", secs)).collect();
    audio_rates.sort_by_key(|a| a.0);
    let audio: Vec<String> = audio_rates.iter().map(|(k, secs)| format!("{k}k:{:.0}s", secs)).collect();
    for (what, _, sound, picture) in &recover {
        let secs = |v: &Option<f64>| v.map(|v| format!("{v:.1}s")).unwrap_or("never".into());
        println!("RECOVER {what} sound={} picture={}", secs(sound), secs(picture));
    }
    println!(
        "SUMMARY audio_start={} stalls={stalls} stall_time={stall_time:.1}s playing={playing_time:.0}s pos={pos:.0}s video_start={} fps={:.1} frozen={frozen:.1}s longest_gap={:.1}s switches={switches} quality=[{}] audio=[{}] notices={}",
        audio_start.map(|t| format!("{t:.1}s")).unwrap_or("never".into()),
        video_start.map(|t| format!("{t:.1}s")).unwrap_or("never".into()),
        if watched > 0.0 { frames as f64 / watched } else { 0.0 },
        longest_gap,
        quality.join(" "),
        audio.join(" "),
        notices.len()
    );
}
