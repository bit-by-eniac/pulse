//! Hairline vector icons drawn with the painter, so no icon font or image
//! files are needed and everything stays sharp at any scale.

use eframe::egui::{epaint::PathShape, pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};
use std::f32::consts::PI;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    VolumeLow,
    Mute,
    Search,
    Video,
    Headphones,
    Fullscreen,
    ExitFullscreen,
    Queue,
    QueueAdd,
    Share,
    Close,
    Back,
    Gear,
    Download,
    Playlist,
    ChevronDown,
    Check,
}

/// Draws `icon` centred in a `size`-sized square.
pub fn paint(p: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    let u = size / 2.0; // half extent; shapes are laid out in [-1, 1]
    let at = |x: f32, y: f32| pos2(center.x + x * u, center.y + y * u);
    let stroke = Stroke::new((size / 11.0).clamp(1.2, 2.0), color);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::Path(PathShape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), stroke)));
    };
    let poly = |pts: &[(f32, f32)]| {
        p.add(Shape::convex_polygon(pts.iter().map(|&(x, y)| at(x, y)).collect(), color, Stroke::NONE));
    };
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| {
        let n = 18;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let a = from + (to - from) * i as f32 / n as f32;
                at(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        p.add(Shape::Path(PathShape::line(pts, stroke)));
    };
    let rect_stroke = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_stroke(Rect::from_two_pos(at(x0, y0), at(x1, y1)), r * u, stroke, StrokeKind::Middle);
    };
    let rect_fill = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_filled(Rect::from_two_pos(at(x0, y0), at(x1, y1)), r * u, color);
    };

    match icon {
        Icon::Play => poly(&[(-0.42, -0.62), (0.66, 0.0), (-0.42, 0.62)]),
        Icon::Pause => {
            rect_fill(-0.5, -0.58, -0.15, 0.58, 0.08);
            rect_fill(0.15, -0.58, 0.5, 0.58, 0.08);
        }
        Icon::Prev => {
            rect_fill(-0.62, -0.55, -0.42, 0.55, 0.05);
            poly(&[(0.62, -0.58), (-0.28, 0.0), (0.62, 0.58)]);
        }
        Icon::Next => {
            rect_fill(0.42, -0.55, 0.62, 0.55, 0.05);
            poly(&[(-0.62, -0.58), (0.28, 0.0), (-0.62, 0.58)]);
        }
        Icon::Shuffle => {
            let curve = |ys: f32, ye: f32| {
                let pts: Vec<(f32, f32)> = (0..=16)
                    .map(|i| {
                        let t = i as f32 / 16.0;
                        let s = t * t * (3.0 - 2.0 * t);
                        (-0.75 + 1.3 * t, ys + (ye - ys) * s)
                    })
                    .collect();
                line(&pts);
            };
            curve(-0.42, 0.42);
            curve(0.42, -0.42);
            poly(&[(0.52, 0.2), (0.82, 0.42), (0.52, 0.64)]);
            poly(&[(0.52, -0.64), (0.82, -0.42), (0.52, -0.2)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            line(&[(-0.78, 0.1), (-0.78, -0.32), (-0.66, -0.44), (0.35, -0.44)]);
            poly(&[(0.3, -0.7), (0.62, -0.44), (0.3, -0.18)]);
            line(&[(0.78, -0.1), (0.78, 0.32), (0.66, 0.44), (-0.35, 0.44)]);
            poly(&[(-0.3, 0.7), (-0.62, 0.44), (-0.3, 0.18)]);
            if icon == Icon::RepeatOne {
                line(&[(-0.12, -0.12), (0.06, -0.24), (0.06, 0.24)]);
            }
        }
        Icon::Volume | Icon::VolumeLow | Icon::Mute => {
            p.add(Shape::convex_polygon(
                [(-0.85, -0.25), (-0.55, -0.25), (-0.1, -0.62), (-0.1, 0.62), (-0.55, 0.25), (-0.85, 0.25)]
                    .iter()
                    .map(|&(x, y)| at(x, y))
                    .collect(),
                color,
                Stroke::NONE,
            ));
            match icon {
                Icon::Mute => {
                    line(&[(0.15, -0.3), (0.75, 0.3)]);
                    line(&[(0.15, 0.3), (0.75, -0.3)]);
                }
                Icon::VolumeLow => arc(0.0, 0.0, 0.42, -PI / 3.5, PI / 3.5),
                _ => {
                    arc(0.0, 0.0, 0.42, -PI / 3.5, PI / 3.5);
                    arc(0.0, 0.0, 0.78, -PI / 3.5, PI / 3.5);
                }
            }
        }
        Icon::Search => {
            p.circle_stroke(at(-0.15, -0.15), 0.55 * u, stroke);
            line(&[(0.27, 0.27), (0.78, 0.78)]);
        }
        Icon::Video => {
            rect_stroke(-0.85, -0.5, 0.3, 0.5, 0.18);
            line(&[(0.3, -0.12), (0.8, -0.45), (0.8, 0.45), (0.3, 0.12)]);
        }
        Icon::Headphones => {
            arc(0.0, 0.1, 0.68, PI, 2.0 * PI);
            line(&[(-0.68, 0.1), (-0.68, 0.2)]);
            line(&[(0.68, 0.1), (0.68, 0.2)]);
            rect_stroke(-0.88, 0.12, -0.48, 0.7, 0.16);
            rect_stroke(0.48, 0.12, 0.88, 0.7, 0.16);
        }
        Icon::Fullscreen => {
            line(&[(-0.8, -0.35), (-0.8, -0.8), (-0.35, -0.8)]);
            line(&[(0.35, -0.8), (0.8, -0.8), (0.8, -0.35)]);
            line(&[(0.8, 0.35), (0.8, 0.8), (0.35, 0.8)]);
            line(&[(-0.35, 0.8), (-0.8, 0.8), (-0.8, 0.35)]);
        }
        Icon::ExitFullscreen => {
            line(&[(-0.8, -0.3), (-0.3, -0.3), (-0.3, -0.8)]);
            line(&[(0.3, -0.8), (0.3, -0.3), (0.8, -0.3)]);
            line(&[(0.8, 0.3), (0.3, 0.3), (0.3, 0.8)]);
            line(&[(-0.3, 0.8), (-0.3, 0.3), (-0.8, 0.3)]);
        }
        Icon::Queue => {
            for y in [-0.55, 0.0, 0.55] {
                line(&[(-0.25, y), (0.85, y)]);
            }
            for y in [-0.55, 0.0, 0.55] {
                p.circle_filled(at(-0.75, y), 0.07 * u + 0.5, color);
            }
        }
        Icon::QueueAdd => {
            line(&[(-0.85, -0.55), (0.45, -0.55)]);
            line(&[(-0.85, -0.05), (0.45, -0.05)]);
            line(&[(-0.85, 0.45), (-0.05, 0.45)]);
            line(&[(0.5, 0.07), (0.5, 0.83)]);
            line(&[(0.12, 0.45), (0.88, 0.45)]);
        }
        Icon::Share => {
            line(&[(0.0, 0.3), (0.0, -0.82)]);
            line(&[(-0.36, -0.46), (0.0, -0.82), (0.36, -0.46)]);
            line(&[(-0.32, -0.2), (-0.72, -0.2), (-0.72, 0.82), (0.72, 0.82), (0.72, -0.2), (0.32, -0.2)]);
        }
        Icon::Close => {
            line(&[(-0.6, -0.6), (0.6, 0.6)]);
            line(&[(-0.6, 0.6), (0.6, -0.6)]);
        }
        Icon::Back => {
            line(&[(0.1, -0.7), (-0.6, 0.0), (0.1, 0.7)]);
            line(&[(-0.6, 0.0), (0.8, 0.0)]);
        }
        Icon::Gear => {
            p.circle_stroke(at(0.0, 0.0), 0.3 * u, stroke);
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                let (c, s) = (a.cos(), a.sin());
                line(&[(0.55 * c, 0.55 * s), (0.85 * c, 0.85 * s)]);
            }
            p.circle_stroke(at(0.0, 0.0), 0.58 * u, stroke);
        }
        Icon::Download => {
            line(&[(0.0, -0.8), (0.0, 0.35)]);
            line(&[(-0.45, -0.1), (0.0, 0.4), (0.45, -0.1)]);
            line(&[(-0.75, 0.8), (0.75, 0.8)]);
        }
        Icon::Playlist => {
            for y in [-0.6, -0.15] {
                line(&[(-0.8, y), (0.8, y)]);
            }
            line(&[(-0.8, 0.3), (-0.1, 0.3)]);
            poly(&[(0.2, 0.0), (0.85, 0.45), (0.2, 0.9)]);
        }
        Icon::ChevronDown => line(&[(-0.5, -0.2), (0.0, 0.3), (0.5, -0.2)]),
        Icon::Check => line(&[(-0.65, 0.05), (-0.2, 0.5), (0.7, -0.5)]),
    }
}

/// The app mark: a white disc with a dark play triangle.
pub fn logo(p: &Painter, center: Pos2, size: f32, bg: Color32) {
    p.circle_filled(center, size / 2.0, Color32::from_rgb(0xf4, 0xf4, 0xf6));
    let s = size / 2.0;
    p.add(Shape::convex_polygon(
        vec![
            center + vec2(-0.22 * s, -0.42 * s),
            center + vec2(0.46 * s, 0.0),
            center + vec2(-0.22 * s, 0.42 * s),
        ],
        bg,
        Stroke::NONE,
    ));
}
