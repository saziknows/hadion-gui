//! What the AI area draws in other views: the buttons and results in the
//! WIP panel, the new pull request form, a pull request's conversation, the
//! commit panel and menu, and the "Explain this failure" window over the
//! Actions log viewer.

use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, CornerRadius, FontFamily, FontId, Frame, Label, Layout, Margin, RichText,
    Stroke, Ui, vec2,
};
use gitvis_core::Commit;
use gitvis_core::devops::ai::cli::{self, CliErrorKind};
use gitvis_core::devops::ai::output::{self, Flaky};
use gitvis_core::devops::ai::{CliModel, Failure, Feature, Model, Provider, dollars};

use super::{Act, Ask, Phase, Prefs, State, Target, control, session};
use crate::devops::DevOpsAction;
use crate::devops::hooks::{HookCx, LogHookCx};
use crate::state::{Action, Page, Selection};
use crate::theme::{Tone, medium, pal, radius, semibold, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::{form, kit, menus};

/// Where requests go now: `Some(true)` Claude Code, `Some(false)` the API,
/// `None` not known yet (Auto before Claude Code has been looked at).
pub(super) fn goes_cli(prefs: &Prefs) -> Option<bool> {
    match prefs.provider_now() {
        Provider::ClaudeCode => Some(true),
        Provider::Api => Some(false),
        Provider::Auto => session::with(|s| s.cli.status().map(cli::Status::plan_ready)),
    }
}

/// `Claude Code · low effort`, `Opus 5.5 · low effort`, `Auto`.
fn caption(prefs: &Prefs, f: Feature) -> String {
    match goes_cli(prefs) {
        Some(true) => match prefs.cli_model_now() {
            CliModel::Default => format!("Claude Code · {} effort", prefs.cli_effort(f).as_str()),
            m => format!(
                "Claude Code · {} · {} effort",
                m.label(),
                prefs.cli_effort(f).as_str()
            ),
        },
        Some(false) => format!(
            "{} · {} effort",
            prefs.model_now().short(),
            prefs.effort(f).as_str()
        ),
        None => "Auto: Claude Code or the API".to_owned(),
    }
}

fn open_settings(actions: &mut Vec<Action>) {
    actions.push(Action::DevOps(DevOpsAction::SetSettingsSection(
        crate::devops::settings_page::Section::Ai,
    )));
    actions.push(Action::ShowPage(Page::Settings));
}

/// Two lines: who answered and how (`Claude Opus 5.5 · through Claude
/// Code (Claude Max plan)`), then the numbers (`518 tokens in, 62 out ·
/// 5.9 s · covered by your plan`).
fn usage_line(a: &Ask) -> String {
    let Some(r) = a.reply() else {
        return String::new();
    };
    let ans = &r.done.answer;
    let model = if ans.model.is_empty() {
        a.engine()
    } else {
        Model::label_of(&ans.model)
    };
    let who = match &r.done.cli {
        Some(run) => format!("{model} · through Claude Code ({})", run.account),
        None => format!("{model} · through the Claude API"),
    };
    let u = &ans.usage;
    let mut numbers = format!(
        "{} tokens in, {} out",
        crate::format::count(u.input_tokens as usize),
        crate::format::count(u.output_tokens as usize)
    );
    if u.cache_creation_input_tokens + u.cache_read_input_tokens > 0 {
        numbers.push_str(&format!(
            " · cache: {} written, {} read",
            crate::format::count(u.cache_creation_input_tokens as usize),
            crate::format::count(u.cache_read_input_tokens as usize)
        ));
    }
    numbers.push_str(&format!(" · {}", crate::format::millis(r.done.elapsed_ms)));
    match &r.done.cli {
        Some(run) => numbers.push_str(&format!(
            " · covered by your plan{}",
            run.cost_estimate
                .filter(|c| *c > 0.0)
                .map(|c| format!(" (Claude Code's list-price estimate: {})", dollars(c)))
                .unwrap_or_default()
        )),
        None => numbers.push_str(&format!(" · about {}", dollars(r.cost))),
    }
    format!("{who}\n{numbers}")
}

/// "Claude Opus 5.5 declined; continued on Claude Opus 5" lines.
fn fallback_notes(a: &Ask) -> Vec<String> {
    a.reply()
        .map(|r| {
            r.done
                .answer
                .fallbacks
                .iter()
                .map(|f| {
                    format!(
                        "{} declined; continued on {}",
                        Model::label_of(&f.from),
                        Model::label_of(&f.to)
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Small secondary line.
fn note(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_2;
    ui.add(Label::new(RichText::new(text).size(size::SMALL).color(c)).wrap());
}

/// Live text of a running request, whether the model is still thinking,
/// the seconds so far and a note (Claude Code retrying after a limit).
fn live_text(a: &Ask) -> Option<(String, bool, u64, Option<String>)> {
    let Phase::Running { live, .. } = &a.phase else {
        return None;
    };
    let l = live.lock().unwrap_or_else(|e| e.into_inner());
    Some((
        l.answer.text.clone(),
        l.answer.text.is_empty(),
        l.started.elapsed().as_secs(),
        l.note.clone(),
    ))
}

/// Spinner, phase, elapsed time and Stop, for a request in flight.
/// Returns the live text (empty while thinking). `short` phrases fit the
/// narrow WIP panel.
fn progress_row(ui: &mut Ui, a: &Ask, short: bool, actions: &mut Vec<Action>) -> String {
    let p = *pal(ui);
    ui.ctx().request_repaint_after(Duration::from_millis(120));
    let mut tip: Option<String> = None;
    let (text, phase) = match &a.phase {
        Phase::Preparing => (
            String::new(),
            if short {
                "Reading what to send…".to_owned()
            } else {
                "Gathering and checking what to send…".to_owned()
            },
        ),
        Phase::Confirming => (
            String::new(),
            if short {
                "Waiting to send".to_owned()
            } else {
                "Waiting for your confirmation".to_owned()
            },
        ),
        Phase::Running { .. } => {
            let (text, thinking, secs, note) = live_text(a).unwrap_or_default();
            let who = a.engine();
            if short {
                tip = note.clone();
            }
            let phase = match (thinking, short) {
                _ if note.is_some() && short => format!("Retrying · {secs} s"),
                _ if note.is_some() => format!("{} · {secs} s", note.unwrap_or_default()),
                (true, true) => format!("Thinking · {secs} s"),
                (true, false) => format!("{who} is thinking · {secs} s"),
                (false, true) => format!(
                    "Writing · {} chars · {secs} s",
                    crate::format::count(text.chars().count())
                ),
                (false, false) => format!(
                    "{who} is writing · {} characters · {secs} s",
                    crate::format::count(text.chars().count())
                ),
            };
            (text, phase)
        }
        Phase::Done(_) => return String::new(),
    };
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 24.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            // Buttons first (rightmost), so the phase text gets what is left.
            ui.spacing_mut().item_spacing.x = 4.0;
            match a.phase {
                Phase::Confirming => {
                    if IconButton::text("Cancel").compact().show(ui).clicked() {
                        actions.push(Act::Cancel(a.target.clone()).into());
                    }
                    if IconButton::text(if short { "Review" } else { "Review and send" })
                        .compact()
                        .show(ui)
                        .on_hover_text("Show the confirmation again")
                        .clicked()
                    {
                        actions.push(Act::Reconfirm(a.target.clone()).into());
                    }
                }
                Phase::Running { .. } => {
                    let stop = IconButton::new(Icon::Stop, "Stop")
                        .compact()
                        .show(ui)
                        .on_hover_text("Stop the request; nothing is filled in");
                    if stop.clicked() {
                        actions.push(Act::Cancel(a.target.clone()).into());
                    }
                }
                _ => {}
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if !matches!(a.phase, Phase::Confirming) {
                    ui.add(egui::Spinner::new().size(13.0).color(p.accent));
                }
                let resp = ui.add(
                    Label::new(RichText::new(&phase).size(size::SMALL).color(p.text_2)).truncate(),
                );
                // In the narrow panel the retry note and the text so far are
                // only in the tooltip.
                if let Some(t) = tip.take() {
                    resp.on_hover_text(t);
                } else if short && !text.is_empty() {
                    let tail: String = {
                        let chars: Vec<char> = text.chars().collect();
                        chars[chars.len().saturating_sub(400)..].iter().collect()
                    };
                    resp.on_hover_text(tail);
                }
            });
        },
    );
    text
}

/// The full text of a compact notice, behind an info button.
fn details_button(ui: &mut Ui, text: &str) {
    if !text.trim().is_empty() {
        IconButton::new(Icon::Info, "Details")
            .compact()
            .show(ui)
            .on_hover_text(text);
    }
}

/// A failure as a callout with what to do about it. `compact` (the WIP
/// panel): a one-line title, the text behind a Details button.
fn failure(ui: &mut Ui, a: &Ask, f: &Failure, compact: bool, actions: &mut Vec<Action>) {
    let t = a.target.clone();
    let cli_title = match f {
        Failure::ClaudeCode(e) => e.kind.title(),
        _ => "",
    };
    let short_api = match f {
        Failure::Api(e) => {
            let mut s = if e.kind.is_empty() {
                "API error".to_owned()
            } else {
                e.kind.replace('_', " ")
            };
            if let Some(st) = e.status.filter(|s| *s != 200) {
                s.push_str(&format!(" (HTTP {st})"));
            }
            s
        }
        _ => String::new(),
    };
    let (tone, title) = match f {
        Failure::NoKey => (Tone::Warning, "No Claude API key"),
        Failure::NoCurl => (Tone::Warning, "curl is not installed"),
        Failure::Cancelled => (Tone::Neutral, "Stopped"),
        Failure::Prepare(_) => (Tone::Warning, "Could not gather what to send"),
        Failure::Transport(_) => (Tone::Danger, "The request did not get through"),
        Failure::Api(_) if compact => (Tone::Danger, short_api.as_str()),
        Failure::Api(_) => (Tone::Danger, "The Claude API returned an error"),
        Failure::Incomplete(_) => (Tone::Danger, "The answer was cut off"),
        Failure::NoClaudeCode => (Tone::Warning, "Claude Code is not installed"),
        Failure::NoProvider(_) => (Tone::Warning, "Set up Claude Code or an API key"),
        Failure::ClaudeCode(e) => (
            match e.kind {
                CliErrorKind::NotSignedIn
                | CliErrorKind::LoginExpired
                | CliErrorKind::SessionLimit
                | CliErrorKind::WeeklyLimit
                | CliErrorKind::TooOld
                | CliErrorKind::ModelNotFound => Tone::Warning,
                _ => Tone::Danger,
            },
            cli_title,
        ),
    };
    let mut body = match f {
        Failure::Cancelled => "Nothing was filled in.".to_owned(),
        Failure::NoCurl => {
            "gitvis sends requests with curl, which reads the key from standard input so it never appears on a command line. curl ships with Windows 10 and 11 (C:\\Windows\\System32\\curl.exe) and with Git for Windows; otherwise: winget install cURL.cURL".to_owned()
        }
        Failure::NoClaudeCode => format!(
            "gitvis looks for claude.exe in ~/.local/bin, on PATH and next to an npm claude.cmd. Install it with one of:\n{}\nthen sign in (`{}` in a terminal), or choose the Claude API in Settings → AI assist.",
            cli::INSTALL_HINTS
                .iter()
                .map(|(how, cmd)| format!("{how}: {cmd}"))
                .collect::<Vec<_>>()
                .join("\n"),
            cli::SIGN_IN
        ),
        other => other.message(),
    };
    if let Failure::Api(e) = f
        && !e.hint().is_empty()
    {
        body.push('\n');
        body.push_str(e.hint());
    }
    if let Failure::ClaudeCode(e) = f
        && !e.kind.next_step().is_empty()
    {
        body.push('\n');
        body.push_str(e.kind.next_step());
    }
    let wait = control::retry_wait(a);
    let shown = if compact { "" } else { body.as_str() };
    kit::callout_with(ui, tone, title, shown, |ui| {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if compact {
                details_button(ui, &body);
            }
            match f {
                Failure::NoKey => {
                    if IconButton::new(
                        Icon::Key,
                        if compact {
                            "Settings"
                        } else {
                            "Open AI settings"
                        },
                    )
                    .compact()
                    .show(ui)
                    .clicked()
                    {
                        open_settings(actions);
                    }
                }
                Failure::NoCurl => {
                    if IconButton::new(Icon::Rerun, "Check again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::Start(t.clone()).into());
                    }
                }
                Failure::NoClaudeCode | Failure::NoProvider(_) => {
                    if IconButton::new(Icon::Gear, "Settings")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        open_settings(actions);
                    }
                    if IconButton::new(Icon::Rerun, "Check again")
                        .compact()
                        .show(ui)
                        .on_hover_text("Look for Claude Code and its sign-in again, then ask")
                        .clicked()
                    {
                        actions.push(Act::Start(t.clone()).into());
                    }
                }
                Failure::ClaudeCode(e)
                    if e.kind.needs_sign_in() || e.kind == CliErrorKind::TooOld =>
                {
                    let cmd = if e.kind == CliErrorKind::TooOld {
                        "claude update"
                    } else {
                        cli::SIGN_IN
                    };
                    if IconButton::new(Icon::Copy, &format!("Copy `{cmd}`"))
                        .compact()
                        .show(ui)
                        .on_hover_text("Run it in a terminal (gitvis never runs it for you)")
                        .clicked()
                    {
                        actions.push(Action::Copy(cmd.to_owned()));
                    }
                    if IconButton::new(Icon::Terminal, "Terminal")
                        .compact()
                        .show(ui)
                        .on_hover_text("Open the terminal panel, to paste and run it there")
                        .clicked()
                    {
                        actions.push(Action::ToggleTerminal);
                    }
                    if IconButton::new(Icon::Rerun, "Check again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::Start(t.clone()).into());
                    }
                }
                Failure::ClaudeCode(e)
                    if matches!(
                        e.kind,
                        CliErrorKind::SessionLimit
                            | CliErrorKind::WeeklyLimit
                            | CliErrorKind::ModelNotFound
                            | CliErrorKind::CreditTooLow
                            | CliErrorKind::ToolsEnabled
                    ) =>
                {
                    if IconButton::new(Icon::Gear, "Settings")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        open_settings(actions);
                    }
                }
                Failure::Api(e) if e.kind == "authentication_error" => {
                    if IconButton::new(
                        Icon::Key,
                        if compact {
                            "Settings"
                        } else {
                            "Open AI settings"
                        },
                    )
                    .compact()
                    .show(ui)
                    .clicked()
                    {
                        open_settings(actions);
                    }
                }
                _ => {
                    let label = match wait {
                        Some(s) => format!("Retry in {s} s"),
                        None => "Try again".to_owned(),
                    };
                    if IconButton::new(Icon::Rerun, &label)
                        .compact()
                        .enabled(wait.is_none())
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::Start(t.clone()).into());
                    }
                    if wait.is_some() {
                        ui.ctx().request_repaint_after(Duration::from_millis(500));
                    }
                }
            }
            if IconButton::text("Dismiss").compact().show(ui).clicked() {
                actions.push(Act::Dismiss(t.clone()).into());
            }
        });
    });
}

