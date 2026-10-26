//! Vector icons drawn with the painter (no icon-font dependency, crisp at
//! any size) and the ghost-style icon button built on them.

use std::f32::consts::TAU;

use egui::epaint::CubicBezierShape;
use egui::{
    Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2, pos2, vec2,
};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Pull,
    Push,
    Fetch,
    Branch,
    Stash,
    Pop,
    Search,
    Gear,
    Plus,
    Minus,
    Close,
    ChevronDown,
    ChevronUp,
    Undo,
    Warning,
    Info,
    Check,
    Error,
    /// Activity log: lines with leading dots.
    List,
    /// A short diagonal arrow out of a box: open externally.
    External,
    /// Play triangle in a circle: workflow runs / GitHub Actions.
    Play,
    /// Two overlapping sheets: copy.
    Copy,
    /// A clock face.
    Clock,
    /// Pull request: two branches joined by an arrow.
    PullRequest,
    /// Stop square in a circle: cancel.
    Stop,
    /// Circular arrow: re-run.
    Rerun,
    /// Shield with a check: exposure / security.
    Shield,
    /// Three bars of different heights: insights / analytics.
    Chart,
    /// Commit nodes on a line with a branch: the history page.
    Graph,
    /// A house: the home page.
    Home,
    /// A rocket: releases and deployments (Ship).
    Rocket,
    /// A padlock: governance, protection, secrets.
    Lock,
    /// A terminal window with a prompt: the workbench and console.
    Terminal,
    /// A circle with a dot: an issue.
    Issue,
    /// A tray: the inbox.
    Inbox,
    /// A cube: a package or container image.
    Package,
    /// A cloud: environments, clusters.
    Cloud,
    /// A price tag: tags and releases.
    Tag,
    /// A bug: bisect.
    Bug,
    /// A folder: worktrees, directories.
    Folder,
    /// Two lines joining: merge.
    Merge,
    /// A speech bubble: comments and reviews.
    Comment,
    /// A key: secrets and variables.
    Key,
    /// A gauge: metrics.
    Gauge,
    /// An eye: watch, reveal.
    Eye,
    /// A pencil: edit.
    Edit,
    /// A bin: delete.
    Trash,
    /// A bell: notifications.
    Bell,
    /// Two chain links: a link.
    Link,
    /// A ship's wheel: Kubernetes / Helm.
    Wheel,
    /// An arrow down onto a line: download.
    Download,
    /// A five-pointed star: favourites.
    Star,
}

