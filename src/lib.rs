//! Pulse: a tiny YouTube audio/video player.
//!
//! - `provider`: the only code that knows about YouTube (behind a trait)
//! - `engine`: queue and playback (mpv for audio, ffmpeg for video frames)
//! - `platform`: everything that differs between operating systems
//! - `app`: the egui user interface

pub mod app;
pub mod config;
pub mod engine;
pub mod platform;
pub mod provider;
