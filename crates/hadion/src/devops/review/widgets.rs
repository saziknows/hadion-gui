//! Pieces the Review page's views share: list rows for pull requests and
//! issues, label chips with their colours, people, times, readable bodies.

use egui::{
    Align2, Color32, CornerRadius, FontId, Label as ELabel, Rect, RichText, Sense, Ui, pos2, vec2,
};
use hadion_core::Oid;
use hadion_core::devops::review::{
    CheckRollup, Issue, Label, MergeState, Mergeable, PrState, PrSummary, ReviewDecision, local,
};

use super::State;
use crate::format;
use crate::theme::{Palette, Tone, medium, pal, semibold, size};
use crate::views::icons::{self, Icon};
use crate::views::{kit, truncated};

/// Three-line list rows: title, facts, badges and labels.
pub const ROW3_H: f32 = 74.0;

/// Longest body shown before "open on GitHub for the rest".
const MAX_BODY: usize = 12_000;

/// Run `add` (another area's hook row) in a child that cannot widen this
/// ui. egui grows a ui to fit its widest child (`max_rect |= rect`), so a
/// row too wide for a narrow window made everything after it wider than
/// the pane: text ran off the right edge and buttons left the screen.
/// Here such a row is clipped at the pane's edge instead.
pub fn contained(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let w = ui.available_width().max(0.0);
    let rect = Rect::from_min_size(ui.cursor().min, vec2(w, ui.available_height().max(0.0)));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
    child.shrink_clip_rect(rect);
    add(&mut child);
    let h = child.min_rect().height();
    ui.allocate_rect(Rect::from_min_size(rect.min, vec2(w, h)), Sense::hover());
}

/// `2026-10-26 16:46 · 1h ago`.
pub fn when(secs: i64, now: i64) -> String {
    format!(
        "{}  ·  {}",
        format::absolute(secs),
        format::relative(secs, now)
    )
}

pub fn rel(secs: Option<i64>, now: i64) -> String {
    secs.map(|s| format::relative(s, now)).unwrap_or_default()
}

/// The colour a label carries on GitHub.
pub fn label_color(l: &Label, p: &Palette) -> Color32 {
    match l.rgb() {
        Some((r, g, b)) => Color32::from_rgb(r, g, b),
        None => p.text_3,
    }
}

fn chip_font() -> FontId {
    FontId::new(size::CAPTION, medium())
}

/// Paint a label chip (colour dot and name) with its left edge at `x`;
/// returns its right edge.
pub fn paint_label_chip(painter: &egui::Painter, p: &Palette, x: f32, cy: f32, l: &Label) -> f32 {
    let g = painter.layout_no_wrap(l.name.clone(), chip_font(), p.text_2);
    let w = g.size().x + 24.0;
    let rect = Rect::from_min_size(pos2(x, cy - 9.0), vec2(w, 18.0));
    painter.rect_filled(rect, CornerRadius::same(9), p.hover);
    painter.circle_filled(pos2(x + 9.0, cy), 3.5, label_color(l, p));
    // A hairline ring keeps pale label colours visible on light surfaces.
    painter.circle_stroke(
        pos2(x + 9.0, cy),
        3.5,
        egui::Stroke::new(0.8, p.text_3.gamma_multiply(0.6)),
    );
    painter.galley(pos2(x + 17.0, cy - g.size().y / 2.0), g, p.text_2);
    rect.right()
}

pub fn label_chip_width(painter: &egui::Painter, l: &Label) -> f32 {
    painter
        .layout_no_wrap(l.name.clone(), chip_font(), Color32::WHITE)
        .size()
        .x
        + 24.0
}

/// An inline label chip.
pub fn label_chip(ui: &mut Ui, l: &Label) {
    let p = *pal(ui);
    let w = label_chip_width(ui.painter(), l);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    paint_label_chip(ui.painter(), &p, rect.left(), rect.center().y, l);
}

/// Wrapped label chips.
pub fn label_chips(ui: &mut Ui, labels: &[Label]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for l in labels {
            label_chip(ui, l);
        }
    });
}