/// Draw `icon` centred on `center` inside a `size`×`size` box.
pub fn paint(p: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let c = center;
    let st = Stroke::new((size / 9.0).clamp(1.4, 2.0), color);
    // Coordinates in [-0.5, 0.5] relative to the box.
    let pt = |x: f32, y: f32| pos2(c.x + x * s, c.y + y * s);

    match icon {
        Icon::Pull => {
            p.line_segment([pt(0.0, -0.46), pt(0.0, 0.14)], st);
            p.line(vec![pt(-0.22, -0.08), pt(0.0, 0.14), pt(0.22, -0.08)], st);
            p.line(
                vec![pt(-0.4, 0.2), pt(-0.4, 0.44), pt(0.4, 0.44), pt(0.4, 0.2)],
                st,
            );
        }
        Icon::Push => {
            p.line_segment([pt(0.0, 0.14), pt(0.0, -0.46)], st);
            p.line(vec![pt(-0.22, -0.24), pt(0.0, -0.46), pt(0.22, -0.24)], st);
            p.line(
                vec![pt(-0.4, 0.2), pt(-0.4, 0.44), pt(0.4, 0.44), pt(0.4, 0.2)],
                st,
            );
        }
        Icon::Fetch => {
            let r = 0.36;
            let (a0, a1) = (-0.22 * TAU, 0.6 * TAU);
            let pts: Vec<Pos2> = (0..=28)
                .map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / 28.0;
                    pt(r * a.cos(), r * a.sin())
                })
                .collect();
            let tip = *pts.last().unwrap();
            p.line(pts, st);
            let tangent = vec2(-a1.sin(), a1.cos());
            let normal = vec2(a1.cos(), a1.sin());
            let back = tip - tangent * 0.18 * s;
            p.line(
                vec![back + normal * 0.13 * s, tip, back - normal * 0.13 * s],
                st,
            );
        }
        Icon::Branch => {
            let r = 0.11 * s;
            p.circle_stroke(pt(-0.27, -0.3), r, st);
            p.circle_stroke(pt(-0.27, 0.3), r, st);
            p.circle_stroke(pt(0.27, -0.3), r, st);
            p.line_segment([pt(-0.27, -0.3 + 0.11), pt(-0.27, 0.3 - 0.11)], st);
            p.add(CubicBezierShape::from_points_stroke(
                [
                    pt(0.27, -0.3 + 0.11),
                    pt(0.27, 0.08),
                    pt(-0.27, -0.02),
                    pt(-0.27, 0.3 - 0.11),
                ],
                false,
                Color32::TRANSPARENT,
                st,
            ));
        }
        Icon::Stash => {
            p.rect_stroke(
                Rect::from_min_max(pt(-0.44, -0.4), pt(0.44, -0.14)),
                CornerRadius::same(2),
                st,
                StrokeKind::Middle,
            );
            p.line(
                vec![
                    pt(-0.36, -0.14),
                    pt(-0.36, 0.42),
                    pt(0.36, 0.42),
                    pt(0.36, -0.14),
                ],
                st,
            );
            p.line_segment([pt(-0.11, 0.1), pt(0.11, 0.1)], st);
        }
        Icon::Pop => {
            p.line(
                vec![pt(-0.4, 0.02), pt(-0.4, 0.44), pt(0.4, 0.44), pt(0.4, 0.02)],
                st,
            );
            p.line_segment([pt(0.0, 0.22), pt(0.0, -0.44)], st);
            p.line(vec![pt(-0.2, -0.24), pt(0.0, -0.44), pt(0.2, -0.24)], st);
        }
        Icon::Search => {
            p.circle_stroke(pt(-0.08, -0.08), 0.28 * s, st);
            p.line_segment([pt(0.13, 0.13), pt(0.42, 0.42)], st);
        }
        Icon::Gear => {
            p.circle_stroke(c, 0.3 * s, st);
            p.circle_stroke(c, 0.1 * s, Stroke::new(st.width * 0.9, color));
            for i in 0..8 {
                let a = TAU * i as f32 / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment(
                    [c + d * 0.3 * s, c + d * 0.47 * s],
                    Stroke::new(st.width + 0.4, color),
                );
            }
        }
        Icon::Plus => {
            p.line_segment([pt(-0.36, 0.0), pt(0.36, 0.0)], st);
            p.line_segment([pt(0.0, -0.36), pt(0.0, 0.36)], st);
        }
        Icon::Minus => {
            p.line_segment([pt(-0.36, 0.0), pt(0.36, 0.0)], st);
        }
        Icon::Close => {
            p.line_segment([pt(-0.3, -0.3), pt(0.3, 0.3)], st);
            p.line_segment([pt(-0.3, 0.3), pt(0.3, -0.3)], st);
        }
        Icon::Shield => {
            p.add(egui::Shape::closed_line(
                vec![
                    pt(0.0, -0.46),
                    pt(0.38, -0.32),
                    pt(0.36, 0.04),
                    pt(0.0, 0.46),
                    pt(-0.36, 0.04),
                    pt(-0.38, -0.32),
                ],
                st,
            ));
            p.line(vec![pt(-0.16, 0.0), pt(-0.05, 0.12), pt(0.17, -0.12)], st);
        }
        Icon::Graph => {
            let r = (s * 0.1).max(2.0);
            p.line_segment([pt(-0.18, -0.46), pt(-0.18, 0.46)], st);
            let branch = vec![pt(-0.18, 0.2), pt(0.2, -0.02), pt(0.2, -0.28)];
            p.line(branch, st);
            for y in [-0.3, 0.02, 0.32] {
                p.circle_filled(pt(-0.18, y), r, color);
            }
            p.circle_filled(pt(0.2, -0.3), r, color);
        }
        Icon::Chart => {
            p.line_segment([pt(-0.44, 0.44), pt(0.46, 0.44)], st);
            p.line_segment([pt(-0.28, 0.36), pt(-0.28, 0.02)], st);
            p.line_segment([pt(0.0, 0.36), pt(0.0, -0.4)], st);
            p.line_segment([pt(0.28, 0.36), pt(0.28, -0.14)], st);
        }
        Icon::ChevronDown => {
            p.line(vec![pt(-0.3, -0.15), pt(0.0, 0.15), pt(0.3, -0.15)], st);
        }
        Icon::ChevronUp => {
            p.line(vec![pt(-0.3, 0.15), pt(0.0, -0.15), pt(0.3, 0.15)], st);
        }
        Icon::Undo => {
            let r = 0.3;
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -TAU * 0.5 * i as f32 / 20.0;
                    pt(0.05 + r * a.cos(), 0.08 + r * a.sin())
                })
                .collect();
            let tip = *pts.last().unwrap();
            p.line(pts, st);
            p.line(
                vec![
                    tip + vec2(-0.16 * s, -0.12 * s),
                    tip,
                    tip + vec2(0.16 * s, -0.1 * s),
                ],
                st,
            );
        }
        Icon::Warning => {
            p.line(
                vec![
                    pt(0.0, -0.42),
                    pt(0.46, 0.38),
                    pt(-0.46, 0.38),
                    pt(0.0, -0.42),
                ],
                st,
            );
            p.line_segment([pt(0.0, -0.12), pt(0.0, 0.14)], st);
            p.circle_filled(pt(0.0, 0.27), 0.055 * s, color);
        }
        Icon::Info => {
            p.circle_stroke(c, 0.42 * s, st);
            p.circle_filled(pt(0.0, -0.2), 0.06 * s, color);
            p.line_segment([pt(0.0, -0.04), pt(0.0, 0.26)], st);
        }
        Icon::Check => {
            p.line(vec![pt(-0.36, 0.02), pt(-0.1, 0.28), pt(0.38, -0.26)], st);
        }
        Icon::Error => {
            p.circle_stroke(c, 0.42 * s, st);
            p.line_segment([pt(-0.17, -0.17), pt(0.17, 0.17)], st);
            p.line_segment([pt(-0.17, 0.17), pt(0.17, -0.17)], st);
        }
        Icon::List => {
            for y in [-0.3, 0.0, 0.3] {
                p.circle_filled(pt(-0.38, y), 0.06 * s, color);
                p.line_segment([pt(-0.2, y), pt(0.42, y)], st);
            }
        }
        Icon::External => {
            p.line(
                vec![
                    pt(0.05, -0.4),
                    pt(-0.4, -0.4),
                    pt(-0.4, 0.4),
                    pt(0.4, 0.4),
                    pt(0.4, -0.05),
                ],
                st,
            );
            p.line_segment([pt(-0.05, 0.05), pt(0.42, -0.42)], st);
            p.line(vec![pt(0.12, -0.42), pt(0.42, -0.42), pt(0.42, -0.12)], st);
        }
        Icon::Play => {
            p.circle_stroke(c, 0.42 * s, st);
            p.add(egui::Shape::convex_polygon(
                vec![pt(-0.12, -0.2), pt(0.2, 0.0), pt(-0.12, 0.2)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Copy => {
            p.rect_stroke(
                Rect::from_min_max(pt(-0.42, -0.42), pt(0.18, 0.18)),
                CornerRadius::same(2),
                st,
                StrokeKind::Middle,
            );
            p.line(
                vec![
                    pt(-0.16, 0.3),
                    pt(-0.16, 0.42),
                    pt(0.42, 0.42),
                    pt(0.42, -0.16),
                    pt(0.3, -0.16),
                ],
                st,
            );
        }
        Icon::Clock => {
            p.circle_stroke(c, 0.42 * s, st);
            p.line(vec![pt(-0.18, 0.1), pt(0.0, 0.02), pt(0.0, -0.28)], st);
        }
        Icon::PullRequest => {
            let r = 0.1 * s;
            p.circle_stroke(pt(-0.27, -0.3), r, st);
            p.circle_stroke(pt(-0.27, 0.3), r, st);
            p.circle_stroke(pt(0.27, 0.3), r, st);
            p.line_segment([pt(-0.27, -0.3 + 0.1), pt(-0.27, 0.3 - 0.1)], st);
            p.line(
                vec![pt(0.02, -0.3), pt(0.27, -0.3), pt(0.27, 0.3 - 0.1)],
                st,
            );
            p.line(vec![pt(0.14, -0.42), pt(0.02, -0.3), pt(0.14, -0.18)], st);
        }
        Icon::Stop => {
            p.circle_stroke(c, 0.42 * s, st);
            p.rect_filled(
                Rect::from_center_size(c, Vec2::splat(0.3 * s)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Rerun => {
            let r = 0.34;
            let (a0, a1) = (-0.1 * TAU, 0.62 * TAU);
            let pts: Vec<Pos2> = (0..=28)
                .map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / 28.0;
                    pt(r * a.cos(), r * a.sin())
                })
                .collect();
            let tip = pts[0];
            p.line(pts, st);
            p.line(
                vec![
                    tip + vec2(-0.18 * s, -0.02 * s),
                    tip,
                    tip + vec2(0.02 * s, -0.2 * s),
                ],
                st,
            );
        }
        Icon::Home => {
            p.line(vec![pt(-0.44, -0.02), pt(0.0, -0.42), pt(0.44, -0.02)], st);
            p.line(
                vec![
                    pt(-0.3, -0.14),
                    pt(-0.3, 0.42),
                    pt(0.3, 0.42),
                    pt(0.3, -0.14),
                ],
                st,
            );
            p.line(
                vec![
                    pt(-0.08, 0.42),
                    pt(-0.08, 0.16),
                    pt(0.08, 0.16),
                    pt(0.08, 0.42),
                ],
                st,
            );
        }
        Icon::Rocket => {
            p.line(
                vec![
                    pt(0.0, -0.46),
                    pt(0.12, -0.32),
                    pt(0.16, -0.16),
                    pt(0.16, 0.18),
                    pt(-0.16, 0.18),
                    pt(-0.16, -0.16),
                    pt(-0.12, -0.32),
                    pt(0.0, -0.46),
                ],
                st,
            );
            p.line(vec![pt(-0.16, 0.0), pt(-0.34, 0.2), pt(-0.16, 0.2)], st);
            p.line(vec![pt(0.16, 0.0), pt(0.34, 0.2), pt(0.16, 0.2)], st);
            p.circle_stroke(pt(0.0, -0.12), 0.06 * s, st);
            p.line(vec![pt(-0.07, 0.26), pt(0.0, 0.44), pt(0.07, 0.26)], st);
        }
        Icon::Lock => {
            let arc: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI + std::f32::consts::PI * i as f32 / 16.0;
                    pt(0.2 * a.cos(), -0.2 + 0.2 * a.sin())
                })
                .collect();
            let mut shackle = vec![pt(-0.2, -0.04)];
            shackle.extend(arc);
            shackle.push(pt(0.2, -0.04));
            p.line(shackle, st);
            p.rect_stroke(
                Rect::from_min_max(pt(-0.32, -0.04), pt(0.32, 0.42)),
                CornerRadius::same(2),
                st,
                StrokeKind::Middle,
            );
            p.circle_filled(pt(0.0, 0.17), 0.055 * s, color);
        }
        Icon::Terminal => {
            p.rect_stroke(
                Rect::from_min_max(pt(-0.44, -0.36), pt(0.44, 0.36)),
                CornerRadius::same(2),
                st,
                StrokeKind::Middle,
            );
            p.line(vec![pt(-0.24, -0.12), pt(-0.08, 0.02), pt(-0.24, 0.16)], st);
            p.line_segment([pt(0.02, 0.18), pt(0.24, 0.18)], st);
        }
        Icon::Issue => {
            p.circle_stroke(c, 0.42 * s, st);
            p.circle_filled(c, 0.09 * s, color);
        }
        Icon::Inbox => {
            p.line(
                vec![
                    pt(-0.42, 0.06),
                    pt(-0.28, -0.36),
                    pt(0.28, -0.36),
                    pt(0.42, 0.06),
                ],
                st,
            );
            p.line(
                vec![
                    pt(-0.42, 0.06),
                    pt(-0.42, 0.38),
                    pt(0.42, 0.38),
                    pt(0.42, 0.06),
                    pt(0.16, 0.06),
                    pt(0.1, 0.18),
                    pt(-0.1, 0.18),
                    pt(-0.16, 0.06),
                    pt(-0.42, 0.06),
                ],
                st,
            );
        }
        Icon::Package => {
            p.line(
                vec![
                    pt(0.0, -0.44),
                    pt(0.4, -0.22),
                    pt(0.4, 0.22),
                    pt(0.0, 0.44),
                    pt(-0.4, 0.22),
                    pt(-0.4, -0.22),
                    pt(0.0, -0.44),
                ],
                st,
            );
            p.line(vec![pt(-0.4, -0.22), pt(0.0, 0.0), pt(0.4, -0.22)], st);
            p.line_segment([pt(0.0, 0.0), pt(0.0, 0.44)], st);
        }
        Icon::Cloud => {
            let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| -> Vec<Pos2> {
                (0..=12)
                    .map(|i| {
                        let a = a0 + (a1 - a0) * i as f32 / 12.0;
                        pt(cx + r * a.cos(), cy + r * a.sin())
                    })
                    .collect()
            };
            let pi = std::f32::consts::PI;
            let mut pts = vec![pt(0.26, 0.26), pt(-0.24, 0.26)];
            pts.extend(arc(-0.24, 0.1, 0.16, 0.5 * pi, 1.5 * pi));
            pts.extend(arc(0.0, -0.02, 0.24, 1.08 * pi, 1.9 * pi));
            pts.extend(arc(0.26, 0.12, 0.14, 1.6 * pi, 2.5 * pi));
            p.line(pts, st);
        }
        Icon::Tag => {
            p.line(
                vec![
                    pt(-0.42, -0.42),
                    pt(0.02, -0.42),
                    pt(0.44, 0.0),
                    pt(0.0, 0.44),
                    pt(-0.42, 0.02),
                    pt(-0.42, -0.42),
                ],
                st,
            );
            p.circle_filled(pt(-0.2, -0.2), 0.065 * s, color);
        }
        Icon::Bug => {
            p.circle_stroke(pt(0.0, 0.1), 0.24 * s, st);
            p.circle_stroke(pt(0.0, -0.26), 0.1 * s, st);
            p.line_segment([pt(0.0, -0.14), pt(0.0, 0.34)], st);
            for y in [-0.02, 0.12, 0.26] {
                p.line_segment([pt(-0.24, y), pt(-0.42, y + 0.06)], st);
                p.line_segment([pt(0.24, y), pt(0.42, y + 0.06)], st);
            }
        }
        Icon::Folder => {
            p.line(
                vec![
                    pt(-0.44, -0.32),
                    pt(-0.12, -0.32),
                    pt(-0.04, -0.2),
                    pt(0.44, -0.2),
                    pt(0.44, 0.36),
                    pt(-0.44, 0.36),
                    pt(-0.44, -0.32),
                ],
                st,
            );
        }
        Icon::Merge => {
            let r = 0.1 * s;
            p.circle_stroke(pt(-0.24, -0.32), r, st);
            p.circle_stroke(pt(-0.24, 0.32), r, st);
            p.circle_stroke(pt(0.26, 0.06), r, st);
            p.line_segment([pt(-0.24, -0.22), pt(-0.24, 0.22)], st);
            p.line(
                vec![
                    pt(-0.24, -0.14),
                    pt(-0.12, 0.0),
                    pt(0.04, 0.06),
                    pt(0.16, 0.06),
                ],
                st,
            );
        }
        Icon::Comment => {
            p.line(
                vec![
                    pt(-0.42, -0.36),
                    pt(0.42, -0.36),
                    pt(0.42, 0.2),
                    pt(0.0, 0.2),
                    pt(-0.26, 0.42),
                    pt(-0.2, 0.2),
                    pt(-0.42, 0.2),
                    pt(-0.42, -0.36),
                ],
                st,
            );
        }
        Icon::Key => {
            p.circle_stroke(pt(-0.22, 0.0), 0.18 * s, st);
            p.line_segment([pt(-0.04, 0.0), pt(0.44, 0.0)], st);
            p.line_segment([pt(0.28, 0.0), pt(0.28, 0.16)], st);
            p.line_segment([pt(0.42, 0.0), pt(0.42, 0.13)], st);
        }
        Icon::Gauge => {
            let pi = std::f32::consts::PI;
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = pi + pi * i as f32 / 20.0;
                    pt(0.4 * a.cos(), 0.16 + 0.4 * a.sin())
                })
                .collect();
            p.line(pts, st);
            p.line_segment([pt(-0.4, 0.16), pt(0.4, 0.16)], st);
            p.line_segment([pt(0.0, 0.16), pt(0.2, -0.1)], st);
            p.circle_filled(pt(0.0, 0.16), 0.06 * s, color);
        }
        Icon::Eye => {
            let pi = std::f32::consts::PI;
            let top: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = pi + pi * i as f32 / 16.0;
                    pt(0.44 * a.cos(), 0.3 * a.sin())
                })
                .collect();
            let bottom: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = pi * i as f32 / 16.0;
                    pt(0.44 * a.cos(), 0.3 * a.sin())
                })
                .collect();
            p.line(top, st);
            p.line(bottom, st);
            p.circle_stroke(c, 0.12 * s, st);
        }
        Icon::Edit => {
            p.line(
                vec![
                    pt(-0.38, 0.38),
                    pt(-0.32, 0.16),
                    pt(0.2, -0.36),
                    pt(0.36, -0.2),
                    pt(-0.16, 0.32),
                    pt(-0.38, 0.38),
                ],
                st,
            );
            p.line_segment([pt(0.08, -0.24), pt(0.24, -0.08)], st);
        }
        Icon::Trash => {
            p.line_segment([pt(-0.4, -0.28), pt(0.4, -0.28)], st);
            p.line(
                vec![
                    pt(-0.12, -0.28),
                    pt(-0.12, -0.42),
                    pt(0.12, -0.42),
                    pt(0.12, -0.28),
                ],
                st,
            );
            p.line(
                vec![
                    pt(-0.28, -0.28),
                    pt(-0.22, 0.42),
                    pt(0.22, 0.42),
                    pt(0.28, -0.28),
                ],
                st,
            );
            p.line_segment([pt(-0.06, -0.12), pt(-0.06, 0.28)], st);
            p.line_segment([pt(0.08, -0.12), pt(0.08, 0.28)], st);
        }
        Icon::Bell => {
            let pi = std::f32::consts::PI;
            let mut pts = vec![pt(-0.38, 0.26), pt(-0.26, 0.1), pt(-0.26, -0.08)];
            pts.extend((0..=14).map(|i| {
                let a = pi + pi * i as f32 / 14.0;
                pt(0.26 * a.cos(), -0.08 + 0.3 * a.sin())
            }));
            pts.extend([pt(0.26, 0.1), pt(0.38, 0.26), pt(-0.38, 0.26)]);
            p.line(pts, st);
            p.line(vec![pt(-0.1, 0.34), pt(0.0, 0.42), pt(0.1, 0.34)], st);
        }
        Icon::Link => {
            let r = CornerRadius::same((0.12 * s) as u8);
            let a = Rect::from_center_size(pt(-0.16, 0.16), vec2(0.42 * s, 0.24 * s));
            let b = Rect::from_center_size(pt(0.16, -0.16), vec2(0.42 * s, 0.24 * s));
            let rot_a = egui::emath::Rot2::from_angle(-std::f32::consts::FRAC_PI_4);
            for (rect, center) in [(a, pt(-0.16, 0.16)), (b, pt(0.16, -0.16))] {
                let corners = [
                    rect.left_top(),
                    rect.right_top(),
                    rect.right_bottom(),
                    rect.left_bottom(),
                    rect.left_top(),
                ];
                let pts: Vec<Pos2> = corners
                    .iter()
                    .map(|q| center + rot_a * (*q - center))
                    .collect();
                p.line(pts, st);
            }
            let _ = r;
        }
        Icon::Wheel => {
            p.circle_stroke(c, 0.28 * s, st);
            p.circle_stroke(c, 0.08 * s, st);
            for k in 0..6 {
                let a = std::f32::consts::TAU * k as f32 / 6.0;
                p.line_segment(
                    [
                        pt(0.08 * a.cos(), 0.08 * a.sin()),
                        pt(0.44 * a.cos(), 0.44 * a.sin()),
                    ],
                    st,
                );
            }
        }
        Icon::Star => {
            let pts: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 5.0;
                    let r = if i % 2 == 0 { 0.46 } else { 0.2 };
                    pt(r * a.cos(), 0.04 + r * a.sin())
                })
                .collect();
            p.line(pts, st);
        }
        Icon::Download => {
            p.line_segment([pt(0.0, -0.44), pt(0.0, 0.16)], st);
            p.line(vec![pt(-0.22, -0.06), pt(0.0, 0.16), pt(0.22, -0.06)], st);
            p.line_segment([pt(-0.4, 0.42), pt(0.4, 0.42)], st);
        }
    }
}