/// A refusal or a cut-off answer; returns true when it drew one.
fn stop_reason_notice(
    ui: &mut Ui,
    a: &Ask,
    actions: &mut Vec<Action>,
    can_use: bool,
    compact: bool,
) -> bool {
    let Some(r) = a.reply() else {
        return false;
    };
    let ans = &r.done.answer;
    if ans.refused() {
        let mut body = format!(
            "Category: {}.",
            ans.refusal_category.as_deref().unwrap_or("not given")
        );
        if let Some(e) = &ans.refusal_explanation {
            body.push(' ');
            body.push_str(e);
        }
        body.push_str(" Nothing was filled in; any partial text was discarded.");
        for n in fallback_notes(a) {
            body.push('\n');
            body.push_str(&n);
        }
        let title = if compact {
            format!(
                "Claude declined ({})",
                ans.refusal_category.as_deref().unwrap_or("no category")
            )
        } else {
            "Claude declined this request".to_owned()
        };
        kit::callout_with(
            ui,
            Tone::Warning,
            &title,
            if compact { "" } else { &body },
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if compact {
                        details_button(ui, &body);
                    }
                    if IconButton::text("Dismiss").compact().show(ui).clicked() {
                        actions.push(Act::Dismiss(a.target.clone()).into());
                    }
                });
            },
        );
        return true;
    }
    if ans.hit_max_tokens() {
        kit::callout_with(
            ui,
            Tone::Warning,
            "The answer hit the 16,000-token limit",
            if compact {
                ""
            } else {
                "It stops mid-way, so it was not filled in. Use it anyway, or try again."
            },
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if can_use
                        && IconButton::text("Use it anyway")
                            .compact()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(
                            Act::Use {
                                target: a.target.clone(),
                                id: a.id,
                            }
                            .into(),
                        );
                    }
                    if IconButton::new(Icon::Rerun, "Try again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::Start(a.target.clone()).into());
                    }
                });
            },
        );
        return true;
    }
    false
}

