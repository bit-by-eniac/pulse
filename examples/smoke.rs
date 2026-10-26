//! Headless end-to-end check of provider + engine against the real tools.
//! `cargo run --example smoke`
use pulse::config::{Config, Mode};
use pulse::engine::{self, Cmd, Status};
use pulse::provider::{ytdlp::YtDlp, Input, Provider, SearchItem, SearchKind};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("  ok: {what} ({:.1}s)", t.elapsed().as_secs_f32());
}

fn main() {
    let provider: Arc<dyn Provider> = Arc::new(YtDlp::new(vec![]));
    println!("health: {:?}", provider.health());

    println!("search videos");
    let vids = provider.search("lofi hip hop radio", SearchKind::Video, 5).unwrap();
    assert!(!vids.is_empty());
    for v in &vids {
        if let SearchItem::Track(t) = v {
            println!("  {} | {:?} | {:?}s | {:?} views", t.title, t.channel, t.duration, t.views);
        }
    }
    println!("search playlists");
    let pls = provider.search("lofi", SearchKind::Playlist, 3).unwrap();
    println!("  {} playlists, first: {:?}", pls.len(), pls.first());
    println!("search channels");
    let chs = provider.search("lofi girl", SearchKind::Channel, 3).unwrap();
    println!("  {} channels, first: {:?}", chs.len(), chs.first());
    if let Some(SearchItem::Container(c)) = pls.first() {
        let l = provider.open(&c.url, 20).unwrap();
        println!("open playlist: '{}' by {:?}: {} tracks", l.title, l.owner, l.tracks.len());
    }
    if let Some(SearchItem::Container(c)) = chs.first() {
        let l = provider.open(&c.url, 10).unwrap();
        println!("open channel: '{}' {} tracks", l.title, l.tracks.len());
    }

    let Input::Track(t1) = provider.parse_input("https://youtu.be/dQw4w9WgXcQ") else { panic!() };
    let r = provider.resolve(&t1).unwrap();
    println!("resolved: {} | audio {} | heights {:?}", r.track.title, r.audio.iter().map(|a| a.label.as_str()).collect::<Vec<_>>().join(", "), r.video.iter().map(|v| (v.height, &v.codec)).collect::<Vec<_>>());

    let cfg = Config { mpv_args: vec!["--ao=null".into()], mode: Mode::Audio, ..Default::default() };
    let eng = engine::start(provider.clone(), &cfg, Arc::new(|| {}));
    let t2 = match provider.parse_input("https://www.youtube.com/watch?v=jNQXAC9IVRw") { Input::Track(t) => t, _ => panic!() };
    println!("engine: audio");
    eng.send(Cmd::PlayQueue { tracks: vec![t1.clone(), t2.clone()], index: 0 });
    wait("playing", 30, || eng.state().status == Status::Playing);
    wait("position advances", 10, || eng.position() > 2.0);
    eng.send(Cmd::TogglePause);
    wait("paused", 5, || eng.state().status == Status::Paused);
    let p = eng.position();
    std::thread::sleep(Duration::from_millis(800));
    assert!((eng.position() - p).abs() < 0.2, "clock must stop while paused");
    eng.send(Cmd::TogglePause);
    wait("resumed", 5, || eng.state().status == Status::Playing);
    eng.send(Cmd::SeekTo(100.0));
    wait("seek", 10, || eng.position() > 100.0 && eng.position() < 105.0);
    let s = eng.state();
    println!("  state: dur={:?} heights={:?} audio={}", s.duration, s.heights, s.audio_label);

    println!("engine: video");
    eng.send(Cmd::SetMode(Mode::Video));
    eng.send(Cmd::VideoTarget(Some((640, 360))));
    wait("first video frame", 30, || eng.frames().take().is_some());
    let mut n = 0;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        if let Some(f) = eng.frames().take() {
            n += 1;
            if n == 1 { println!("  frame {}x{}", f.width, f.height); }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("  {n} frames in 3s (expect ~90 at 30fps)");
    eng.send(Cmd::SetQuality(Some(144)));
    wait("quality switch to 144p", 30, || eng.state().active_height == Some(144));
    wait("frame after switch", 30, || eng.frames().take().is_some());

    println!("engine: next track");
    eng.send(Cmd::Next);
    wait("second track loaded", 30, || eng.state().index == Some(1) && eng.state().status == Status::Playing);
    println!("  title: {:?}", eng.state().current().map(|t| t.title.clone()));
    eng.send(Cmd::Shutdown);
    std::thread::sleep(Duration::from_millis(500));
    println!("ALL OK");
}
