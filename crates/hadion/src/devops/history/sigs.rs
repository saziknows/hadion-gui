//! Signature verification: the visible graph rows are verified lazily, in
//! batches, one read at a time (the next batch starts when the last one
//! ends, with whatever is visible then), and cached per commit. Badges in
//! the graph; the commit panel shows signer, key, fingerprint and trust,
//! and explains "can't check" with the commands that fix it.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use egui::{Painter, Pos2, Rect, Sense, Ui, vec2};
use hadion_core::devops::history::signature::{self as core, Level, Setup, Sig, Status};
use hadion_core::{Commit, Oid};

use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::devops::hooks::HookCx;
use crate::state::{Action, RepoState};
use crate::theme::{Palette, pal};
use crate::views::icons::{self, Icon};
use crate::views::kit::{self, Tone};

/// Commits verified per read.
const BATCH: usize = 80;

#[derive(Default)]
pub struct Sigs {
    pub cache: HashMap<Oid, Sig>,
    pub in_flight: bool,
    /// Rows the graph shows.
    pub visible: Range<usize>,
    /// Full results (with what gpg / ssh-keygen said) for the panel.
    pub detail: HashMap<Oid, (Sig, String)>,
    pub detail_pending: HashSet<Oid>,
    pub setup: Option<Setup>,
    /// The last batch failed.
    pub error: Option<String>,
}

impl Sigs {
    pub fn get(&self, id: &Oid) -> Option<&Sig> {
        self.cache.get(id)
    }
}