/// Inline Markdown: `code` in monospace, **bold** in the semibold weight.
fn inline_job(ui: &Ui, text: &str, color: egui::Color32, width: f32) -> LayoutJob {
    let p = *pal(ui);
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let body = FontId::proportional(size::BODY);
    let mono = FontId::monospace(size::MONO);
    let bold = FontId::new(size::BODY, semibold());
    let mut code = false;
    let mut strong = false;
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, code: bool, strong: bool| {
        if buf.is_empty() {
            return;
        }
        let (font, c) = if code {
            (mono.clone(), p.text_strong)
        } else if strong {
            (bold.clone(), p.text_strong)
        } else {
            (body.clone(), color)
        };
        job.append(
            buf,
            0.0,
            TextFormat {
                font_id: font,
                color: c,
                ..Default::default()
            },
        );
        buf.clear();
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            flush(&mut job, &mut buf, code, strong);
            code = !code;
        } else if !code && c == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut job, &mut buf, code, strong);
            strong = !strong;
            i += 1;
        } else {
            buf.push(c);
        }
        i += 1;
    }
    flush(&mut job, &mut buf, code, strong);
    job
}

/// A small Markdown renderer for answers: headings, bullets, quotes, code
/// fences, paragraphs.
pub fn markdown(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let width = ui.available_width().min(kit::PROSE);
    let mut fence: Option<String> = None;
    let mut para = String::new();
    let flush_para = |ui: &mut Ui, para: &mut String| {
        if para.trim().is_empty() {
            para.clear();
            return;
        }
        let job = inline_job(ui, para.trim(), p.text, width);
        ui.add(Label::new(job).wrap());
        para.clear();
    };
    for line in text.lines() {
        if let Some(code) = fence.as_mut() {
            if line.trim_start().starts_with("```") {
                let c = std::mem::take(code);
                fence = None;
                kit::code_block(ui, c.trim_end(), actions);
            } else {
                code.push_str(line);
                code.push('\n');
            }
            continue;
        }
        let t = line.trim();
        if t.starts_with("```") {
            flush_para(ui, &mut para);
            fence = Some(String::new());
            continue;
        }
        if t.is_empty() {
            flush_para(ui, &mut para);
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            flush_para(ui, &mut para);
            ui.add_space(space::XS);
            ui.label(
                RichText::new(h.trim_start_matches('#').trim())
                    .size(size::BODY)
                    .family(semibold())
                    .color(p.text_strong),
            );
            continue;
        }
        let bullet = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("• "));
        let numbered = t
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if bullet.is_some() || numbered.is_some() {
            flush_para(ui, &mut para);
            let (mark, rest) = match (bullet, numbered) {
                (Some(b), _) => ("•".to_owned(), b),
                (None, Some((n, r))) => (format!("{n}."), r),
                _ => unreachable!(),
            };
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_sized(
                    vec2(16.0, 18.0),
                    Label::new(RichText::new(mark).size(size::BODY).color(p.text_2)),
                );
                let job = inline_job(ui, rest, p.text, (width - 22.0).max(80.0));
                ui.add(Label::new(job).wrap());
            });
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            flush_para(ui, &mut para);
            let r = Frame::NONE
                .inner_margin(Margin {
                    left: 10,
                    right: 4,
                    top: 1,
                    bottom: 1,
                })
                .show(ui, |ui| {
                    ui.add(
                        Label::new(
                            RichText::new(q.trim_start())
                                .family(FontFamily::Monospace)
                                .size(size::MONO)
                                .color(p.text_2),
                        )
                        .wrap(),
                    );
                })
                .response;
            ui.painter().vline(
                r.rect.left() + 2.0,
                r.rect.y_range(),
                Stroke::new(2.0, p.border_strong),
            );
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(t);
    }
    if let Some(code) = fence {
        kit::code_block(ui, code.trim_end(), actions);
    }
    flush_para(ui, &mut para);
}