/// Paint badges left to right from `x`; returns where the next item goes.
pub fn paint_badges_from(
    painter: &egui::Painter,
    p: &Palette,
    dark: bool,
    mut x: f32,
    cy: f32,
    badges: &[(String, Tone)],
    max_right: f32,
) -> f32 {
    for (text, tone) in badges {
        let w = kit::badge_width(painter, text);
        if x + w > max_right {
            break;
        }
        kit::paint_badge(painter, p, dark, x + w, cy, text, *tone);
        x += w + 4.0;
    }
    x
}

/// Status badges of a pull request row: state, review decision, checks,
/// conflicts, review requested from the viewer.
pub fn pr_badges(pr: &PrSummary, requested: bool) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    match pr.state {
        PrState::Merged => b.push(("Merged".to_owned(), Tone::Purple)),
        PrState::Closed => b.push(("Closed".to_owned(), Tone::Danger)),
        PrState::Open if pr.draft => b.push(("Draft".to_owned(), Tone::Neutral)),
        PrState::Open => {}
    }
    if requested && pr.is_open() {
        b.push(("Your review requested".to_owned(), Tone::Accent));
    }
    match pr.review_decision {
        Some(ReviewDecision::Approved) => b.push(("Approved".to_owned(), Tone::Success)),
        Some(ReviewDecision::ChangesRequested) => {
            b.push(("Changes requested".to_owned(), Tone::Danger))
        }
        _ => {}
    }
    if pr.is_open() {
        match pr.checks {
            Some(c @ (CheckRollup::Failure | CheckRollup::Error)) => {
                b.push((c.label().to_owned(), Tone::Danger))
            }
            Some(c @ (CheckRollup::Pending | CheckRollup::Expected)) => {
                b.push((c.label().to_owned(), Tone::Warning))
            }
            Some(CheckRollup::Success) => b.push(("Checks passed".to_owned(), Tone::Success)),
            None => {}
        }
        if pr.mergeable == Mergeable::Conflicting || pr.merge_state == MergeState::Dirty {
            b.push(("Conflicts".to_owned(), Tone::Danger));
        } else if pr.merge_state == MergeState::Behind {
            b.push(("Behind base".to_owned(), Tone::Warning));
        }
    }
    b
}

/// The colour of a pull request's icon: open neutral, draft faint, merged
/// purple, closed red.
pub fn pr_icon_color(p: &Palette, pr: &PrSummary) -> Color32 {
    match pr.state {
        PrState::Merged => p.purple,
        PrState::Closed => p.danger,
        PrState::Open if pr.draft => p.text_3,
        PrState::Open => p.success,
    }
}

/// `+1,052 −13` painted right-aligned at `right`; returns its left edge.
fn paint_diffstat(
    painter: &egui::Painter,
    p: &Palette,
    right: f32,
    cy: f32,
    add: u64,
    del: u64,
) -> f32 {
    let font = FontId::proportional(size::SMALL);
    let d = painter.text(
        pos2(right, cy),
        Align2::RIGHT_CENTER,
        format!("\u{2212}{}", format::count(del as usize)),
        font.clone(),
        p.danger,
    );
    let a = painter.text(
        pos2(d.left() - 6.0, cy),
        Align2::RIGHT_CENTER,
        format!("+{}", format::count(add as usize)),
        font,
        p.success,
    );
    a.left()
}

