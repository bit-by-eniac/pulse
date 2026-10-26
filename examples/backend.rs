//! Checks the yt-dlp backend against the live site, the way the app uses it.
//!
//! cargo run --release --example backend -- resolve [browser|off] [video link]
//! cargo run --release --example backend -- update

use pulse::provider::ytdlp::YtDlp;
use pulse::provider::{Input, Provider};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let provider = YtDlp::new(Vec::new());
    match args.next().as_deref() {
        Some("update") => {
            let started = Instant::now();
            match provider.update() {
                Ok(msg) => println!("{msg} ({:.1}s)", started.elapsed().as_secs_f32()),
                Err(e) => println!("{:?}: {}", e.kind, e.message),
            }
            println!("{:?}", provider.health());
        }
        Some("resolve") => resolve(&provider, args.next().filter(|b| b != "off"), args.next()),
        _ => eprintln!("usage: backend resolve [browser|off] [link]  |  backend update"),
    }
}

fn resolve(provider: &YtDlp, browser: Option<String>, link: Option<String>) {
    let link = link.unwrap_or_else(|| "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into());
    println!("browsers found: {:?}", provider.sign_in_sources().iter().map(|s| &s.name).collect::<Vec<_>>());
    provider.set_sign_in(browser);
    let Input::Track(track) = provider.parse_input(&link) else {
        eprintln!("not a video link: {link}");
        std::process::exit(2);
    };
    for round in 1..=2 {
        let started = Instant::now();
        match provider.resolve(&track) {
            Ok(r) => println!(
                "round {round}: OK in {:.1}s  \"{}\"  audio {}  video heights {:?}",
                started.elapsed().as_secs_f32(),
                r.track.title,
                r.audio.iter().map(|a| a.label.as_str()).collect::<Vec<_>>().join(", "),
                r.video.iter().map(|v| v.height).collect::<Vec<_>>()
            ),
            Err(e) => println!("round {round}: {:?} in {:.1}s: {}", e.kind, started.elapsed().as_secs_f32(), e.message),
        }
    }
}