/// The status of a request whose answer is filled into a field (the WIP
/// commit message, the new pull request form).
/// `compact` (the WIP panel, whose commit box has a fixed height): the
/// finished state is one line, its numbers in the line itself.
fn fill_status(ui: &mut Ui, a: &Ask, done_text: &str, compact: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    match &a.phase {
        Phase::Preparing | Phase::Confirming | Phase::Running { .. } => {
            let text = progress_row(ui, a, compact, actions);
            if !text.is_empty() && !compact {
                Frame::NONE
                    .fill(p.raised)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(radius::M))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt(("ai_live", a.id))
                            .max_height(140.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.add(
                                    Label::new(
                                        RichText::new(&text).size(size::SMALL).color(p.text_2),
                                    )
                                    .wrap(),
                                );
                            });
                    });
            }
        }
        Phase::Done(Err(f)) => failure(ui, a, f, compact, actions),
        Phase::Done(Ok(r)) => {
            if stop_reason_notice(ui, a, actions, true, compact) {
                return;
            }
            if a.unapplied {
                let preview: String = match a.target {
                    Target::Wip => output::commit_message(&r.done.answer.text).message,
                    _ => {
                        let (t, b) = output::pr_description(&r.done.answer.text);
                        format!("{t}\n\n{b}")
                    }
                };
                form::card(ui, Tone::Neutral, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        RichText::new("Claude's answer (not filled in: you kept yours)")
                            .size(size::SMALL)
                            .family(medium())
                            .color(p.text_2),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt(("ai_unapplied", a.id))
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.add(
                                Label::new(
                                    RichText::new(&preview)
                                        .family(FontFamily::Monospace)
                                        .size(size::MONO)
                                        .color(p.text),
                                )
                                .wrap(),
                            );
                        });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if IconButton::new(Icon::Check, "Use it")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(
                                Act::Use {
                                    target: a.target.clone(),
                                    id: a.id,
                                }
                                .into(),
                            );
                        }
                        if IconButton::new(Icon::Copy, "Copy")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::Copy(preview.clone()));
                        }
                        if IconButton::text("Dismiss").compact().show(ui).clicked() {
                            actions.push(Act::Dismiss(a.target.clone()).into());
                        }
                    });
                    note(ui, &usage_line(a));
                });
                return;
            }
            // Filled in: one line with what it cost.
            let done_text = if compact {
                short_usage(a)
            } else {
                done_text.to_owned()
            };
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 22.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    crate::views::icons::show(ui, Icon::Check, 12.0, p.success);
                    ui.add(
                        Label::new(
                            RichText::new(&done_text)
                                .size(size::SMALL)
                                .family(medium())
                                .color(p.text),
                        )
                        .truncate(),
                    )
                    .on_hover_text(usage_line(a));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Hide this line")
                            .clicked()
                        {
                            actions.push(Act::Dismiss(a.target.clone()).into());
                        }
                    });
                },
            );
            if !compact {
                note(ui, &usage_line(a));
            }
            for n in fallback_notes(a) {
                note(ui, &n);
            }
            if a.target == Target::Wip {
                for w in output::commit_message(&r.done.answer.text).warnings {
                    kit::callout(ui, Tone::Warning, "", &w);
                }
            }
            if let Some(p) = &a.prepared
                && !p.redactions.is_empty()
                && !compact
            {
                note(
                    ui,
                    &format!("Redacted before sending: {}", p.redactions.summary()),
                );
            }
        }
    }
}

