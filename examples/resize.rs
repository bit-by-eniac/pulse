//! Measures how long the picture stalls when the window is resized (e.g. in and out
//! of fullscreen). `cargo run --release --example resize -- [video link]`
use pulse::config::{Config, Mode};
use pulse::engine::{self, Cmd, Status};
use pulse::provider::{ytdlp::YtDlp, Input, Provider};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let link = std::env::args().nth(1).unwrap_or_else(|| "https://youtu.be/dQw4w9WgXcQ".into());
    let provider: Arc<dyn Provider> = Arc::new(YtDlp::new(vec![]));
    let Input::Track(track) = provider.parse_input(&link) else { panic!("not a video link") };
    let cfg = Config { mpv_args: vec!["--ao=null".into()], mode: Mode::Video, ..Default::default() };
    let eng = engine::start(provider, &cfg, Arc::new(|| {}));
    eng.send(Cmd::PlayQueue { tracks: vec![track], index: 0 });
    eng.send(Cmd::VideoTarget(Some((800, 450))));
    let t = Instant::now();
    while eng.frames().take().is_none() {
        assert!(t.elapsed() < Duration::from_secs(60), "no video");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(eng.state().status, Status::Playing);
    println!("first frame after {:.1}s", t.elapsed().as_secs_f32());

    let mut worst = 0.0f32;
    // Plain jumps, then the many small steps of a fullscreen animation.
    let jumps = [(800, 450), (1920, 1080), (800, 450), (1920, 1080), (800, 450)].map(|t| (t, false));
    for ((w, h), animate) in jumps.into_iter().chain([((1920, 1080), true), ((800, 450), true)]) {
        if animate {
            let from = eng.frames().take().map(|f| (f.width as f32, f.height as f32)).unwrap_or((800.0, 450.0));
            for i in 1..=12 {
                let k = i as f32 / 12.0;
                let step = (from.0 + (w as f32 - from.0) * k, from.1 + (h as f32 - from.1) * k);
                eng.send(Cmd::VideoTarget(Some((step.0 as u32, step.1 as u32))));
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        eng.send(Cmd::VideoTarget(Some((w, h))));
        let (start, mut last, mut gap, mut n, mut size) = (Instant::now(), Instant::now(), 0.0f32, 0, (0, 0));
        while start.elapsed() < Duration::from_secs(6) {
            if let Some(f) = eng.frames().take() {
                gap = gap.max(last.elapsed().as_secs_f32());
                last = Instant::now();
                n += 1;
                size = (f.width, f.height);
                eng.frames().recycle(f.pixels);
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        gap = gap.max(last.elapsed().as_secs_f32());
        worst = worst.max(gap);
        println!(
            "target {w}x{h}{}: {n} frames in 6s, now {}x{} @{:?}p, longest stall {:.0} ms",
            if animate { " (animated)" } else { "" },
            size.0,
            size.1,
            eng.state().active_height,
            gap * 1000.0
        );
    }
    // Paused: the picture must still come back at the new size.
    eng.send(Cmd::TogglePause);
    std::thread::sleep(Duration::from_millis(500));
    eng.send(Cmd::VideoTarget(Some((1920, 1080))));
    let t = Instant::now();
    loop {
        if let Some(f) = eng.frames().take() {
            if f.width == 1920 {
                break;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(15), "no frame at the new size while paused");
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("paused resize: new frame after {:.1}s", t.elapsed().as_secs_f32());
    eng.send(Cmd::Shutdown);
    std::thread::sleep(Duration::from_millis(500));
    println!("worst stall {:.0} ms", worst * 1000.0);
}
