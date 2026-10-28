//! Keyboard input while the terminal has focus.
//!
//! [`take_keys`] runs from `tick`, before the app looks at its shortcuts,
//! and removes every key, text, copy, cut, paste and IME event from egui's
//! queue (except Ctrl+`, which still toggles the panel). [`translate`]
//! turns them into what the shell receives; a few combinations stay with
//! the terminal itself:
//!
//! | Keys | Does |
//! |---|---|
//! | Ctrl+C | copies when text is selected, otherwise sends `^C` |
//! | Ctrl+Shift+C / Ctrl+Insert | copy |
//! | Ctrl+V, Ctrl+Shift+V, Shift+Insert | paste (bracketed when the program asked) |
//! | Shift+PgUp / Shift+PgDn | scroll the history a page |
//! | Ctrl+Shift+Up / Down | scroll a line |
//! | Shift+Home / Shift+End | top of the history / live screen |

use egui::{Event, Key, Modifiers};
use gitvis_core::devops::terminal::keys::{self, Key as TKey, Modes, Mods};

use super::State;

/// What an input event asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    Bytes(Vec<u8>),
    Copy,
    /// Ask the platform for the clipboard (it arrives as a paste event).
    RequestPaste,
    /// History lines (up > 0) or pages.
    ScrollLines(isize),
    ScrollPages(isize),
    ScrollTop,
    ScrollBottom,
}

fn mods(m: &Modifiers) -> Mods {
    Mods {
        ctrl: m.ctrl,
        alt: m.alt,
        shift: m.shift,
    }
}

fn special(key: Key) -> Option<TKey> {
    Some(match key {
        Key::Enter => TKey::Enter,
        Key::Tab => TKey::Tab,
        Key::Backspace => TKey::Backspace,
        Key::Escape => TKey::Escape,
        Key::ArrowUp => TKey::Up,
        Key::ArrowDown => TKey::Down,
        Key::ArrowLeft => TKey::Left,
        Key::ArrowRight => TKey::Right,
        Key::Home => TKey::Home,
        Key::End => TKey::End,
        Key::PageUp => TKey::PageUp,
        Key::PageDown => TKey::PageDown,
        Key::Insert => TKey::Insert,
        Key::Delete => TKey::Delete,
        Key::F1 => TKey::F(1),
        Key::F2 => TKey::F(2),
        Key::F3 => TKey::F(3),
        Key::F4 => TKey::F(4),
        Key::F5 => TKey::F(5),
        Key::F6 => TKey::F(6),
        Key::F7 => TKey::F(7),
        Key::F8 => TKey::F(8),
        Key::F9 => TKey::F(9),
        Key::F10 => TKey::F(10),
        Key::F11 => TKey::F(11),
        Key::F12 => TKey::F(12),
        _ => return None,
    })
}

/// The character a key types without Shift (for Ctrl and Alt combinations).
fn key_char(key: Key) -> Option<char> {
    let name = key.name();
    if name.len() == 1 {
        return name.chars().next().map(|c| c.to_ascii_lowercase());
    }
    Some(match key {
        Key::Space => ' ',
        Key::Minus => '-',
        Key::Equals => '=',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::Backslash => '\\',
        Key::Slash => '/',
        Key::Comma => ',',
        Key::Period => '.',
        Key::Semicolon => ';',
        Key::Quote => '\'',
        Key::Backtick => '`',
        _ => return None,
    })
}

/// Is this the panel toggle (left for the app)?
fn is_toggle(ev: &Event) -> bool {
    matches!(ev, Event::Key { key: Key::Backtick, modifiers, .. } if modifiers.ctrl && !modifiers.alt)
}