/// `Opus 5.5 · about $0.0042 · 2 redacted`, `Claude Code · your plan`
/// (details in the tooltip).
fn short_usage(a: &Ask) -> String {
    let Some(r) = a.reply() else {
        return String::new();
    };
    let ans = &r.done.answer;
    let model = if ans.model.is_empty() {
        a.engine()
    } else {
        Model::label_of(&ans.model)
            .trim_start_matches("Claude ")
            .to_owned()
    };
    let mut s = if r.done.cli.is_some() {
        "Claude Code · your plan".to_owned()
    } else {
        format!("{model} · about {}", dollars(r.cost))
    };
    if let Some(p) = &a.prepared {
        let n = p.redactions.values() + p.redactions.withheld.len();
        if n > 0 {
            s.push_str(&format!(" · {n} redacted"));
        }
    }
    s
}

/// Above the commit message box.
pub fn wip_tools(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    // The commit box has a fixed height (150 px in `views/details.rs`), so
    // everything here stays on one 24 px row except errors and an answer
    // waiting for "Replace your message?".
    let p = *pal(ui);
    let prefs = &h.settings.ext.ai;
    let st: &State = &h.devops.ai;
    let staged = !h.snap.status.staged.is_empty();
    let ask = st.asks.get(&Target::Wip);
    let in_flight = ask.is_some_and(|a| a.running() || matches!(a.phase, Phase::Confirming));
    if let Some(a) = ask.filter(|_| in_flight) {
        progress_row(ui, a, true, actions);
        ui.add_space(2.0);
        return;
    }
    let done_line = ask.filter(|a| a.applied).map(short_usage);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 24.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let enabled = staged && !h.busy;
            let resp = IconButton::new(Icon::Edit, "Write with Claude")
                .compact()
                .enabled(enabled)
                .show(ui);
            let resp = if !staged {
                resp.on_disabled_hover_text("Stage some changes first")
            } else {
                resp.on_hover_text(
                    "Claude writes the commit message from the staged diff and your recent subjects",
                )
            };
            if resp.clicked() {
                actions.push(Act::Start(Target::Wip).into());
            }
            match (&done_line, ask) {
                (Some(line), Some(a)) => {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Hide this line")
                            .clicked()
                        {
                            actions.push(Act::Dismiss(Target::Wip).into());
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            crate::views::icons::show(ui, Icon::Check, 12.0, p.success);
                            let mut tip = usage_line(a);
                            if let Some(pr) = &a.prepared
                                && !pr.redactions.is_empty()
                            {
                                tip.push_str(&format!(
                                    "
Redacted before sending: {}",
                                    pr.redactions.summary()
                                ));
                            }
                            ui.add(
                                Label::new(
                                    RichText::new(line).size(size::SMALL).color(p.text_2),
                                )
                                .truncate(),
                            )
                            .on_hover_text(tip);
                        });
                    });
                }
                _ => {
                    ui.add(
                        Label::new(
                            RichText::new(caption(prefs, Feature::CommitMessage))
                                .size(size::SMALL)
                                .color(p.text_3),
                        )
                        .truncate(),
                    );
                }
            }
        },
    );
    if let Some(a) = ask.filter(|a| !a.applied) {
        let what = format!("Message written by {}", a.engine());
        fill_status(ui, a, &what, true, actions);
    } else if let Some(a) = ask
        && let Some(r) = a.reply()
    {
        for w in output::commit_message(&r.done.answer.text).warnings {
            kit::callout(ui, Tone::Warning, "", &w);
        }
        for n in fallback_notes(a) {
            note(ui, &n);
        }
    }
    ui.add_space(2.0);
}

