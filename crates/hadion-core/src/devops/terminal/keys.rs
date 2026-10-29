//! What a key press, a paste or a mouse event sends to the program in the
//! terminal: xterm conventions, as pure functions.
//!
//! - Printable text goes as UTF-8; Alt adds an `ESC` prefix.
//! - Enter `\r`, Backspace `\x7f` (Ctrl+Backspace `\x08`), Tab `\t`,
//!   Shift+Tab `ESC[Z`, Escape `\x1b`.
//! - Arrows: `ESC[A` normally, `ESCOA` in application cursor mode (`DECCKM`,
//!   which full-screen programs switch on); with modifiers always
//!   `ESC[1;<m>A` where `m = 1 + shift + 2·alt + 4·ctrl`.
//! - Home / End like arrows (`ESC[H`, `ESCOH`); Insert, Delete, PgUp, PgDn
//!   as `ESC[2~`, `ESC[3~`, `ESC[5~`, `ESC[6~` (`ESC[5;<m>~` with modifiers).
//! - F1–F4 `ESCOP`–`ESCOS`, F5–F12 `ESC[15~` … `ESC[24~`.
//! - Ctrl+letter `\x01`–`\x1a`, Ctrl+Space / Ctrl+2 `\0`, Ctrl+[ `ESC`,
//!   Ctrl+\ `\x1c`, Ctrl+] `\x1d`, Ctrl+6 `\x1e`, Ctrl+/ or Ctrl+- `\x1f`.
//! - Pastes are wrapped in `ESC[200~ … ESC[201~` when the program turned
//!   bracketed paste on; line ends become `\r` (what Enter sends).

/// A key that is not plain text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 … F12.
    F(u8),
    /// A character key pressed with Ctrl and/or Alt (letters, digits,
    /// punctuation, space). Plain typing arrives as text instead.
    Char(char),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// The xterm modifier parameter (`1` = none).
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// Input modes a program can switch on, read from the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// `ESC[?1h` (DECCKM): arrows and Home / End send `ESCO…`.
    pub app_cursor: bool,
    /// `ESC[?2004h`: pastes are bracketed.
    pub bracketed_paste: bool,
}

/// The bytes for a key press, or `None` when the key sends nothing.
pub fn encode_key(key: Key, mods: Mods, modes: Modes) -> Option<Vec<u8>> {
    let m = mods.param();
    // Cursor-style keys: `ESC[<c>` / `ESCO<c>`, with modifiers `ESC[1;<m><c>`.
    let cursor = |c: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{}", c as char).into_bytes()
        } else if modes.app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    // Tilde keys: `ESC[<n>~`, with modifiers `ESC[<n>;<m>~`.
    let tilde = |n: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt_prefix = |mut v: Vec<u8>| -> Vec<u8> {
        if mods.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    Some(match key {
        Key::Enter => alt_prefix(vec![b'\r']),
        Key::Backspace => {
            if mods.ctrl {
                alt_prefix(vec![0x08])
            } else {
                alt_prefix(vec![0x7f])
            }
        }
        Key::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                alt_prefix(vec![b'\t'])
            }
        }
        Key::Escape => alt_prefix(vec![0x1b]),
        Key::Up => cursor(b'A'),
        Key::Down => cursor(b'B'),
        Key::Right => cursor(b'C'),
        Key::Left => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => {
            let c = b'P' + (n - 1);
            if m > 1 {
                format!("\x1b[1;{m}{}", c as char).into_bytes()
            } else {
                vec![0x1b, b'O', c]
            }
        }
        Key::F(n @ 5..=12) => {
            const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(CODES[(n - 5) as usize])
        }
        Key::F(_) => return None,
        Key::Char(c) => return encode_char(c, mods),
    })
}

