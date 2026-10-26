//! Syntax colouring for diff lines through syntect, run on the reader thread.

use std::sync::OnceLock;

use egui::Color32;
use gitvis_core::diff::{FileDiff, LineKind};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::state::Span;

/// Diffs with more lines than this are shown without colouring.
const MAX_LINES: usize = 20_000;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// The grammar for a path and whether it is the language itself (`true`) or
/// a stand-in that colours it acceptably (`false`).
fn syntax_for(path: &str) -> Option<(&'static SyntaxReference, bool)> {
    let set = syntaxes();
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "dockerfile" | "containerfile" => Some("Dockerfile"),
        "makefile" | "gnumakefile" => Some("Makefile"),
        "cmakelists.txt" => Some("CMake"),
        _ => None,
    };
    if let Some(n) = by_name
        && let Some(s) = set.find_syntax_by_name(n)
    {
        return Some((s, true));
    }
    let ext = name.rsplit('.').next().filter(|e| *e != name)?;
    // The bundled syntax set predates some languages; fall back to the
    // closest grammar rather than showing plain text.
    let as_is = [ext];
    let candidates: &[&str] = match ext {
        "ts" | "tsx" | "mts" | "cts" => &["ts", "js"],
        "jsx" | "mjs" | "cjs" => &["js"],
        "yml" => &["yaml"],
        "toml" => &["toml", "ini"],
        "md" | "mdx" => &["md"],
        "kt" | "kts" => &["kt", "java"],
        "swift" => &["swift", "c"],
        "ps1" | "psm1" => &["ps1", "sh"],
        "vue" | "svelte" => &["html"],
        _ => &as_is,
    };
    candidates
        .iter()
        .enumerate()
        .find_map(|(i, token)| set.find_syntax_by_token(token).map(|s| (s, i == 0)))
}

/// The language name for a path, when a matching grammar exists.
pub fn language_for(path: &str) -> Option<String> {
    syntax_for(path).and_then(|(s, exact)| exact.then(|| s.name.clone()))
}

/// Per hunk, per line, the coloured spans (char offsets into the display text).
pub fn highlight(diff: &FileDiff, path: &str, dark: bool) -> Vec<Vec<Vec<Span>>> {
    let total: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
    let empty = || {
        diff.hunks
            .iter()
            .map(|h| vec![Vec::new(); h.lines.len()])
            .collect()
    };
    if total > MAX_LINES || diff.binary || diff.too_large {
        return empty();
    }
    let Some((syntax, _)) = syntax_for(path) else {
        return empty();
    };
    let theme = &themes().themes[if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    }];
    let set = syntaxes();
    let mut out = Vec::with_capacity(diff.hunks.len());
    for hunk in &diff.hunks {
        // Each hunk starts fresh: state from earlier hunks is unknown anyway.
        let mut hl = HighlightLines::new(syntax, theme);
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in &hunk.lines {
            if line.kind == LineKind::NoNewline {
                lines.push(Vec::new());
                continue;
            }
            let text = format!("{}\n", line.text);
            let spans = match hl.highlight_line(&text, set) {
                Ok(regions) => {
                    let mut spans = Vec::with_capacity(regions.len());
                    let mut pos = 0u32;
                    for (style, piece) in regions {
                        let n = piece.trim_end_matches('\n').chars().count() as u32;
                        if n > 0 {
                            let c = style.foreground;
                            spans.push(Span {
                                start: pos,
                                end: pos + n,
                                color: Color32::from_rgb(c.r, c.g, c.b),
                            });
                        }
                        pos += n;
                    }
                    spans
                }
                Err(_) => Vec::new(),
            };
            lines.push(spans);
        }
        out.push(lines);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_languages() {
        assert_eq!(language_for("src/main.rs").as_deref(), Some("Rust"));
        assert_eq!(language_for("x.py").as_deref(), Some("Python"));
        let swift = highlight(
            &gitvis_core::diff::parse(b"diff --git a/a.swift b/a.swift\n--- a/a.swift\n+++ b/a.swift\n@@ -1,1 +1,1 @@\n-let a = 1\n+let a = 2\n").remove(0),
            "a.swift",
            true,
        );
        assert!(
            !swift[0][0].is_empty(),
            "coloured through the fallback grammar"
        );
        assert!(language_for("weird.unknownext").is_none());
    }

    #[test]
    fn spans_cover_text() {
        let diff = gitvis_core::diff::parse(
            b"diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-fn a() {}\n+fn b() { let x = 1; }\n",
        )
        .remove(0);
        let spans = highlight(&diff, "x.rs", true);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].len(), 2);
        let last = spans[0][1].last().unwrap();
        assert_eq!(last.end as usize, "fn b() { let x = 1; }".chars().count());
    }
}