/// Above the new pull request form.
pub fn new_pr_tools(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let prefs = &h.settings.ext.ai;
    let ask = h.devops.ai.asks.get(&Target::NewPr);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 24,
            top: 12,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_max_width(kit::DETAIL_MAX);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 30.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let busy =
                        ask.is_some_and(|a| a.running() || matches!(a.phase, Phase::Confirming));
                    if IconButton::new(Icon::Edit, "Write title and description with Claude")
                        .enabled(!busy)
                        .show(ui)
                        .on_hover_text(
                            "Reads the commits between the branches (subjects and bodies), the diff stat and the template",
                        )
                        .clicked()
                    {
                        actions.push(Act::Start(Target::NewPr).into());
                    }
                    ui.label(
                        RichText::new(caption(prefs, Feature::PrDescription))
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                },
            );
            if let Some(a) = ask {
                ui.add_space(4.0);
                let what = format!("Title and description written by {}", a.engine());
                fill_status(ui, a, &what, false, actions);
            }
        });
}

/// A result card (pull request summary, commit explanation).
fn result_card(
    ui: &mut Ui,
    st: &State,
    a: &Ask,
    title: &str,
    actions: &mut Vec<Action>,
    badges: &[(String, Tone)],
) {
    let p = *pal(ui);
    let collapsed = st.collapsed.contains(&a.target);
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        // Buttons first (right to left), so a narrow panel cuts the title
        // instead of drawing the buttons over it.
        let wide = ui.available_width() > 520.0;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 24.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if matches!(a.phase, Phase::Done(_))
                    && IconButton::icon(Icon::Close)
                        .compact()
                        .show(ui)
                        .on_hover_text("Dismiss")
                        .clicked()
                {
                    actions.push(Act::Dismiss(a.target.clone()).into());
                }
                if let Some(r) = a.reply() {
                    if IconButton::icon(Icon::Copy)
                        .compact()
                        .show(ui)
                        .on_hover_text("Copy the answer")
                        .clicked()
                    {
                        actions.push(Action::Copy(r.done.answer.text.clone()));
                    }
                    let regen = if wide {
                        IconButton::new(Icon::Rerun, "Regenerate")
                    } else {
                        IconButton::icon(Icon::Rerun)
                    };
                    if regen
                        .compact()
                        .show(ui)
                        .on_hover_text("Ask again with the current settings")
                        .clicked()
                    {
                        actions.push(Act::Start(a.target.clone()).into());
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if matches!(a.phase, Phase::Done(Ok(_)))
                        && IconButton::icon(if collapsed {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronUp
                        })
                        .compact()
                        .show(ui)
                        .on_hover_text(if collapsed { "Show" } else { "Fold" })
                        .clicked()
                    {
                        actions.push(Act::ToggleCollapsed(a.target.clone()).into());
                    }
                    ui.add(
                        Label::new(
                            RichText::new(title)
                                .size(size::BODY)
                                .family(semibold())
                                .color(p.text_strong),
                        )
                        .truncate(),
                    );
                    if wide {
                        for (b, tone) in badges {
                            kit::badge(ui, b, *tone);
                        }
                    }
                });
            },
        );
        if !wide && !badges.is_empty() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (b, tone) in badges {
                    kit::badge(ui, b, *tone);
                }
            });
        }
        match &a.phase {
            Phase::Done(Err(f)) => failure(ui, a, f, false, actions),
            Phase::Done(Ok(r)) => {
                if stop_reason_notice(ui, a, actions, false, false) || collapsed {
                    if collapsed {
                        note(ui, &usage_line(a));
                    }
                    return;
                }
                markdown(ui, &r.done.answer.text, actions);
                ui.add_space(2.0);
                kit::hairline(ui);
                note(ui, &usage_line(a));
                for n in fallback_notes(a) {
                    note(ui, &n);
                }
                if let Some(p) = &a.prepared {
                    let mut parts: Vec<String> =
                        p.sources.iter().map(|(w, d)| format!("{w}: {d}")).collect();
                    if !p.redactions.is_empty() {
                        parts.push(format!("redacted {}", p.redactions.summary()));
                    }
                    note(ui, &format!("Sent: {}", parts.join(" · ")));
                }
            }
            _ => {
                let text = progress_row(ui, a, false, actions);
                if !text.is_empty() {
                    markdown(ui, &text, actions);
                }
            }
        }
    });
}