/// Paint a pull request row (its background is drawn by `kit::list`).
pub fn paint_pr_row(
    ui: &Ui,
    rect: Rect,
    pr: &PrSummary,
    requested: bool,
    selected: bool,
    now: i64,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let (y1, y2, y3) = (rect.top() + 16.0, rect.top() + 36.0, rect.top() + 57.0);
    icons::paint(
        painter,
        Icon::PullRequest,
        pos2(rect.left() + 20.0, y1),
        14.0,
        pr_icon_color(&p, pr),
    );
    let x = rect.left() + 36.0;
    let right = rect.right() - 12.0;
    let meta = rel(pr.updated, now);
    let mut title_right = right;
    if !meta.is_empty() {
        let g = painter.layout_no_wrap(meta, FontId::proportional(size::SMALL), p.text_3);
        title_right -= g.size().x + 10.0;
        painter.galley(pos2(right - g.size().x, y1 - g.size().y / 2.0), g, p.text_3);
    }
    let title_c = if selected { p.text_strong } else { p.text };
    let g = truncated(
        painter,
        &pr.title,
        FontId::new(size::BODY, medium()),
        title_c,
        (title_right - x).max(20.0),
    );
    painter.galley(pos2(x, y1 - g.size().y / 2.0), g, title_c);
    // Facts line: number, author, branches; size on the right.
    let stat_left = paint_diffstat(painter, &p, right, y2, pr.additions, pr.deletions);
    let mut facts = format!(
        "#{}  ·  {}  ·  {} → {}",
        pr.number, pr.author, pr.head, pr.base
    );
    if pr.total_comments > 0 {
        facts.push_str(&format!(
            "  ·  {}",
            format::plural(pr.total_comments as usize, "comment", "comments")
        ));
    }
    let g = truncated(
        painter,
        &facts,
        FontId::proportional(size::SMALL),
        p.text_2,
        (stat_left - x - 10.0).max(20.0),
    );
    painter.galley(pos2(x, y2 - g.size().y / 2.0), g, p.text_2);
    // Badges, then labels.
    let badges = pr_badges(pr, requested);
    let mut cx = paint_badges_from(painter, &p, dark, x, y3, &badges, right);
    if !badges.is_empty() {
        cx += 4.0;
    }
    paint_labels_from(painter, &p, cx, y3, &pr.labels, right);
}

/// Label chips from `x` until `right`, then `+N` for the rest.
fn paint_labels_from(
    painter: &egui::Painter,
    p: &Palette,
    mut x: f32,
    cy: f32,
    labels: &[Label],
    right: f32,
) {
    for (i, l) in labels.iter().enumerate() {
        let w = label_chip_width(painter, l);
        let rest = labels.len() - i;
        let reserve = if rest > 1 { 34.0 } else { 0.0 };
        if x + w + reserve > right {
            painter.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                format!("+{rest}"),
                FontId::proportional(size::SMALL),
                p.text_3,
            );
            return;
        }
        x = paint_label_chip(painter, p, x, cy, l) + 4.0;
    }
}

/// Paint an issue row.
pub fn paint_issue_row(ui: &Ui, rect: Rect, issue: &Issue, selected: bool, now: i64) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let (y1, y2, y3) = (rect.top() + 16.0, rect.top() + 36.0, rect.top() + 57.0);
    let color = if issue.open {
        p.success
    } else if issue.state_reason == "not_planned" {
        p.text_3
    } else {
        p.purple
    };
    icons::paint(
        painter,
        Icon::Issue,
        pos2(rect.left() + 20.0, y1),
        14.0,
        color,
    );
    let x = rect.left() + 36.0;
    let right = rect.right() - 12.0;
    let meta = rel(issue.updated, now);
    let mut title_right = right;
    if !meta.is_empty() {
        let g = painter.layout_no_wrap(meta, FontId::proportional(size::SMALL), p.text_3);
        title_right -= g.size().x + 10.0;
        painter.galley(pos2(right - g.size().x, y1 - g.size().y / 2.0), g, p.text_3);
    }
    let title_c = if selected { p.text_strong } else { p.text };
    let g = truncated(
        painter,
        &issue.title,
        FontId::new(size::BODY, medium()),
        title_c,
        (title_right - x).max(20.0),
    );
    painter.galley(pos2(x, y1 - g.size().y / 2.0), g, title_c);
    let mut facts = format!(
        "#{}  ·  {}  ·  opened {}",
        issue.number,
        issue.author,
        rel(issue.created, now)
    );
    if issue.comments > 0 {
        facts.push_str(&format!(
            "  ·  {}",
            format::plural(issue.comments as usize, "comment", "comments")
        ));
    }
    if let Some(m) = &issue.milestone {
        facts.push_str(&format!("  ·  {m}"));
    }
    if !issue.assignees.is_empty() {
        facts.push_str(&format!("  ·  assigned to {}", issue.assignees.join(", ")));
    }
    let g = truncated(
        painter,
        &facts,
        FontId::proportional(size::SMALL),
        p.text_2,
        (right - x).max(20.0),
    );
    painter.galley(pos2(x, y2 - g.size().y / 2.0), g, p.text_2);
    let mut badges = Vec::new();
    if !issue.open {
        badges.push((issue.state_label().to_owned(), Tone::Neutral));
    }
    let mut cx = paint_badges_from(painter, &p, dark, x, y3, &badges, right);
    if !badges.is_empty() {
        cx += 4.0;
    }
    if issue.labels.is_empty() && badges.is_empty() {
        painter.text(
            pos2(x, y3),
            Align2::LEFT_CENTER,
            "No labels",
            FontId::proportional(size::SMALL),
            p.text_3,
        );
    }
    paint_labels_from(painter, &p, cx, y3, &issue.labels, right);
}