/// Dot for a run / job / check outcome: filled green check, red cross,
/// amber arc while running, hollow while queued, grey otherwise. Running
/// outcomes animate; the caller should request a repaint for them.
pub fn outcome_dot(p: &Painter, outcome: gitvis_core::github::Outcome, c: Pos2, r: f32, t: f64) {
    use gitvis_core::github::Outcome;
    let weak = Color32::from_gray(128);
    match outcome {
        Outcome::Success => {
            p.circle_filled(c, r, theme::GREEN);
            let s = r * 1.1;
            p.line(
                vec![
                    pos2(c.x - s * 0.5, c.y + s * 0.02),
                    pos2(c.x - s * 0.12, c.y + s * 0.4),
                    pos2(c.x + s * 0.55, c.y - s * 0.4),
                ],
                Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE),
            );
        }
        Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired | Outcome::Stale => {
            p.circle_filled(c, r, theme::RED);
            let d = r * 0.42;
            let st = Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE);
            p.line_segment([pos2(c.x - d, c.y - d), pos2(c.x + d, c.y + d)], st);
            p.line_segment([pos2(c.x - d, c.y + d), pos2(c.x + d, c.y - d)], st);
        }
        Outcome::InProgress => {
            let st = Stroke::new((r * 0.32).clamp(1.3, 2.2), theme::AMBER);
            p.circle_stroke(
                c,
                r * 0.85,
                Stroke::new(st.width, theme::AMBER.gamma_multiply(0.25)),
            );
            let a0 = (t * 2.4) as f32 % TAU;
            let pts: Vec<Pos2> = (0..=14)
                .map(|i| {
                    let a = a0 + TAU * 0.3 * i as f32 / 14.0;
                    pos2(c.x + r * 0.85 * a.cos(), c.y + r * 0.85 * a.sin())
                })
                .collect();
            p.line(pts, st);
        }
        Outcome::Queued => {
            p.circle_stroke(
                c,
                r * 0.85,
                Stroke::new((r * 0.3).clamp(1.2, 2.0), theme::AMBER),
            );
        }
        Outcome::Cancelled => {
            p.circle_filled(c, r, weak);
            p.line_segment(
                [pos2(c.x - r * 0.45, c.y), pos2(c.x + r * 0.45, c.y)],
                Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE),
            );
        }
        Outcome::Skipped => {
            p.circle_stroke(c, r * 0.85, Stroke::new((r * 0.28).clamp(1.2, 2.0), weak));
            p.line_segment(
                [
                    pos2(c.x - r * 0.4, c.y + r * 0.4),
                    pos2(c.x + r * 0.4, c.y - r * 0.4),
                ],
                Stroke::new((r * 0.28).clamp(1.2, 2.0), weak),
            );
        }
        Outcome::Neutral | Outcome::Unknown => {
            p.circle_filled(c, r * 0.8, weak);
        }
    }
}

