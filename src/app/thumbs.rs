//! Thumbnail loader: downloads and decodes images on worker threads and keeps
//! a small, bounded set of textures, so memory stays low however long the
//! result list is.

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions, Vec2};
use std::collections::HashMap;
use std::io::Read;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_TEXTURES: usize = 72;
const WORKERS: usize = 3;
/// Textures are stored at this width: enough for a card, small in memory.
const WIDTH: u32 = 256;

enum Slot {
    Loading,
    Failed,
    Ready { tex: TextureHandle, used: u64 },
}

pub struct Thumbs {
    ctx: egui::Context,
    slots: HashMap<String, Slot>,
    requests: Sender<String>,
    done: Receiver<(String, Option<ColorImage>)>,
    frame: u64,
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, request_rx) = channel::<String>();
        let (done_tx, done) = channel();
        let request_rx = Arc::new(Mutex::new(request_rx));
        // One client for all workers: they share its connections and TLS setup.
        let agent = crate::platform::http_agent(Some(Duration::from_secs(12)));
        for _ in 0..WORKERS {
            let (rx, tx, ctx, agent) = (request_rx.clone(), done_tx.clone(), ctx.clone(), agent.clone());
            std::thread::spawn(move || {
                loop {
                    let Ok(url) = rx.lock().unwrap().recv() else { return };
                    let image = fetch(&agent, &url);
                    if tx.send((url, image)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self { ctx: ctx.clone(), slots: HashMap::new(), requests, done, frame: 0 }
    }

    /// Call once per frame: turns finished downloads into textures and evicts old ones.
    pub fn poll(&mut self) {
        self.frame += 1;
        while let Ok((url, image)) = self.done.try_recv() {
            let slot = match image {
                Some(img) => Slot::Ready {
                    tex: self.ctx.load_texture(&url, img, TextureOptions::LINEAR),
                    used: self.frame,
                },
                None => Slot::Failed,
            };
            self.slots.insert(url, slot);
        }
        let ready = self.slots.values().filter(|s| matches!(s, Slot::Ready { .. })).count();
        if ready > MAX_TEXTURES {
            let mut ages: Vec<(u64, String)> = self
                .slots
                .iter()
                .filter_map(|(k, s)| if let Slot::Ready { used, .. } = s { Some((*used, k.clone())) } else { None })
                .collect();
            ages.sort();
            for (_, key) in ages.into_iter().take(ready - MAX_TEXTURES + 8) {
                self.slots.remove(&key);
            }
        }
        // Failed downloads are remembered so they are not retried every frame, but
        // not forever: a long session would otherwise collect every broken URL.
        if self.slots.len() > MAX_TEXTURES * 4 {
            self.slots.retain(|_, s| !matches!(s, Slot::Failed));
        }
    }

    /// The texture for `url`, starting the download the first time it is asked for.
    pub fn get(&mut self, url: &str) -> Option<(TextureId, Vec2)> {
        match self.slots.get_mut(url) {
            Some(Slot::Ready { tex, used }) => {
                *used = self.frame;
                Some((tex.id(), tex.size_vec2()))
            }
            Some(_) => None,
            None => {
                self.slots.insert(url.to_string(), Slot::Loading);
                let _ = self.requests.send(url.to_string());
                None
            }
        }
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Option<ColorImage> {
    let mut bytes = Vec::new();
    agent
        .get(url)
        .call()
        .ok()?
        .into_body()
        .into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    let height = (img.height() as f32 * WIDTH as f32 / img.width() as f32).max(1.0) as u32;
    let img = img.resize_exact(WIDTH, height, image::imageops::FilterType::Triangle).to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}
