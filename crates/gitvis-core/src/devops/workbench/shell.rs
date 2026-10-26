//! Shell helpers: splitting typed arguments, quoting for `sh`, and finding
//! Git for Windows' `sh` (used by `git bisect run` and test commands).

use std::path::{Path, PathBuf};

use crate::devops::proc::resolve;

/// Split a typed argument string like a POSIX shell would, without any
/// expansion: whitespace separates, single quotes are literal, double
/// quotes allow `\"` and `\\`, a backslash outside quotes escapes the next
/// character. `--watch "a b" 'c d'` → `["--watch", "a b", "c d"]`.
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                have = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    cur.push(d);
                }
            }
            '"' => {
                have = true;
                while let Some(d) = chars.next() {
                    match d {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                            cur.push(chars.next().expect("peeked"));
                        }
                        _ => cur.push(d),
                    }
                }
            }
            '\\' if !cfg!(windows) || matches!(chars.peek(), Some(' ' | '"' | '\'')) => {
                // On Windows a lone backslash is a path separator.
                have = true;
                if let Some(d) = chars.next() {
                    cur.push(d);
                }
            }
            c if c.is_whitespace() => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                have = true;
                cur.push(c);
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// Quote one word for `sh`: `it's` → `'it'\''s'`; plain words stay as they
/// are.
pub fn sh_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// A path as `sh` (and git's sequencer) on Windows wants it: forward
/// slashes, single-quoted: `'C:/Users/me/AppData/Local/Temp/todo'`.
pub fn sh_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Git for Windows' `sh.exe`, next to the git it ships (`…\Git\cmd\git.exe`
/// → `…\Git\bin\sh.exe` or `…\Git\usr\bin\sh.exe`), else `sh` on PATH.
pub fn find_sh() -> Option<PathBuf> {
    if cfg!(windows)
        && let Some(git) = resolve("git")
    {
        // git.exe lives in Git\cmd, Git\bin or Git\mingw64\bin.
        let mut dir = git.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(d) = dir.clone() else { break };
            for rel in ["bin/sh.exe", "usr/bin/sh.exe"] {
                let candidate = d.join(rel);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    if let Some(p) = resolve("sh") {
        return Some(p);
    }
    if cfg!(windows) {
        for root in [r"C:\Program Files\Git", r"C:\Program Files (x86)\Git"] {
            let p = Path::new(root).join(r"bin\sh.exe");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    } else {
        Some(PathBuf::from("/bin/sh"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            split_args(r#"--watch "a b" 'c d' e"#),
            vec!["--watch", "a b", "c d", "e"]
        );
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert_eq!(split_args(r#"""#), vec![""]);
        assert_eq!(
            split_args(r#"say "it \"works\"""#),
            vec!["say", "it \"works\""]
        );
        assert_eq!(split_args("a''b"), vec!["ab"]);
        assert_eq!(split_args(r"x\ y"), vec!["x y"]);
    }

    #[test]
    fn quotes_for_sh() {
        assert_eq!(sh_quote("plain-word_1.txt"), "plain-word_1.txt");
        assert_eq!(sh_quote("two words"), "'two words'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(
            sh_path(Path::new(r"C:\Users\o'neil\todo")),
            "'C:/Users/o'\\''neil/todo'"
        );
    }

    #[test]
    fn finds_a_shell() {
        let sh = find_sh().expect("a shell on this machine");
        assert!(sh.exists() || !cfg!(windows), "{}", sh.display());
    }
}