/// Start a batch for the visible rows not yet verified.
fn next_batch(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let s = &mut repo.devops.history.sigs;
    if s.in_flight {
        return;
    }
    let ids: Vec<Oid> = s
        .visible
        .clone()
        .filter_map(|row| snap.commit_of_row(row))
        .filter_map(|i| snap.commits.get(i))
        .map(|c| c.id)
        .filter(|id| !s.cache.contains_key(id))
        .take(BATCH)
        .collect();
    if ids.is_empty() {
        return;
    }
    s.in_flight = true;
    let shas: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    cx.read(
        format!("Verify {} signatures", ids.len()),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r = core::verify(dir, &shas, false, &mut cmds);
            log.commands.extend(cmds);
            Msg::Signatures(ids, r)
        },
    );
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::VisibleRows(r) => {
            repo.devops.history.sigs.visible = r;
            next_batch(cx, repo);
        }
        Act::VerifyVisible => next_batch(cx, repo),
        Act::RecheckSignatures => {
            let s = &mut repo.devops.history.sigs;
            if s.in_flight {
                return;
            }
            s.cache.clear();
            s.detail.clear();
            s.setup = None;
            s.error = None;
            next_batch(cx, repo);
        }
        Act::VerifyDetail(id) => {
            let s = &mut repo.devops.history.sigs;
            if s.detail.contains_key(&id) || !s.detail_pending.insert(id) {
                return;
            }
            let need_setup = s.setup.is_none();
            let sha = id.to_string();
            cx.read(
                format!(
                    "Verify the signature of {}",
                    hadion_core::repo::short_id(id)
                ),
                move |dir, log| {
                    let mut cmds = Vec::new();
                    let setup = need_setup.then(|| core::setup(dir, &mut cmds));
                    let r = core::verify(dir, std::slice::from_ref(&sha), true, &mut cmds);
                    log.commands.extend(cmds);
                    Msg::SignatureDetail(id, r, Box::new(setup.unwrap_or_default()))
                },
            );
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let s = &mut repo.devops.history.sigs;
    match msg {
        Msg::Signatures(ids, r) => {
            s.in_flight = false;
            match r {
                Ok(v) => {
                    s.error = None;
                    let mut got: HashMap<String, Sig> = v.sigs.into_iter().collect();
                    for id in ids {
                        let sig = got.remove(&id.to_string()).unwrap_or_else(Sig::unsigned);
                        s.cache.insert(id, sig);
                    }
                }
                Err(e) => {
                    // Do not retry the same rows in a loop.
                    s.error = Some(e);
                    for id in ids {
                        s.cache.insert(id, Sig::unsigned());
                    }
                }
            }
            next_batch(cx, repo);
        }
        Msg::SignatureDetail(id, r, setup) => {
            s.detail_pending.remove(&id);
            // An empty setup means it was not read this time (cached).
            if !setup.format.is_empty() {
                s.setup = Some(*setup);
            }
            match r {
                Ok(v) => {
                    if let Some((_, sig)) = v.sigs.into_iter().next() {
                        s.cache.insert(id, sig.clone());
                        s.detail.insert(id, (sig, v.stderr));
                    }
                }
                Err(e) => {
                    let mut sig = Sig::unsigned();
                    sig.status = Status::CantCheck;
                    s.detail.insert(id, (sig, e));
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn level_color(p: &Palette, l: Level) -> egui::Color32 {
    match l {
        Level::Good => p.success,
        Level::Warn => p.warning,
        Level::Bad => p.danger,
        Level::Unknown | Level::None => p.text_3,
    }
}

pub fn tone(l: Level) -> Tone {
    match l {
        Level::Good => Tone::Success,
        Level::Warn => Tone::Warning,
        Level::Bad => Tone::Danger,
        Level::Unknown | Level::None => Tone::Neutral,
    }
}

/// The badge of a graph row, centred on `center`; returns its hit rect
/// (`None` for unsigned commits).
pub fn paint_badge(painter: &Painter, p: &Palette, center: Pos2, sig: &Sig) -> Option<Rect> {
    sig.status.badge()?;
    let c = level_color(p, sig.status.level());
    icons::paint(painter, Icon::Shield, center, 13.0, c);
    Some(Rect::from_center_size(center, vec2(16.0, 18.0)))
}

/// What the badge's tooltip says.
pub fn badge_tip(sig: &Sig) -> String {
    let mut s = format!(
        "{}{}",
        sig.status.badge().unwrap_or(""),
        sig.kind
            .map(|k| format!(" · {} signature", k.label()))
            .unwrap_or_default()
    );
    if !sig.signer.is_empty() {
        s.push_str(&format!("\nSigned by {}", sig.signer));
    }
    if !sig.key.is_empty() {
        s.push_str(&format!("\nKey {}", sig.key));
    }
    s.push_str("\n\n");
    s.push_str(sig.status.label());
    s
}

/// The signature block of the commit panel.
pub fn commit_panel(ui: &mut Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    let s = &h.devops.history.sigs;
    let detail = s.detail.get(&c.id);
    let sig = detail.map(|(s, _)| s).or_else(|| s.get(&c.id));
    let Some(sig) = sig else {
        return;
    };
    if sig.status == Status::Unsigned {
        return;
    }
    if detail.is_none() && !s.detail_pending.contains(&c.id) {
        actions.push(Act::VerifyDetail(c.id).into());
    }
    let p = *pal(ui);
    kit::section_with(ui, "Signature", None, |ui| {
        if kit::link(ui, "Check again").clicked() {
            actions.push(Act::RecheckSignatures.into());
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
        icons::paint(
            ui.painter(),
            Icon::Shield,
            r.center(),
            14.0,
            level_color(&p, sig.status.level()),
        );
        if let Some(b) = sig.status.badge() {
            kit::badge(ui, b, tone(sig.status.level()));
        }
        ui.label(egui::RichText::new(sig.status.label()).color(p.text));
    });
    ui.add_space(2.0);
    kit::kv(ui, 92.0, |kv| {
        if let Some(k) = sig.kind {
            kv.text("Kind", k.label());
        }
        if sig.signer.is_empty() {
            kv.muted(
                "Signer",
                if sig.status == Status::Untrusted {
                    "not identified: no trusted key matches"
                } else {
                    "unknown (not checked)"
                },
            );
        } else {
            kv.text("Signer", sig.signer.as_str());
        }
        if !sig.key.is_empty() {
            kv.text("Key", kit::mono(&sig.key));
        }
        if !sig.fingerprint.is_empty() && sig.fingerprint != sig.key {
            kv.text("Fingerprint", kit::mono(&sig.fingerprint));
        }
        if !sig.primary.is_empty() && sig.primary != sig.fingerprint {
            kv.text("Primary key", kit::mono(&sig.primary));
        }
        if !sig.trust.is_empty() && sig.trust != "undefined" {
            kv.text("Trust", sig.trust.as_str());
        } else if sig.status != Status::CantCheck {
            kv.muted("Trust", "undefined");
        }
    });
    let setup = s.setup.clone().map(|mut st| {
        if let Some((_, err)) = detail {
            st.stderr = err.clone();
        }
        st
    });
    match (setup, detail) {
        (Some(setup), Some(_)) => {
            if let Some(a) = core::advise(sig, &setup, &c.email) {
                ui.add_space(4.0);
                let t = match sig.status.level() {
                    Level::Bad => Tone::Danger,
                    Level::Warn => Tone::Warning,
                    _ => Tone::Accent,
                };
                kit::callout_with(ui, t, &a.title, &a.body, |ui| {
                    if !a.fix.is_empty() {
                        ui.add_space(4.0);
                        kit::code_block(ui, &a.fix.join("\n"), actions);
                        ui.label(
                            egui::RichText::new(
                                "Run these in a terminal, then Check again. Hadion does not change your keys or git configuration.",
                            )
                            .size(crate::theme::size::SMALL)
                            .color(p.text_2),
                        );
                    }
                });
            }
        }
        _ => kit::loading(ui, "Reading the signature details…"),
    }
    if let Some((full, err)) = detail
        && (!full.message.is_empty() || !err.trim().is_empty())
    {
        ui.add_space(2.0);
        egui::CollapsingHeader::new(
            egui::RichText::new("What the verifier said")
                .size(crate::theme::size::SMALL)
                .color(p.text_2),
        )
        .id_salt(("hist_sig_raw", c.id))
        .show(ui, |ui| {
            let mut text = full.message.clone();
            if !err.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(err.trim());
            }
            kit::code_block(ui, &text, actions);
        });
    }
}