/// An avatar and a name on one line.
pub fn person(ui: &mut Ui, name: &str, strong: bool) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    crate::views::avatar(
        ui.painter(),
        r.center(),
        10.0,
        if name.is_empty() { "?" } else { name },
        dark,
    );
    let text = if name.is_empty() { "ghost" } else { name };
    let rt = RichText::new(text).color(if strong { p.text_strong } else { p.text });
    ui.label(if strong {
        rt.family(semibold())
    } else {
        rt.family(medium())
    });
}

/// A GitHub body as wrapped plain text, capped in length.
pub fn prose(ui: &mut Ui, body: &str, url: &str, actions: &mut Vec<crate::state::Action>) {
    let text = local::plain_text(body);
    if text.is_empty() {
        kit::muted(ui, "No description.");
        return;
    }
    if text.chars().count() > MAX_BODY {
        let cut: String = text.chars().take(MAX_BODY).collect();
        kit::body(ui, &cut);
        ui.horizontal(|ui| {
            kit::muted(ui, "Cut at 12,000 characters.");
            if !url.is_empty() && kit::link(ui, "Read the rest on GitHub").clicked() {
                actions.push(crate::state::Action::OpenUrl(url.to_owned()));
            }
        });
    } else {
        kit::body(ui, &text);
    }
}

/// Longest comment shown in a timeline or thread (bots post pages).
const MAX_COMMENT: usize = 3_000;

/// A comment as wrapped plain text: in threads and timelines.
pub fn comment_text(ui: &mut Ui, body: &str) {
    let p = *pal(ui);
    let text = local::plain_text(body);
    let total = text.chars().count();
    let shown: String = text.chars().take(MAX_COMMENT).collect();
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width().min(kit::PROSE));
        ui.add(ELabel::new(RichText::new(shown).size(size::BODY).color(p.text)).wrap());
        if total > MAX_COMMENT {
            kit::muted(
                ui,
                &format!(
                    "Cut here: {} more characters on GitHub.",
                    format::count(total - MAX_COMMENT)
                ),
            );
        }
    });
}

/// The pull request a commit belongs to: `(#N)` at the end of its
/// summary, a GitHub merge subject, or the head of a loaded pull request.
pub fn pr_for_commit(st: &State, oid: Oid, summary: &str) -> Option<u64> {
    if let Some(n) =
        local::trailing_pr_number(summary).or_else(|| local::merge_subject_pr_number(summary))
    {
        return Some(n);
    }
    let sha = oid.to_string();
    [&st.open, &st.closed]
        .into_iter()
        .filter_map(|f| f.data.as_ref())
        .flat_map(|l| l.items.iter().chain(&l.requested).chain(&l.mine))
        .find(|p| p.head_oid == sha)
        .map(|p| p.number)
        .or_else(|| {
            st.prs
                .values()
                .filter_map(|v| v.detail.data.as_ref())
                .find(|d| d.pr.head_oid == sha)
                .map(|d| d.pr.number)
        })
}

/// A one-line note with a spinner, for panes that are loading.
pub fn loading_block(ui: &mut Ui, text: &str, detail: &str) {
    ui.add_space(8.0);
    kit::loading(ui, text);
    if !detail.is_empty() {
        ui.add_space(2.0);
        kit::secondary(ui, detail);
    }
}

/// An error with a retry button; returns true when retry was clicked.
pub fn error_block(ui: &mut Ui, title: &str, error: &str) -> bool {
    let mut retry = false;
    kit::callout_with(ui, Tone::Danger, title, error, |ui| {
        ui.add_space(4.0);
        retry = crate::views::form::button(ui, "Retry", true).clicked();
    });
    retry
}