/// A character key with Ctrl and/or Alt held.
pub fn encode_char(c: char, mods: Mods) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if mods.alt {
        out.push(0x1b);
    }
    if mods.ctrl {
        let code = match c.to_ascii_lowercase() {
            l @ 'a'..='z' => l as u8 - b'a' + 1,
            ' ' | '2' | '@' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '^' | '6' => 0x1e,
            '/' | '-' | '_' | '7' => 0x1f,
            '8' => 0x7f,
            _ => return None,
        };
        out.push(code);
    } else {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

/// Typed text (Alt adds an `ESC` prefix, the meta convention).
pub fn encode_text(text: &str, alt: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// Pasted text: line ends become `\r` (what Enter sends), and with
/// bracketed paste on the text is wrapped so the shell does not run each
/// line as it arrives. An end marker inside the text is removed, so a paste
/// cannot break out of the brackets.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let inner = normalised.replace("\x1b[201~", "").replace("\x1b[200~", "");
        let mut out = Vec::with_capacity(inner.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(inner.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalised.into_bytes()
    }
}

/// A key named in a script (`term-keys ctrl-c`, `up`, `shift-tab`, `f5`,
/// `alt-b`, `enter`). Modifiers are `ctrl-`, `alt-`, `shift-` prefixes.
pub fn parse_named(name: &str) -> Option<(Key, Mods)> {
    let mut mods = Mods::NONE;
    let mut rest = name.trim().to_ascii_lowercase();
    loop {
        if let Some(r) = rest
            .strip_prefix("ctrl-")
            .or_else(|| rest.strip_prefix("ctrl+"))
        {
            mods.ctrl = true;
            rest = r.to_owned();
        } else if let Some(r) = rest
            .strip_prefix("alt-")
            .or_else(|| rest.strip_prefix("alt+"))
        {
            mods.alt = true;
            rest = r.to_owned();
        } else if let Some(r) = rest
            .strip_prefix("shift-")
            .or_else(|| rest.strip_prefix("shift+"))
        {
            mods.shift = true;
            rest = r.to_owned();
        } else {
            break;
        }
    }
    let key = match rest.as_str() {
        "enter" | "return" => Key::Enter,
        "backspace" | "bs" => Key::Backspace,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pgup" | "pageup" => Key::PageUp,
        "pgdn" | "pagedown" => Key::PageDown,
        "insert" | "ins" => Key::Insert,
        "delete" | "del" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            Key::F(n)
        }
        // A bare character (`q`) is typed as text by `encode_named`.
        s if s.chars().count() == 1 => Key::Char(s.chars().next()?),
        _ => return None,
    };
    Some((key, mods))
}

/// The bytes for a named key (see [`parse_named`]).
pub fn encode_named(name: &str, modes: Modes) -> Option<Vec<u8>> {
    let (key, mods) = parse_named(name)?;
    match key {
        Key::Char(c) if !mods.ctrl && !mods.alt => Some(encode_text(&c.to_string(), false)),
        k => encode_key(k, mods, modes),
    }
}

// ---------------------------------------------------------------------------
// Mouse reports
// ---------------------------------------------------------------------------

/// Which mouse events the program asked for (`ESC[?1000h` and friends).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    None,
    /// X10: presses only.
    Press,
    /// 1000: presses and releases.
    PressRelease,
    /// 1002: plus motion while a button is down.
    ButtonMotion,
    /// 1003: plus all motion.
    AnyMotion,
}

/// How mouse reports are encoded (`ESC[?1005h`, `ESC[?1006h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held (AnyMotion).
    None,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Motion,
}

/// A mouse event at a 0-based cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub button: MouseButton,
    pub row: u16,
    pub col: u16,
    pub mods: Mods,
}

