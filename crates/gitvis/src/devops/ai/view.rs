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
use gitvis_core::devops::ai::output::{self, Flaky};
use gitvis_core::devops::ai::{Failure, Feature, Model, dollars};

use super::{Act, Ask, Phase, Prefs, State, Target, control};
use crate::devops::DevOpsAction;
use crate::devops::hooks::{HookCx, LogHookCx};
use crate::state::{Action, Page, Selection};
use crate::theme::{Tone, medium, pal, radius, semibold, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::{form, kit, menus};

/// `Opus 5.5 · low effort`.
fn caption(prefs: &Prefs, f: Feature) -> String {
    format!(
        "{} · {} effort",
        prefs.model_now().short(),
        prefs.effort(f).as_str()
    )
}

fn open_settings(actions: &mut Vec<Action>) {
    actions.push(Action::DevOps(DevOpsAction::SetSettingsSection(
        crate::devops::settings_page::Section::Ai,
    )));
    actions.push(Action::ShowPage(Page::Settings));
}

/// `Claude Opus 5.5 · 1,204 in · 96 out · cache 0 / 800 · $0.0042 · 3.1 s`.
fn usage_line(a: &Ask) -> String {
    let Some(r) = a.reply() else {
        return String::new();
    };
    let ans = &r.done.answer;
    let model = if ans.model.is_empty() {
        a.model.label().to_owned()
    } else {
        Model::label_of(&ans.model)
    };
    let u = &ans.usage;
    let mut s = format!(
        "{model} · {} in · {} out",
        crate::format::count(u.input_tokens as usize),
        crate::format::count(u.output_tokens as usize)
    );
    if u.cache_creation_input_tokens + u.cache_read_input_tokens > 0 {
        s.push_str(&format!(
            " · cache write {} / read {}",
            crate::format::count(u.cache_creation_input_tokens as usize),
            crate::format::count(u.cache_read_input_tokens as usize)
        ));
    }
    s.push_str(&format!(
        " · about {} · {}",
        dollars(r.cost),
        crate::format::millis(r.done.elapsed_ms)
    ));
    s
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

/// Live text of a running request, and whether the model is still thinking.
fn live_text(a: &Ask) -> Option<(String, bool, u64)> {
    let Phase::Running { live, .. } = &a.phase else {
        return None;
    };
    let l = live.lock().unwrap_or_else(|e| e.into_inner());
    Some((
        l.answer.text.clone(),
        l.answer.text.is_empty(),
        l.started.elapsed().as_secs(),
    ))
}

/// Spinner, phase, elapsed time and Stop, for a request in flight.
/// Returns the live text (empty while thinking).
fn progress_row(ui: &mut Ui, a: &Ask, actions: &mut Vec<Action>) -> String {
    let p = *pal(ui);
    ui.ctx().request_repaint_after(Duration::from_millis(120));
    let (text, phase) = match &a.phase {
        Phase::Preparing => (
            String::new(),
            "Gathering and checking what to send…".to_owned(),
        ),
        Phase::Confirming => (String::new(), "Waiting for your confirmation".to_owned()),
        Phase::Running { .. } => {
            let (text, thinking, secs) = live_text(a).unwrap_or_default();
            let phase = if thinking {
                format!("{} is thinking · {secs} s", a.model.short())
            } else {
                format!(
                    "{} is writing · {} characters · {secs} s",
                    a.model.short(),
                    crate::format::count(text.chars().count())
                )
            };
            (text, phase)
        }
        Phase::Done(_) => return String::new(),
    };
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 26.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if !matches!(a.phase, Phase::Confirming) {
                ui.add(egui::Spinner::new().size(13.0).color(p.accent));
            }
            ui.add(Label::new(RichText::new(phase).size(size::SMALL).color(p.text_2)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                match a.phase {
                    Phase::Confirming => {
                        if IconButton::text("Cancel").compact().show(ui).clicked() {
                            actions.push(Act::Cancel(a.target.clone()).into());
                        }
                        if IconButton::text("Review and send")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::Reconfirm(a.target.clone()).into());
                        }
                    }
                    Phase::Running { .. } => {
                        if IconButton::new(Icon::Stop, "Stop")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::Cancel(a.target.clone()).into());
                        }
                    }
                    _ => {}
                }
            });
        },
    );
    text
}