/// At the top of a pull request's conversation.
pub fn pr_tools(ui: &mut Ui, h: &HookCx, number: u64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let prefs = &h.settings.ext.ai;
    let st = &h.devops.ai;
    let target = Target::Pr(number);
    match st.asks.get(&target) {
        None => {
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 30.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if IconButton::new(Icon::Comment, "Summarise with Claude")
                        .show(ui)
                        .on_hover_text(
                            "A summary, the risky areas and questions for the author, from the description, the diff and the review threads",
                        )
                        .clicked()
                    {
                        actions.push(Act::Start(target).into());
                    }
                    ui.label(
                        RichText::new(caption(prefs, Feature::SummarisePr))
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                },
            );
        }
        Some(a) => {
            let badges = vec![(a.engine(), Tone::Neutral)];
            result_card(ui, st, a, "Claude's summary", actions, &badges);
        }
    }
    ui.add_space(space::M);
}

/// In the commit panel, after the details.
pub fn commit_panel(ui: &mut Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    let st = &h.devops.ai;
    let target = Target::Commit(c.id.to_string());
    let prefs = &h.settings.ext.ai;
    match st.asks.get(&target) {
        None => {
            kit::section_with(ui, "Claude", None, |ui| {
                if IconButton::new(Icon::Comment, "Explain this commit")
                    .compact()
                    .show(ui)
                    .on_hover_text(format!(
                        "What the commit does and what could break ({})",
                        caption(prefs, Feature::ExplainCommit)
                    ))
                    .clicked()
                {
                    actions.push(Act::Start(target.clone()).into());
                }
            });
        }
        Some(a) => {
            kit::section(ui, "Claude", None);
            let risk = a
                .reply()
                .and_then(|r| output::risk(&r.done.answer.text))
                .map(|r| {
                    let tone = match r.to_ascii_lowercase().as_str() {
                        "high" => Tone::Danger,
                        "medium" => Tone::Warning,
                        _ => Tone::Success,
                    };
                    (format!("{r} risk"), tone)
                });
            let badges: Vec<(String, Tone)> = risk.into_iter().collect();
            result_card(ui, st, a, "What this commit does", actions, &badges);
        }
    }
}

