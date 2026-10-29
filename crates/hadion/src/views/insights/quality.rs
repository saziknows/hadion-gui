//! Commits: hygiene and provenance.

use egui::Ui;

use hadion_core::insights::InsightsReport;
use hadion_core::insights::history::CommitNote;

use super::{Cell, CommitLine, Cx, commit_table, more, num, percent, sec, why};
use crate::format;
use crate::state::Action;
use crate::theme::pal;
use crate::views::kit::{self, Col, Stat, Tone};

fn line(c: &CommitNote) -> CommitLine<'_> {
    CommitLine {
        id: c.id,
        time: c.time,
        author: &c.author,
        summary: &c.summary,
    }
}

pub fn show(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let q = &rep.history.quality;
    kit::scroll_page(ui, "ins_commits", |ui| {
        let p = *pal(ui);
        let t = q.total.max(1) as u64;
        let share = |n: u32, label: &str| format!("{label} ({})", percent(n as u64, t));
        let merges_label = share(q.merges, "Merges");
        let bots_label = share(q.bot_commits, "Bot commits");
        let differ_label = share(q.author_committer_differ, "Author ≠ committer");
        kit::title(ui, "Hygiene");
        ui.add_space(4.0);
        super::stats(
            ui,
            &[
                Stat::new("Commits", format::count(q.total as usize))
                    .tip("Commits read by the history pass"),
                Stat::new(&merges_label, format::count(q.merges as usize)),
                Stat::new(
                    "With a body",
                    percent((q.total - q.no_body.min(q.total)) as u64, t),
                )
                .tip("Commits with a message body under the summary"),
                Stat::new("Issue references", percent(q.with_issue_ref as u64, t))
                    .tip("Messages referencing an issue or ticket"),
                Stat::new("Conventional", percent(q.conventional as u64, t))
                    .tip("Conventional Commits style (type(scope): …)"),
                Stat::new("Long summaries", format::count(q.long_summary as usize))
                    .tone_if(q.long_summary > 0, Tone::Warning)
                    .tip("Summary lines over 72 characters"),
                Stat::new("One-word summaries", format::count(q.one_word as usize))
                    .tone_if(q.one_word > 0, Tone::Warning),
                Stat::new("Throwaway messages", format::count(q.wip.len()))
                    .tone_if(!q.wip.is_empty(), Tone::Warning)
                    .tip("WIP, fix, asdf…"),
                Stat::new("Unsquashed fixups", format::count(q.fixups.len()))
                    .tone_if(!q.fixups.is_empty(), Tone::Warning)
                    .tip("fixup! / squash! commits that reached history"),
                Stat::new("Reverts", format::count(q.reverts.len())),
                Stat::new("Giant commits", format::count(q.giant.len()))
                    .tip("50 files or more, or 5,000 lines or more"),
                Stat::new("Median files", q.median_files.to_string())
                    .tip("Median files per non-merge commit"),
                Stat::new("Median lines", format::count(q.median_lines as usize))
                    .tip("Median lines per non-merge commit"),
            ],
        );
        if !q.conventional_types.is_empty() {
            kit::section(ui, "Conventional commit types", None);
            kit::count_bars(ui, &q.conventional_types, p.accent, 14);
        }
        if !q.merges_by_year.is_empty() {
            kit::section(ui, "Merge vs rebase style by year", None);
            kit::secondary(
                ui,
                "Share of merge commits per year: high means merge-based integration, near zero means rebase or squash.",
            );
            for (y, m, total) in &q.merges_by_year {
                kit::bar_row(
                    ui,
                    &y.to_string(),
                    *m as f32 / (*total).max(1) as f32,
                    &format!(
                        "{} of {}",
                        format::count(*m as usize),
                        format::count(*total as usize)
                    ),
                    p.accent,
                );
            }
        }
        if !q.conventional_by_year.is_empty() && q.conventional > 0 {
            kit::section(ui, "Conventional commits adoption by year", None);
            for (y, c, total) in &q.conventional_by_year {
                kit::bar_row(
                    ui,
                    &y.to_string(),
                    *c as f32 / (*total).max(1) as f32,
                    &format!(
                        "{} of {}",
                        format::count(*c as usize),
                        format::count(*total as usize)
                    ),
                    p.accent,
                );
            }
        }

        notes(ui, cx, "ins_wip", "Throwaway messages", &q.wip, actions);
        if !q.wip.is_empty() {
            why(
                ui,
                "Messages like these say nothing to the next reader, and git bisect and blame land on them. Squash them before merging, or amend the message.",
            );
        }
        notes(
            ui,
            cx,
            "ins_fixups",
            "fixup! and squash! commits never squashed",
            &q.fixups,
            actions,
        );
        if !q.fixups.is_empty() {
            why(
                ui,
                "fixup! and squash! commits are meant to be folded by git rebase -i --autosquash before merging. These made it into history as they were.",
            );
        }

        kit::section(ui, "Reverts", Some(q.reverts.len()));
        if q.reverts.is_empty() {
            kit::muted(ui, "No revert commits.");
        } else {
            let rows: Vec<(CommitLine, Vec<Cell>)> = q
                .reverts
                .iter()
                .take(60)
                .map(|r| {
                    (
                        line(&r.commit),
                        vec![
                            Cell::Mono(hadion_core::repo::short_id(r.reverts)),
                            super::badges(&[(
                                if r.double { "Revert of a revert" } else { "" },
                                Tone::Warning,
                            )]),
                        ],
                    )
                })
                .collect();
            commit_table(
                ui,
                cx,
                "ins_reverts",
                &[Col::text("Reverts"), Col::text("")],
                0,
                rows,
                actions,
            );
            more(ui, q.reverts.len(), 60);
        }

        kit::section(
            ui,
            "Giant commits (50 files or 5,000 lines and more)",
            Some(q.giant.len()),
        );
        if q.giant.is_empty() {
            kit::muted(ui, "No giant commits.");
        } else {
            let rows: Vec<(CommitLine, Vec<Cell>)> = q
                .giant
                .iter()
                .take(40)
                .map(|g| {
                    (
                        line(&g.commit),
                        vec![
                            num(g.files as usize),
                            num(g.lines as usize),
                            super::badges(&[(
                                if g.is_merge { "Merge" } else { "" },
                                Tone::Neutral,
                            )]),
                        ],
                    )
                })
                .collect();
            commit_table(
                ui,
                cx,
                "ins_giant",
                &[Col::num("Files"), Col::num("Lines"), Col::text("")],
                0,
                rows,
                actions,
            );
            more(ui, q.giant.len(), 40);
        }

        // Provenance.
        ui.add_space(28.0);
        kit::title(ui, "Provenance");
        ui.add_space(4.0);
        let sig = q.signatures.as_ref();
        let sig_tone = sig.map_or(Tone::Neutral, |s| {
            if s.bad > 0 {
                Tone::Danger
            } else if s.good + s.unknown == 0 {
                Tone::Warning
            } else {
                Tone::Neutral
            }
        });
        super::stats(
            ui,
            &[
                Stat::new(&bots_label, format::count(q.bot_commits as usize))
                    .tip("Author or committer looks like a bot (dependabot, renovate, github-actions, [bot]…)"),
                Stat::new(
                    &differ_label,
                    format::count(q.author_committer_differ as usize),
                )
                .tip("Rebased, cherry-picked, or merged through the web UI by someone else"),
                Stat::new("Date anomalies", format::count(q.date_anomalies.len()))
                    .tone_if(!q.date_anomalies.is_empty(), Tone::Warning)
                    .tip("Future, pre-2000, rebased, backdated or offset-jump dates"),
                Stat::new(
                    "Signed",
                    sig.map(|s| format!("{} of {}", s.good + s.unknown, s.checked))
                        .unwrap_or_else(|| "–".into()),
                )
                .tone(sig_tone)
                .tip("Newest commits with a valid signature (G) or a valid but untrusted key (U), of those checked with %G?"),
                Stat::new(
                    "Bad signatures",
                    sig.map(|s| s.bad.to_string()).unwrap_or_else(|| "–".into()),
                )
                .tone_if(sig.is_some_and(|s| s.bad > 0), Tone::Danger)
                .tip("Signatures that do not verify"),
            ],
        );
        if !q.bots.is_empty() {
            kit::section(ui, "Bots", Some(q.bots.len()));
            kit::count_bars(ui, &q.bots, p.accent, 10);
        }
        if let Some(s) = sig {
            kit::section(
                ui,
                "Signatures of the newest commits",
                Some(s.checked as usize),
            );
            let rows = [
                ("Good (G)", s.good, p.success),
                ("Good, untrusted key (U)", s.unknown, p.accent),
                ("Expired (X/Y)", s.expired, p.warning),
                ("Revoked (R)", s.revoked, p.danger),
                ("Bad (B)", s.bad, p.danger),
                ("Cannot check (E)", s.error, p.warning),
                ("Unsigned (N)", s.none, p.text_3),
            ];
            for (label, n, color) in rows {
                if n > 0 {
                    kit::bar_row(
                        ui,
                        label,
                        n as f32 / s.checked.max(1) as f32,
                        &format::count(n as usize),
                        color,
                    );
                }
            }
            if s.error > 0 {
                kit::secondary(
                    ui,
                    "E means git could not verify: the key is not in the local keyring, or gpg / ssh-keygen with an allowed-signers file is not set up on this machine.",
                );
            }
            if !s.by_author.is_empty() {
                super::group(ui, "Signed commits per author", Some(s.by_author.len()));
                for (a, signed, total) in s.by_author.iter().take(15) {
                    kit::bar_row(
                        ui,
                        a,
                        *signed as f32 / (*total).max(1) as f32,
                        &format!("{signed} of {total}"),
                        p.success,
                    );
                }
                more(ui, s.by_author.len(), 15);
            }
        }

        kit::section(ui, "Date anomalies", Some(q.date_anomalies.len()));
        if q.date_anomalies.is_empty() {
            kit::muted(ui, "Author and committer dates look plausible everywhere.");
        } else {
            kit::kv(ui, 110.0, |kv| {
                kv.text("Future", "dated after now");
                kv.text("Epoch", "dated before 2000");
                kv.text("Rebased", "committed more than a day after authoring");
                kv.text("Backdated", "committed before authoring");
                kv.text(
                    "Offset jump",
                    "the author's UTC offset differs from their usual one by 5 hours or more",
                );
            });
            if !q.anomaly_counts.is_empty() {
                let items: Vec<(String, u32)> = q
                    .anomaly_counts
                    .iter()
                    .map(|(k, n)| (sentence(k), *n))
                    .collect();
                kit::count_bars(ui, &items, p.warning, 8);
            }
            let rows: Vec<(CommitLine, Vec<Cell>)> = q
                .date_anomalies
                .iter()
                .take(80)
                .map(|a| {
                    (
                        line(&a.commit),
                        vec![Cell::Tone(sentence(a.kind), Tone::Warning), sec(&a.detail)],
                    )
                })
                .collect();
            commit_table(
                ui,
                cx,
                "ins_anomalies",
                &[Col::text("Kind"), Col::text("Detail")],
                1,
                rows,
                actions,
            );
            more(ui, q.date_anomalies.len(), 80);
        }
        ui.add_space(12.0);
    });
}

/// "offset-jump" → "Offset jump".
fn sentence(s: &str) -> String {
    let s = s.replace('-', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn notes(
    ui: &mut Ui,
    cx: &Cx,
    id: &str,
    title: &str,
    notes: &[CommitNote],
    actions: &mut Vec<Action>,
) {
    kit::section(ui, title, Some(notes.len()));
    if notes.is_empty() {
        kit::muted(ui, "None.");
        return;
    }
    let rows: Vec<(CommitLine, Vec<Cell>)> = notes
        .iter()
        .take(60)
        .map(|n| (line(n), Vec::new()))
        .collect();
    commit_table(ui, cx, id, &[], 0, rows, actions);
    more(ui, notes.len(), 60);
}