/// Text colour matching [`outcome_dot`].
pub fn outcome_color(outcome: gitvis_core::github::Outcome, weak: Color32) -> Color32 {
    use gitvis_core::github::Outcome;
    match outcome {
        Outcome::Success => theme::GREEN,
        Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired | Outcome::Stale => {
            theme::RED
        }
        Outcome::InProgress | Outcome::Queued => theme::AMBER,
        _ => weak,
    }
}

/// Allocate space for an icon and draw it.
pub fn show(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui.painter(), icon, rect.center(), size, color);
    rect
}

/// A button with an optional icon and label. Ghost by default (no fill
/// until hovered); `primary` fills it with the accent colour.
pub struct IconButton<'a> {
    icon: Option<Icon>,
    label: Option<&'a str>,
    primary: bool,
    danger: bool,
    enabled: bool,
    compact: bool,
    selected: bool,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: Icon, label: &'a str) -> Self {
        Self {
            icon: Some(icon),
            label: Some(label),
            primary: false,
            danger: false,
            enabled: true,
            compact: false,
            selected: false,
        }
    }

    /// Keep the hover fill on, for toggles.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn icon(icon: Icon) -> Self {
        Self {
            label: None,
            ..Self::new(icon, "")
        }
    }

    pub fn text(label: &'a str) -> Self {
        Self {
            icon: None,
            ..Self::new(Icon::Plus, label)
        }
    }

    pub fn primary(mut self, primary: bool) -> Self {
        self.primary = primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = *theme::pal(ui);
        let height = if self.compact { 26.0 } else { 30.0 };
        let icon_size = if self.compact { 14.0 } else { 15.0 };
        let font = FontId::new(
            if self.compact {
                theme::size::SMALL
            } else {
                theme::size::BODY
            },
            theme::medium(),
        );

        let base_text = if self.primary {
            Color32::WHITE
        } else if self.danger {
            p.danger
        } else {
            p.text
        };
        let galley = self
            .label
            .filter(|l| !l.is_empty())
            .map(|l| ui.painter().layout_no_wrap(l.to_owned(), font, base_text));
        let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
        let icon_w = if self.icon.is_some() { icon_size } else { 0.0 };
        let gap = if self.icon.is_some() && galley.is_some() {
            7.0
        } else {
            0.0
        };
        let pad = if galley.is_some() {
            if self.compact { 9.0 } else { 11.0 }
        } else {
            (height - icon_size) / 2.0
        };
        let width = pad * 2.0 + icon_w + gap + text_w;

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), sense);
        if !ui.is_rect_visible(rect) {
            return resp;
        }

        let hovered = self.enabled && resp.hovered();
        let pressed = self.enabled && resp.is_pointer_button_down_on();
        let fill = if self.primary {
            // Primary + danger: a filled destructive button.
            let base = if self.danger { p.danger } else { p.accent };
            if pressed {
                base.lerp_to_gamma(Color32::BLACK, 0.15)
            } else if hovered {
                base.lerp_to_gamma(Color32::WHITE, 0.10)
            } else {
                base
            }
        } else if pressed {
            p.border_strong
        } else if self.selected {
            p.selection
        } else if hovered {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        let alpha = if self.enabled { 1.0 } else { 0.38 };
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(
                rect,
                CornerRadius::same(theme::radius::M),
                fill.gamma_multiply(alpha),
            );
        }
        let text_color = if self.primary || self.danger {
            base_text
        } else if hovered || self.selected {
            p.text_strong
        } else {
            base_text
        }
        .gamma_multiply(alpha);

        let mut x = rect.left() + pad;
        if let Some(icon) = self.icon {
            paint(
                painter,
                icon,
                pos2(x + icon_size / 2.0, rect.center().y),
                icon_size,
                text_color,
            );
            x += icon_size + gap;
        }
        if let Some(galley) = galley {
            painter.galley_with_override_text_color(
                pos2(x, rect.center().y - galley.size().y / 2.0),
                galley,
                text_color,
            );
        }
        if self.enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

impl egui::Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui)
    }
}

/// Thin vertical divider for toolbars.
pub fn divider(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0, ui.visuals().widgets.noninteractive.bg_stroke.color);
}

/// A small keyboard-shortcut badge like `Ctrl+F`.
pub fn kbd(ui: &mut Ui, text: &str) {
    let font = FontId::proportional(11.0);
    let color = ui.visuals().weak_text_color();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(10.0, 6.0), Sense::hover());
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, color.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}