/// The report for a mouse event, or `None` when the mode does not report it.
pub fn encode_mouse(ev: MouseEvent, mode: MouseMode, enc: MouseEncoding) -> Option<Vec<u8>> {
    let wheel = matches!(ev.button, MouseButton::WheelUp | MouseButton::WheelDown);
    let wanted = match (mode, ev.kind) {
        (MouseMode::None, _) => false,
        (_, MouseKind::Press) => true,
        (MouseMode::Press, MouseKind::Release) => false,
        (_, MouseKind::Release) => !wheel,
        (MouseMode::ButtonMotion, MouseKind::Motion) => ev.button != MouseButton::None,
        (MouseMode::AnyMotion, MouseKind::Motion) => true,
        (_, MouseKind::Motion) => false,
    };
    if !wanted {
        return None;
    }
    let mut code: u32 = match ev.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if ev.kind == MouseKind::Motion {
        code += 32;
    }
    if ev.mods.shift {
        code += 4;
    }
    if ev.mods.alt {
        code += 8;
    }
    if ev.mods.ctrl {
        code += 16;
    }
    let (x, y) = (u32::from(ev.col) + 1, u32::from(ev.row) + 1);
    match enc {
        MouseEncoding::Sgr => {
            let fin = if ev.kind == MouseKind::Release {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{code};{x};{y}{fin}").into_bytes())
        }
        MouseEncoding::Default | MouseEncoding::Utf8 => {
            // Releases are button 3 in the legacy encodings.
            if ev.kind == MouseKind::Release {
                code = (code & !3) | 3;
            }
            let mut out = b"\x1b[M".to_vec();
            let max = if enc == MouseEncoding::Utf8 {
                2015
            } else {
                223
            };
            let push = |out: &mut Vec<u8>, v: u32| {
                let v = v.min(max) + 32;
                if enc == MouseEncoding::Utf8 {
                    let c = char::from_u32(v).unwrap_or(' ');
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                } else {
                    out.push(v as u8);
                }
            };
            push(&mut out, code);
            push(&mut out, x);
            push(&mut out, y);
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: Mods = Mods::NONE;
    const NORMAL: Modes = Modes {
        app_cursor: false,
        bracketed_paste: false,
    };
    const APP: Modes = Modes {
        app_cursor: true,
        bracketed_paste: false,
    };

    fn ctrl() -> Mods {
        Mods { ctrl: true, ..N }
    }
    fn alt() -> Mods {
        Mods { alt: true, ..N }
    }
    fn shift() -> Mods {
        Mods { shift: true, ..N }
    }

    fn key(k: Key, m: Mods, modes: Modes) -> Vec<u8> {
        encode_key(k, m, modes).unwrap()
    }

    #[test]
    fn basic_keys() {
        assert_eq!(key(Key::Enter, N, NORMAL), b"\r");
        assert_eq!(key(Key::Backspace, N, NORMAL), b"\x7f");
        assert_eq!(key(Key::Backspace, ctrl(), NORMAL), b"\x08");
        assert_eq!(key(Key::Backspace, alt(), NORMAL), b"\x1b\x7f");
        assert_eq!(key(Key::Tab, N, NORMAL), b"\t");
        assert_eq!(key(Key::Tab, shift(), NORMAL), b"\x1b[Z");
        assert_eq!(key(Key::Escape, N, NORMAL), b"\x1b");
        assert_eq!(key(Key::Enter, alt(), NORMAL), b"\x1b\r");
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(key(Key::Up, N, NORMAL), b"\x1b[A");
        assert_eq!(key(Key::Down, N, NORMAL), b"\x1b[B");
        assert_eq!(key(Key::Right, N, NORMAL), b"\x1b[C");
        assert_eq!(key(Key::Left, N, NORMAL), b"\x1b[D");
        assert_eq!(key(Key::Up, N, APP), b"\x1bOA");
        assert_eq!(key(Key::Left, N, APP), b"\x1bOD");
        assert_eq!(key(Key::Home, N, NORMAL), b"\x1b[H");
        assert_eq!(key(Key::End, N, APP), b"\x1bOF");
        // Modifiers always use the CSI form.
        assert_eq!(key(Key::Up, ctrl(), APP), b"\x1b[1;5A");
        assert_eq!(key(Key::Right, shift(), NORMAL), b"\x1b[1;2C");
        assert_eq!(key(Key::Left, alt(), NORMAL), b"\x1b[1;3D");
        let cs = Mods {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(key(Key::End, cs, NORMAL), b"\x1b[1;6F");
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(key(Key::Insert, N, NORMAL), b"\x1b[2~");
        assert_eq!(key(Key::Delete, N, NORMAL), b"\x1b[3~");
        assert_eq!(key(Key::PageUp, N, NORMAL), b"\x1b[5~");
        assert_eq!(key(Key::PageDown, N, NORMAL), b"\x1b[6~");
        assert_eq!(key(Key::Delete, ctrl(), NORMAL), b"\x1b[3;5~");
        assert_eq!(key(Key::F(1), N, NORMAL), b"\x1bOP");
        assert_eq!(key(Key::F(4), N, NORMAL), b"\x1bOS");
        assert_eq!(key(Key::F(1), shift(), NORMAL), b"\x1b[1;2P");
        assert_eq!(key(Key::F(5), N, NORMAL), b"\x1b[15~");
        assert_eq!(key(Key::F(6), N, NORMAL), b"\x1b[17~");
        assert_eq!(key(Key::F(10), N, NORMAL), b"\x1b[21~");
        assert_eq!(key(Key::F(11), N, NORMAL), b"\x1b[23~");
        assert_eq!(key(Key::F(12), N, NORMAL), b"\x1b[24~");
        assert_eq!(key(Key::F(12), ctrl(), NORMAL), b"\x1b[24;5~");
        assert_eq!(encode_key(Key::F(13), N, NORMAL), None);
    }

    #[test]
    fn control_characters() {
        assert_eq!(key(Key::Char('a'), ctrl(), NORMAL), [0x01]);
        assert_eq!(key(Key::Char('c'), ctrl(), NORMAL), [0x03]);
        assert_eq!(key(Key::Char('D'), ctrl(), NORMAL), [0x04]);
        assert_eq!(key(Key::Char('l'), ctrl(), NORMAL), [0x0c]);
        assert_eq!(key(Key::Char('z'), ctrl(), NORMAL), [0x1a]);
        assert_eq!(key(Key::Char(' '), ctrl(), NORMAL), [0x00]);
        assert_eq!(key(Key::Char('['), ctrl(), NORMAL), [0x1b]);
        assert_eq!(key(Key::Char('\\'), ctrl(), NORMAL), [0x1c]);
        assert_eq!(key(Key::Char(']'), ctrl(), NORMAL), [0x1d]);
        assert_eq!(key(Key::Char('6'), ctrl(), NORMAL), [0x1e]);
        assert_eq!(key(Key::Char('/'), ctrl(), NORMAL), [0x1f]);
        assert_eq!(encode_key(Key::Char('é'), ctrl(), NORMAL), None);
        // Alt is an ESC prefix, also combined with Ctrl.
        assert_eq!(key(Key::Char('b'), alt(), NORMAL), b"\x1bb");
        let ca = Mods {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(key(Key::Char('x'), ca, NORMAL), b"\x1b\x18");
    }

    #[test]
    fn text_and_paste() {
        assert_eq!(encode_text("héllo", false), "héllo".as_bytes());
        assert_eq!(encode_text("f", true), b"\x1bf");
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(
            encode_paste("ls\nrm -rf x", true),
            b"\x1b[200~ls\rrm -rf x\x1b[201~"
        );
        // A paste cannot close the brackets early.
        assert_eq!(encode_paste("a\x1b[201~b", true), b"\x1b[200~ab\x1b[201~");
    }

    #[test]
    fn named_keys() {
        assert_eq!(encode_named("ctrl-c", NORMAL).unwrap(), [0x03]);
        assert_eq!(encode_named("Ctrl+D", NORMAL).unwrap(), [0x04]);
        assert_eq!(encode_named("up", NORMAL).unwrap(), b"\x1b[A");
        assert_eq!(encode_named("up", APP).unwrap(), b"\x1bOA");
        assert_eq!(encode_named("tab", NORMAL).unwrap(), b"\t");
        assert_eq!(encode_named("shift-tab", NORMAL).unwrap(), b"\x1b[Z");
        assert_eq!(encode_named("enter", NORMAL).unwrap(), b"\r");
        assert_eq!(encode_named("esc", NORMAL).unwrap(), b"\x1b");
        assert_eq!(encode_named("f5", NORMAL).unwrap(), b"\x1b[15~");
        assert_eq!(encode_named("alt-b", NORMAL).unwrap(), b"\x1bb");
        assert_eq!(encode_named("pgup", NORMAL).unwrap(), b"\x1b[5~");
        assert_eq!(encode_named("q", NORMAL).unwrap(), b"q");
        assert_eq!(encode_named("ctrl-space", NORMAL).unwrap(), [0]);
        assert_eq!(encode_named("f13", NORMAL), None);
        assert_eq!(encode_named("bogus", NORMAL), None);
    }

    fn ev(kind: MouseKind, button: MouseButton, row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            kind,
            button,
            row,
            col,
            mods: N,
        }
    }

    #[test]
    fn mouse_reports() {
        use MouseButton as B;
        use MouseKind as K;
        let press = ev(K::Press, B::Left, 4, 9);
        // SGR: 1-based coordinates, M for press, m for release.
        assert_eq!(
            encode_mouse(press, MouseMode::PressRelease, MouseEncoding::Sgr).unwrap(),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            encode_mouse(
                ev(K::Release, B::Left, 4, 9),
                MouseMode::PressRelease,
                MouseEncoding::Sgr
            )
            .unwrap(),
            b"\x1b[<0;10;5m"
        );
        // Legacy: +32 offsets, release is button 3.
        assert_eq!(
            encode_mouse(press, MouseMode::PressRelease, MouseEncoding::Default).unwrap(),
            [0x1b, b'[', b'M', 32, 32 + 10, 32 + 5]
        );
        assert_eq!(
            encode_mouse(
                ev(K::Release, B::Left, 0, 0),
                MouseMode::PressRelease,
                MouseEncoding::Default
            )
            .unwrap(),
            [0x1b, b'[', b'M', 32 + 3, 33, 33]
        );
        // Wheel: buttons 64 / 65, press only.
        assert_eq!(
            encode_mouse(
                ev(K::Press, B::WheelUp, 0, 0),
                MouseMode::Press,
                MouseEncoding::Sgr
            )
            .unwrap(),
            b"\x1b[<64;1;1M"
        );
        assert_eq!(
            encode_mouse(
                ev(K::Release, B::WheelDown, 0, 0),
                MouseMode::AnyMotion,
                MouseEncoding::Sgr
            ),
            None
        );
        // Motion only in the motion modes; drag adds 32.
        let drag = ev(K::Motion, B::Left, 1, 1);
        assert_eq!(
            encode_mouse(drag, MouseMode::PressRelease, MouseEncoding::Sgr),
            None
        );
        assert_eq!(
            encode_mouse(drag, MouseMode::ButtonMotion, MouseEncoding::Sgr).unwrap(),
            b"\x1b[<32;2;2M"
        );
        let hover = ev(K::Motion, B::None, 1, 1);
        assert_eq!(
            encode_mouse(hover, MouseMode::ButtonMotion, MouseEncoding::Sgr),
            None
        );
        assert_eq!(
            encode_mouse(hover, MouseMode::AnyMotion, MouseEncoding::Sgr).unwrap(),
            b"\x1b[<35;2;2M"
        );
        // No mode: nothing is reported.
        assert_eq!(
            encode_mouse(press, MouseMode::None, MouseEncoding::Sgr),
            None
        );
        // Modifiers.
        let mut m = press;
        m.mods.ctrl = true;
        m.mods.shift = true;
        assert_eq!(
            encode_mouse(m, MouseMode::Press, MouseEncoding::Sgr).unwrap(),
            b"\x1b[<20;10;5M"
        );
        // UTF-8 encoding carries big coordinates.
        let far = ev(K::Press, B::Left, 0, 299);
        let out = encode_mouse(far, MouseMode::Press, MouseEncoding::Utf8).unwrap();
        assert_eq!(&out[..4], &[0x1b, b'[', b'M', 32]);
        assert_eq!(
            String::from_utf8(out[4..].to_vec()).unwrap(),
            format!("{}{}", char::from_u32(300 + 32).unwrap(), '!')
        );
    }
}