/// In a commit's context menu.
pub fn commit_menu(ui: &mut Ui, _h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    menus::separator(ui);
    if menus::entry(ui, "Explain with Claude", false).clicked() {
        actions.push(Action::Select(Selection::Commit(c.id)));
        actions.push(Act::Start(Target::Commit(c.id.to_string())).into());
        ui.close();
    }
}

/// In the Actions job log viewer's header (a right-to-left row), plus the
/// explanation window while a result exists for the job.
pub fn log_tools(ui: &mut Ui, h: &LogHookCx, run: u64, job: u64, actions: &mut Vec<Action>) {
    let st = h.ai;
    let failed = match st.job_failed.get(&job) {
        Some(f) => *f,
        None => {
            actions.push(Act::SeenJob { run, job }.into());
            None
        }
    };
    let target = Target::Job { run, job };
    let ask = st.asks.get(&target);
    if failed == Some(false) && ask.is_none() {
        return;
    }
    let busy = ask.is_some_and(|a| a.running() || matches!(a.phase, Phase::Confirming));
    if IconButton::new(Icon::Bug, "Explain with Claude")
        .compact()
        .enabled(!busy)
        .show(ui)
        .on_hover_text(format!(
            "Why this job failed, with the evidence from the log, a fix and whether it looks flaky ({})",
            caption(&h.settings.ext.ai, Feature::ExplainFailure)
        ))
        .clicked()
    {
        actions.push(Act::Start(target.clone()).into());
    }
    if let Some(a) = ask {
        explain_window(ui.ctx(), a, job, actions);
    }
}

fn explain_window(ctx: &egui::Context, a: &Ask, job: u64, actions: &mut Vec<Action>) {
    let p = *crate::theme::palette(ctx.global_style().visuals.dark_mode);
    let frame = Frame::NONE
        .fill(p.overlay)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(radius::L))
        .inner_margin(Margin::same(16))
        .shadow(ctx.global_style().visuals.window_shadow);
    let screen = ctx.content_rect();
    egui::Window::new("Explain this failure")
        .id(egui::Id::unique(("ai_explain_window", job)))
        .title_bar(false)
        .collapsible(false)
        .frame(frame)
        .default_width(600.0)
        .resizable([true, false])
        .pivot(Align2::RIGHT_TOP)
        .default_pos(egui::pos2(screen.right() - 24.0, screen.top() + 90.0))
        .constrain(true)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let flaky = a.reply().and_then(|r| output::flaky(&r.done.answer.text));
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 26.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(
                        RichText::new("Why the job failed")
                            .size(size::TITLE)
                            .family(semibold())
                            .color(p.text_strong),
                    );
                    kit::badge(ui, &a.engine(), Tone::Neutral);
                    if let Some(f) = flaky {
                        let tone = match f {
                            Flaky::Yes => Tone::Warning,
                            Flaky::No => Tone::Danger,
                            Flaky::Maybe => Tone::Neutral,
                        };
                        kit::badge(ui, f.label(), tone);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Close")
                            .clicked()
                        {
                            actions.push(Act::Dismiss(a.target.clone()).into());
                        }
                    });
                },
            );
            kit::hairline(ui);
            // The body scrolls; the window keeps to its content up to that.
            let max_body = (screen.height() * 0.62).clamp(200.0, 560.0);
            egui::ScrollArea::vertical()
                .id_salt(("ai_explain_scroll", job))
                .max_height(max_body)
                .auto_shrink([false, true])
                .show(ui, |ui| match &a.phase {
                    Phase::Done(Err(f)) => failure(ui, a, f, false, actions),
                    Phase::Done(Ok(r)) => {
                        if !stop_reason_notice(ui, a, actions, false, false) {
                            markdown(ui, &r.done.answer.text, actions);
                        }
                    }
                    _ => {
                        let text = progress_row(ui, a, false, actions);
                        if !text.is_empty() {
                            markdown(ui, &text, actions);
                        }
                    }
                });
            kit::hairline(ui);
            if a.reply().is_some() {
                note(ui, &usage_line(a));
                for n in fallback_notes(a) {
                    note(ui, &n);
                }
            }
            if let Some(pr) = &a.prepared {
                let excerpt = pr
                    .sources
                    .first()
                    .map(|(w, d)| format!("{w}: {d}"))
                    .unwrap_or_default();
                let red = if pr.redactions.is_empty() {
                    String::new()
                } else {
                    format!(" · redacted {}", pr.redactions.summary())
                };
                note(ui, &format!("Sent: {excerpt}{red}"));
            }
            if let Some(r) = a.reply() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if IconButton::new(Icon::Copy, "Copy")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::Copy(r.done.answer.text.clone()));
                    }
                    if IconButton::new(Icon::Rerun, "Regenerate")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::Start(a.target.clone()).into());
                    }
                });
            }
        });
}