/// A failure as a callout with what to do about it.
fn failure(ui: &mut Ui, a: &Ask, f: &Failure, actions: &mut Vec<Action>) {
    let t = a.target.clone();
    let (tone, title) = match f {
        Failure::NoKey => (Tone::Warning, "No Claude API key"),
        Failure::NoCurl => (Tone::Warning, "curl is not installed"),
        Failure::Cancelled => (Tone::Neutral, "Stopped"),
        Failure::Prepare(_) => (Tone::Warning, "Could not gather what to send"),
        Failure::Transport(_) => (Tone::Danger, "The request did not get through"),
        Failure::Api(_) => (Tone::Danger, "The Claude API returned an error"),
        Failure::Incomplete(_) => (Tone::Danger, "The answer was cut off"),
    };
    let mut body = match f {
        Failure::Cancelled => "Nothing was filled in.".to_owned(),
        Failure::NoCurl => {
            "gitvis sends requests with curl, which reads the key from standard input so it never appears on a command line. curl ships with Windows 10 and 11 (C:\\Windows\\System32\\curl.exe) and with Git for Windows; otherwise: winget install cURL.cURL".to_owned()
        }
        other => other.message(),
    };
    if let Failure::Api(e) = f
        && !e.hint().is_empty()
    {
        body.push_str("\n");
        body.push_str(e.hint());
    }
    let wait = control::retry_wait(a);
    kit::callout_with(ui, tone, title, &body, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            match f {
                Failure::NoKey => {
                    if IconButton::new(Icon::Key, "Open AI settings")
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
                Failure::Api(e) if e.kind == "authentication_error" => {
                    if IconButton::new(Icon::Key, "Open AI settings")
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
fn stop_reason_notice(ui: &mut Ui, a: &Ask, actions: &mut Vec<Action>, can_use: bool) -> bool {
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
        kit::callout_with(
            ui,
            Tone::Warning,
            "Claude declined this request",
            &body,
            |ui| {
                if IconButton::text("Dismiss").compact().show(ui).clicked() {
                    actions.push(Act::Dismiss(a.target.clone()).into());
                }
            },
        );
        return true;
    }
    if ans.hit_max_tokens() {
        kit::callout_with(
            ui,
            Tone::Warning,
            "The answer hit the 16,000-token limit",
            "It stops mid-way, so it was not filled in. Use it anyway, or try again.",
            |ui| {
                ui.horizontal(|ui| {
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
fn fill_status(ui: &mut Ui, a: &Ask, done_text: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    match &a.phase {
        Phase::Preparing | Phase::Confirming | Phase::Running { .. } => {
            let text = progress_row(ui, a, actions);
            if !text.is_empty() {
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
        Phase::Done(Err(f)) => failure(ui, a, f, actions),
        Phase::Done(Ok(r)) => {
            if stop_reason_notice(ui, a, actions, true) {
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
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 22.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    crate::views::icons::show(ui, Icon::Check, 12.0, p.success);
                    ui.add(
                        Label::new(
                            RichText::new(done_text)
                                .size(size::SMALL)
                                .family(medium())
                                .color(p.text),
                        )
                        .truncate(),
                    );
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
            note(ui, &usage_line(a));
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
            {
                note(
                    ui,
                    &format!("Redacted before sending: {}", p.redactions.summary()),
                );
            }
        }
    }
}

/// Above the commit message box.
pub fn wip_tools(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let prefs = &h.settings.ext.ai;
    let st: &State = &h.devops.ai;
    let staged = !h.snap.status.staged.is_empty();
    let ask = st.asks.get(&Target::Wip);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 28.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let busy = ask.is_some_and(|a| a.running() || matches!(a.phase, Phase::Confirming));
            let enabled = staged && !busy && !h.busy;
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
            ui.add(
                Label::new(
                    RichText::new(caption(prefs, Feature::CommitMessage))
                        .size(size::SMALL)
                        .color(p.text_3),
                )
                .truncate(),
            );
        },
    );
    if let Some(a) = ask {
        let what = format!("Message written by {}", a.model.label());
        fill_status(ui, a, &what, actions);
    }
    ui.add_space(4.0);
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
                let what = format!("Title and description written by {}", a.model.label());
                fill_status(ui, a, &what, actions);
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
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 24.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let done = matches!(a.phase, Phase::Done(Ok(_)));
                if done
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
                ui.label(
                    RichText::new(title)
                        .size(size::BODY)
                        .family(semibold())
                        .color(p.text_strong),
                );
                for (b, tone) in badges {
                    kit::badge(ui, b, *tone);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                        if IconButton::new(Icon::Rerun, "Regenerate")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::Start(a.target.clone()).into());
                        }
                    }
                });
            },
        );
        match &a.phase {
            Phase::Done(Err(f)) => failure(ui, a, f, actions),
            Phase::Done(Ok(r)) => {
                if stop_reason_notice(ui, a, actions, false) || collapsed {
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
                let text = progress_row(ui, a, actions);
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
            let badges = vec![(a.model.short().to_owned(), Tone::Neutral)];
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
                    .on_hover_text(&format!(
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
        .on_hover_text(&format!(
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
        .resizable(true)
        .frame(frame)
        .default_size(vec2(600.0, 560.0))
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
                    kit::badge(ui, a.model.short(), Tone::Neutral);
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
            let footer_h = 64.0;
            egui::ScrollArea::vertical()
                .id_salt(("ai_explain_scroll", job))
                .max_height((ui.available_height() - footer_h).max(120.0))
                .auto_shrink([false, false])
                .show(ui, |ui| match &a.phase {
                    Phase::Done(Err(f)) => failure(ui, a, f, actions),
                    Phase::Done(Ok(r)) => {
                        if !stop_reason_notice(ui, a, actions, false) {
                            markdown(ui, &r.done.answer.text, actions);
                        }
                    }
                    _ => {
                        let text = progress_row(ui, a, actions);
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
