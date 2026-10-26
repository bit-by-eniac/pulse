#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use pulse::config::Config;
use pulse::provider::ytdlp::YtDlp;
use pulse::provider::Provider;
use std::sync::Arc;

fn main() -> eframe::Result {
    let config = Config::load();
    let provider = Arc::new(YtDlp::new(config.ytdlp_args.clone()));
    provider.set_sign_in(config.sign_in.clone());
    // `pulse <link or search words>` opens that right away.
    let initial = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    pulse::app::run(provider, config, initial)
}