/// Translate input events. `has_selection` decides what Ctrl+C does.
pub fn translate(events: &[Event], modes: Modes, has_selection: bool) -> Vec<Out> {
    let mut out = Vec::new();
    let mut skip_text = false;
    for (i, ev) in events.iter().enumerate() {
        match ev {
            Event::Text(t) => {
                if std::mem::take(&mut skip_text) {
                    continue;
                }
                out.push(Out::Bytes(keys::encode_text(t, false)));
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => {
                out.push(Out::Bytes(keys::encode_text(t, false)));
            }
            Event::Paste(t) => out.push(Out::Bytes(keys::encode_paste(t, modes.bracketed_paste))),
            Event::Copy => {
                out.push(if has_selection {
                    Out::Copy
                } else {
                    Out::Bytes(vec![0x03])
                });
            }
            Event::Cut => out.push(Out::Bytes(vec![0x18])),
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                skip_text = false;
                let m = mods(modifiers);
                // The terminal's own combinations.
                if m.shift && !m.ctrl && !m.alt {
                    match key {
                        Key::PageUp => {
                            out.push(Out::ScrollPages(1));
                            continue;
                        }
                        Key::PageDown => {
                            out.push(Out::ScrollPages(-1));
                            continue;
                        }
                        Key::Home => {
                            out.push(Out::ScrollTop);
                            continue;
                        }
                        Key::End => {
                            out.push(Out::ScrollBottom);
                            continue;
                        }
                        _ => {}
                    }
                }
                if m.ctrl && m.shift && !m.alt {
                    match key {
                        Key::C => {
                            out.push(Out::Copy);
                            continue;
                        }
                        Key::V => {
                            out.push(Out::RequestPaste);
                            continue;
                        }
                        Key::ArrowUp => {
                            out.push(Out::ScrollLines(1));
                            continue;
                        }
                        Key::ArrowDown => {
                            out.push(Out::ScrollLines(-1));
                            continue;
                        }
                        _ => {}
                    }
                }
                if let Some(k) = special(*key) {
                    if let Some(b) = keys::encode_key(k, m, modes) {
                        out.push(Out::Bytes(b));
                    }
                    continue;
                }
                if m.ctrl || m.alt {
                    let Some(c) = key_char(*key) else {
                        continue;
                    };
                    if m.alt && !m.ctrl {
                        // Alt+key: ESC + the character the key types (the
                        // text event that may follow carries it).
                        if let Some(Event::Text(t)) = events.get(i + 1) {
                            out.push(Out::Bytes(keys::encode_text(t, true)));
                            skip_text = true;
                            continue;
                        }
                        let c = if m.shift { c.to_ascii_uppercase() } else { c };
                        out.push(Out::Bytes(keys::encode_text(&c.to_string(), true)));
                        continue;
                    }
                    if let Some(b) = keys::encode_char(c, m) {
                        out.push(Out::Bytes(b));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Synthetic key presses for scripts (`term-press ctrl+l tab g i t enter`):
/// each space-separated token is a key with optional `ctrl+` / `alt+` /
/// `shift+` prefixes; a single character also produces its text event, as
/// a real key press does. They enter egui's queue at the start of a frame,
/// so they meet the app's shortcuts exactly like real keys.
pub fn parse_press(spec: &str) -> Vec<Event> {
    let mut out = Vec::new();
    for token in spec.split_whitespace() {
        let mut m = Modifiers::NONE;
        let mut rest = token.to_ascii_lowercase();
        loop {
            if let Some(r) = rest.strip_prefix("ctrl+") {
                m.ctrl = true;
                m.command = true;
                rest = r.to_owned();
            } else if let Some(r) = rest.strip_prefix("alt+") {
                m.alt = true;
                rest = r.to_owned();
            } else if let Some(r) = rest.strip_prefix("shift+") {
                m.shift = true;
                rest = r.to_owned();
            } else {
                break;
            }
        }
        let named = match rest.as_str() {
            "up" => Some(Key::ArrowUp),
            "down" => Some(Key::ArrowDown),
            "left" => Some(Key::ArrowLeft),
            "right" => Some(Key::ArrowRight),
            "esc" => Some(Key::Escape),
            "pgup" => Some(Key::PageUp),
            "pgdn" => Some(Key::PageDown),
            "`" | "backtick" => Some(Key::Backtick),
            "space" => Some(Key::Space),
            r if r.chars().count() > 1 => {
                let mut name = r.to_owned();
                name[..1].make_ascii_uppercase();
                Key::from_name(&name)
            }
            r => Key::from_name(&r.to_ascii_uppercase()),
        };
        let Some(key) = named else {
            continue;
        };
        out.push(Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: m,
        });
        let text = match key {
            Key::Space => Some(" ".to_owned()),
            _ if rest.chars().count() == 1 => Some(if m.shift {
                rest.to_ascii_uppercase()
            } else {
                rest.clone()
            }),
            _ => None,
        };
        if let Some(t) = text
            && !m.ctrl
        {
            out.push(Event::Text(t));
        }
        out.push(Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers: m,
        });
    }
    out
}

/// Take the terminal's events out of egui's queue and send them.
pub fn take_keys(st: &mut State, ctx: &egui::Context) {
    if !st.injected.is_empty() {
        let events = std::mem::take(&mut st.injected);
        ctx.input_mut(|i| i.events.extend(events));
    }
    let focused = st.focused && !st.tabs.is_empty();
    let paste_to = st.paste_pending;
    if !focused && paste_to.is_none() {
        return;
    }
    let events: Vec<Event> = ctx.input_mut(|i| {
        let mut taken = Vec::new();
        i.events.retain(|e| {
            let take = if is_toggle(e) {
                false
            } else if focused {
                matches!(
                    e,
                    Event::Key { .. }
                        | Event::Text(_)
                        | Event::Copy
                        | Event::Cut
                        | Event::Paste(_)
                        | Event::Ime(egui::ImeEvent::Commit(_))
                )
            } else {
                matches!(e, Event::Paste(_))
            };
            if take {
                taken.push(e.clone());
            }
            !take
        });
        taken
    });
    if events.is_empty() {
        return;
    }
    let target = match paste_to {
        Some(id) if events.iter().any(|e| matches!(e, Event::Paste(_))) => {
            st.paste_pending = None;
            st.tab_index(id).unwrap_or(st.active)
        }
        _ => st.active,
    };
    let Some(tab) = st.tabs.get_mut(target) else {
        return;
    };
    tab.view();
    let modes = tab.modes();
    let rows = tab.view.as_ref().map(|(_, v)| v.rows).unwrap_or(24) as isize;
    let mut copy = false;
    let mut request_paste = false;
    for o in translate(&events, modes, tab.selection.is_some()) {
        match o {
            Out::Bytes(b) => tab.send_input(b),
            Out::Copy => copy = true,
            Out::RequestPaste => request_paste = true,
            Out::ScrollLines(n) => tab.scroll_by(n),
            Out::ScrollPages(n) => tab.scroll_by(n * (rows - 1).max(1)),
            Out::ScrollTop => tab.scroll_by(isize::MAX / 2),
            Out::ScrollBottom => tab.scroll_to_bottom(),
        }
    }
    if copy {
        super::copy_selection(st);
        // The next Ctrl+C interrupts again.
        if let Some(t) = st.tabs.get_mut(target) {
            t.selection = None;
        }
    }
    if request_paste {
        st.paste_pending = st.tabs.get(target).map(|t| t.id);
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn bytes(events: &[Event]) -> Vec<u8> {
        translate(events, Modes::default(), false)
            .into_iter()
            .flat_map(|o| match o {
                Out::Bytes(b) => b,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn typing_and_special_keys() {
        assert_eq!(
            bytes(&[
                key(Key::G, Modifiers::NONE),
                Event::Text("g".into()),
                Event::Text("it".into()),
                key(Key::Enter, Modifiers::NONE),
            ]),
            b"git\r"
        );
        assert_eq!(bytes(&[key(Key::Tab, Modifiers::NONE)]), b"\t");
        assert_eq!(bytes(&[key(Key::Tab, Modifiers::SHIFT)]), b"\x1b[Z");
        assert_eq!(bytes(&[key(Key::ArrowUp, Modifiers::NONE)]), b"\x1b[A");
        assert_eq!(bytes(&[key(Key::F5, Modifiers::NONE)]), b"\x1b[15~");
        assert_eq!(bytes(&[key(Key::Backspace, Modifiers::NONE)]), b"\x7f");
        let app = translate(
            &[key(Key::ArrowLeft, Modifiers::NONE)],
            Modes {
                app_cursor: true,
                bracketed_paste: false,
            },
            false,
        );
        assert_eq!(app, vec![Out::Bytes(b"\x1bOD".to_vec())]);
    }

    #[test]
    fn control_and_alt() {
        assert_eq!(bytes(&[key(Key::L, Modifiers::CTRL)]), [0x0c]);
        assert_eq!(bytes(&[key(Key::K, Modifiers::CTRL)]), [0x0b]);
        assert_eq!(bytes(&[key(Key::Z, Modifiers::CTRL)]), [0x1a]);
        assert_eq!(bytes(&[key(Key::D, Modifiers::CTRL)]), [0x04]);
        assert_eq!(bytes(&[key(Key::OpenBracket, Modifiers::CTRL)]), [0x1b]);
        // Alt + key followed by its text: one ESC-prefixed character.
        assert_eq!(
            bytes(&[key(Key::B, Modifiers::ALT), Event::Text("b".into())]),
            b"\x1bb"
        );
        // Alt + key without text.
        assert_eq!(bytes(&[key(Key::F, Modifiers::ALT)]), b"\x1bf");
        // Plain letters come from the text event only.
        assert_eq!(bytes(&[key(Key::A, Modifiers::NONE)]), b"");
    }

    #[test]
    fn copy_paste_and_scrolling() {
        // Ctrl+C: copy with a selection, interrupt without.
        assert_eq!(translate(&[Event::Copy], Modes::default(), true), vec![Out::Copy]);
        assert_eq!(bytes(&[Event::Copy]), [0x03]);
        assert_eq!(bytes(&[Event::Cut]), [0x18]);
        assert_eq!(
            translate(
                &[Event::Paste("a\nb".into())],
                Modes {
                    app_cursor: false,
                    bracketed_paste: true
                },
                false
            ),
            vec![Out::Bytes(b"\x1b[200~a\rb\x1b[201~".to_vec())]
        );
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        let t = |e| translate(&[e], Modes::default(), false);
        assert_eq!(t(key(Key::C, cs)), vec![Out::Copy]);
        assert_eq!(t(key(Key::V, cs)), vec![Out::RequestPaste]);
        assert_eq!(t(key(Key::PageUp, Modifiers::SHIFT)), vec![Out::ScrollPages(1)]);
        assert_eq!(t(key(Key::End, Modifiers::SHIFT)), vec![Out::ScrollBottom]);
        assert_eq!(t(key(Key::ArrowUp, cs)), vec![Out::ScrollLines(1)]);
    }

    #[test]
    fn scripted_presses() {
        let ev = parse_press("g ctrl+l tab shift+a space up `");
        let text: Vec<&Event> = ev.iter().filter(|e| matches!(e, Event::Text(_))).collect();
        assert_eq!(
            text,
            vec![
                &Event::Text("g".into()),
                &Event::Text("A".into()),
                &Event::Text(" ".into()),
                &Event::Text("`".into())
            ]
        );
        // Through the translator: what the shell receives.
        assert_eq!(
            bytes(&parse_press("e c h o space ctrl+l tab up")),
            b"echo \x0c\t\x1b[A"
        );
        assert!(parse_press("ctrl+`").iter().any(is_toggle));
        assert!(parse_press("bogus-key").is_empty());
    }

    #[test]
    fn panel_toggle_is_left_to_the_app() {
        assert!(is_toggle(&key(Key::Backtick, Modifiers::CTRL)));
        assert!(!is_toggle(&key(Key::Backtick, Modifiers::NONE)));
        assert!(!is_toggle(&key(Key::K, Modifiers::CTRL)));
    }
}
