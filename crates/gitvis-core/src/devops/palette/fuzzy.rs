//! Case-insensitive fuzzy matching with scoring and matched positions.
//!
//! A query is split on whitespace into tokens; every token has to appear in
//! the text as a subsequence (letters in order, gaps allowed). Each token is
//! aligned by a small dynamic programme that prefers, in this order of
//! weight: matches at the very start of the text, at word starts (after a
//! space, `/`, `-`, `_`, `.`, …) and camelCase humps, runs of consecutive
//! characters, and short gaps. Equal scores are ordered by the caller
//! (shorter texts first, see `rank`).
//!
//! The text is prepared once ([`Haystack`]: lower-cased characters, the
//! positional bonus of every character and a character-set mask), so a
//! search over tens of thousands of items only touches what it needs:
//! a mask test, a forward scan, and the alignment inside the matched window.

/// Points for every matched character.
pub const SCORE_MATCH: i32 = 16;
/// Penalty for opening a gap between two matched characters.
pub const GAP_START: i32 = 3;
/// Penalty for every further skipped character.
pub const GAP_EXT: i32 = 1;
/// First character of the text.
pub const BONUS_START: i32 = 12;
/// Character after whitespace.
pub const BONUS_WHITESPACE: i32 = 10;
/// Character after a path separator.
pub const BONUS_SLASH: i32 = 9;
/// Character after other punctuation (`-`, `_`, `.`, `:`, `(`, …).
pub const BONUS_BOUNDARY: i32 = 8;
/// An upper-case letter after a lower-case one, a digit after a letter.
pub const BONUS_CAMEL: i32 = 7;
/// Minimum bonus of a character that directly follows the previous match.
pub const BONUS_CONSECUTIVE: i32 = 5;
/// The positional bonus of a token's first character counts this often.
pub const FIRST_CHAR_MULTIPLIER: i32 = 2;
/// A token matched in the keywords rather than the title scores this much less.
pub const KEYWORD_PENALTY: i32 = 12;

/// Longest token the alignment considers (longer ones are cut).
const MAX_TOKEN: usize = 48;
/// Above this many cells the alignment falls back to the greedy match.
const MAX_CELLS: usize = 64 * 1024;
const NONE: i32 = i32::MIN / 4;

/// A text prepared for matching.
#[derive(Clone, Debug, Default)]
pub struct Haystack {
    /// Lower-cased characters (one per original character).
    chars: Box<[char]>,
    /// Positional bonus of each character.
    bonus: Box<[i8]>,
    /// Character classes present (see [`mask_of`]).
    mask: u64,
}

impl Haystack {
    pub fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut bonus = Vec::with_capacity(text.len());
        let mut mask = 0u64;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            let lower = lower(c);
            mask |= mask_of(lower);
            chars.push(lower);
            bonus.push(position_bonus(prev, c) as i8);
            prev = Some(c);
        }
        Self {
            chars: chars.into_boxed_slice(),
            bonus: bonus.into_boxed_slice(),
            mask,
        }
    }

    /// Length in characters.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
}

/// One whitespace-separated part of a query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    chars: Vec<char>,
    mask: u64,
    /// Lower-case text, for prefix tests against exact keys (SHAs).
    text: String,
}

impl Token {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
}

/// A parsed query: lower-cased tokens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    tokens: Vec<Token>,
    raw: String,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let tokens = text
            .split_whitespace()
            .map(|t| {
                let chars: Vec<char> = t.chars().map(lower).take(MAX_TOKEN).collect();
                let mask = chars.iter().fold(0u64, |m, c| m | mask_of(*c));
                let text: String = chars.iter().collect();
                Token { chars, mask, text }
            })
            .collect();
        Self {
            tokens,
            raw: text.to_owned(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// The text the query was built from.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Every text matching `self` also matches `prev` (the query only grew
    /// at its end), so a search may start from `prev`'s matches.
    pub fn narrows(&self, prev: &Query) -> bool {
        !prev.is_empty() && self.raw.starts_with(prev.raw.as_str())
    }
}

/// Scratch space for the alignment, reused between calls.
#[derive(Default)]
pub struct Matcher {
    score: Vec<i32>,
    back: Vec<u16>,
    run: Vec<i8>,
    first: Vec<usize>,
    last: Vec<usize>,
    tmp: Vec<u32>,
}

impl Matcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Score of `query` against `hay` (`None` when a token does not match).
    pub fn score(&mut self, query: &Query, hay: &Haystack) -> Option<i32> {
        let mut total = 0;
        for t in &query.tokens {
            total += self.token(t, hay, None)?;
        }
        Some(total)
    }

    /// Score and the matched character indices (sorted, unique).
    pub fn indices(&mut self, query: &Query, hay: &Haystack, out: &mut Vec<u32>) -> Option<i32> {
        out.clear();
        let mut total = 0;
        let mut tmp = std::mem::take(&mut self.tmp);
        for t in &query.tokens {
            tmp.clear();
            match self.token(t, hay, Some(&mut tmp)) {
                Some(s) => total += s,
                None => {
                    self.tmp = tmp;
                    out.clear();
                    return None;
                }
            }
            out.extend_from_slice(&tmp);
        }
        self.tmp = tmp;
        out.sort_unstable();
        out.dedup();
        Some(total)
    }

    /// Align one token. `out` receives the matched indices when given.
    pub fn token(&mut self, t: &Token, hay: &Haystack, out: Option<&mut Vec<u32>>) -> Option<i32> {
        let pat = &t.chars;
        let text = &hay.chars;
        let (m, n) = (pat.len(), text.len());
        if m == 0 {
            return Some(0);
        }
        if m > n || t.mask & !hay.mask != 0 {
            return None;
        }
        // Earliest position of every pattern character (forward greedy).
        self.first.clear();
        let mut j = 0;
        for &pc in pat {
            while j < n && text[j] != pc {
                j += 1;
            }
            if j == n {
                return None;
            }
            self.first.push(j);
            j += 1;
        }
        // Latest position of every pattern character (backward greedy).
        self.last.clear();
        self.last.resize(m, 0);
        let mut j = n;
        for i in (0..m).rev() {
            loop {
                j -= 1;
                if text[j] == pat[i] {
                    break;
                }
            }
            self.last[i] = j;
        }
        let lo = self.first[0];
        let w = self.last[m - 1] - lo + 1;
        if m == 1 {
            // The best single position: highest bonus, earliest on ties.
            let mut best = (NONE, 0usize);
            for j in self.first[0]..=self.last[0] {
                if text[j] == pat[0] {
                    let s = SCORE_MATCH + hay.bonus[j] as i32 * FIRST_CHAR_MULTIPLIER;
                    if s > best.0 {
                        best = (s, j);
                    }
                }
            }
            if let Some(out) = out {
                out.push(best.1 as u32);
            }
            return Some(best.0);
        }
        if m * w > MAX_CELLS || w > u16::MAX as usize {
            return Some(self.greedy(pat, hay, out));
        }
        self.score.clear();
        self.score.resize(m * w, NONE);
        self.back.clear();
        self.back.resize(m * w, u16::MAX);
        self.run.clear();
        self.run.resize(m * w, 0);
        // Row 0: the first character anywhere in its range.
        for j in self.first[0]..=self.last[0] {
            if text[j] == pat[0] {
                let b = hay.bonus[j] as i32;
                self.score[j - lo] = SCORE_MATCH + b * FIRST_CHAR_MULTIPLIER;
                self.run[j - lo] = b as i8;
            }
        }
        for i in 1..m {
            let (row, prev) = (i * w, (i - 1) * w);
            // Running best of `score[i-1][k] + GAP_EXT * k` over k <= j - 2.
            let mut best = NONE;
            let mut best_k = 0usize;
            let start = self.first[i];
            // Seed the running best with every k up to start - 2.
            let seed_from = self.first[i - 1];
            for k in seed_from..start.saturating_sub(1) {
                let s = self.score[prev + k - lo];
                if s > NONE && s + GAP_EXT * k as i32 > best {
                    best = s + GAP_EXT * k as i32;
                    best_k = k;
                }
            }
            for j in start..=self.last[i] {
                if j >= 2 && j - 2 >= seed_from && j - 2 >= start.saturating_sub(1) {
                    let k = j - 2;
                    let s = self.score[prev + k - lo];
                    if s > NONE && s + GAP_EXT * k as i32 > best {
                        best = s + GAP_EXT * k as i32;
                        best_k = k;
                    }
                }
                if text[j] != pat[i] {
                    continue;
                }
                let b = hay.bonus[j] as i32;
                let mut cell = NONE;
                let mut from = u16::MAX;
                let mut run = 0i8;
                // Directly after the previous character.
                if j > lo {
                    let d = self.score[prev + j - 1 - lo];
                    if d > NONE {
                        let chunk = self.run[prev + j - 1 - lo] as i32;
                        let bonus = b.max(chunk).max(BONUS_CONSECUTIVE);
                        cell = d + SCORE_MATCH + bonus;
                        from = (j - 1 - lo) as u16;
                        run = if b >= BONUS_BOUNDARY && b > chunk {
                            b as i8
                        } else {
                            chunk as i8
                        };
                    }
                }
                // After a gap.
                if best > NONE {
                    let g = best - GAP_EXT * (j as i32 - 2) - GAP_START + SCORE_MATCH + b;
                    if g > cell {
                        cell = g;
                        from = (best_k - lo) as u16;
                        run = b as i8;
                    }
                }
                self.score[row + j - lo] = cell;
                self.back[row + j - lo] = from;
                self.run[row + j - lo] = run;
            }
        }
        let row = (m - 1) * w;
        let mut best = (NONE, 0usize);
        for j in self.first[m - 1]..=self.last[m - 1] {
            let s = self.score[row + j - lo];
            if s > best.0 {
                best = (s, j);
            }
        }
        if best.0 <= NONE {
            // Cannot happen for a subsequence, but stay safe.
            return Some(self.greedy(pat, hay, out));
        }
        if let Some(out) = out {
            let start = out.len();
            let mut col = best.1 - lo;
            for i in (0..m).rev() {
                out.push((col + lo) as u32);
                if i > 0 {
                    col = self.back[i * w + col] as usize;
                }
            }
            out[start..].reverse();
        }
        Some(best.0)
    }

    /// Score of the leftmost greedy alignment (very long texts).
    fn greedy(&mut self, pat: &[char], hay: &Haystack, out: Option<&mut Vec<u32>>) -> i32 {
        let mut score = 0;
        let mut prev: Option<usize> = None;
        let mut chunk = 0i32;
        let mut j = 0;
        let mut idx = Vec::with_capacity(pat.len());
        for (i, &pc) in pat.iter().enumerate() {
            while hay.chars[j] != pc {
                j += 1;
            }
            let b = hay.bonus[j] as i32;
            score += SCORE_MATCH;
            match prev {
                Some(p) if p + 1 == j => score += b.max(chunk).max(BONUS_CONSECUTIVE),
                Some(p) => {
                    score += b - GAP_START - GAP_EXT * (j - p - 2) as i32;
                    chunk = b;
                }
                None => {
                    score += b * if i == 0 { FIRST_CHAR_MULTIPLIER } else { 1 };
                    chunk = b;
                }
            }
            idx.push(j as u32);
            prev = Some(j);
            j += 1;
        }
        if let Some(out) = out {
            out.extend(idx);
        }
        score
    }
}

/// One-shot convenience: score and matched character indices of `query`
/// in `text`.
pub fn fuzzy_match(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q = Query::new(query);
    let h = Haystack::new(text);
    let mut m = Matcher::new();
    let mut idx = Vec::new();
    let s = m.indices(&q, &h, &mut idx)?;
    Some((s, idx.into_iter().map(|i| i as usize).collect()))
}

fn lower(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Character class bit: letters and digits get their own bit, other ASCII
/// shares 20 bits, everything else one.
fn mask_of(c: char) -> u64 {
    match c {
        'a'..='z' => 1 << (c as u32 - 'a' as u32),
        '0'..='9' => 1 << (26 + c as u32 - '0' as u32),
        c if c.is_ascii() => 1 << (36 + (c as u32 % 20)),
        _ => 1 << 63,
    }
}

fn is_separator(c: char) -> bool {
    matches!(
        c,
        '-' | '_'
            | '.'
            | ':'
            | ','
            | ';'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '#'
            | '@'
            | '!'
            | '~'
            | '"'
            | '\''
            | '`'
            | '='
            | '+'
            | '*'
            | '|'
            | '&'
    )
}

/// Bonus of `c` given the character before it.
fn position_bonus(prev: Option<char>, c: char) -> i32 {
    let Some(p) = prev else {
        return BONUS_START;
    };
    if !c.is_alphanumeric() {
        return 0;
    }
    if p.is_whitespace() {
        BONUS_WHITESPACE
    } else if p == '/' || p == '\\' {
        BONUS_SLASH
    } else if is_separator(p) {
        BONUS_BOUNDARY
    } else if (p.is_lowercase() && c.is_uppercase())
        || (p.is_alphabetic() && c.is_numeric())
        || (p.is_numeric() && c.is_alphabetic())
    {
        BONUS_CAMEL
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, t: &str) -> Option<i32> {
        fuzzy_match(q, t).map(|(s, _)| s)
    }

    fn idx(q: &str, t: &str) -> Vec<usize> {
        fuzzy_match(q, t).map(|(_, i)| i).unwrap_or_default()
    }

    #[test]
    fn subsequence_case_insensitive() {
        assert!(score("psh", "Push").is_some());
        assert!(score("PUSH", "push to origin").is_some());
        assert!(score("hsup", "push").is_none());
        assert!(score("pushx", "push").is_none());
        assert!(score("", "anything") == Some(0));
        assert!(score("a", "").is_none());
    }

    #[test]
    fn every_token_must_match() {
        assert!(score("merge feat", "Merge feature into main").is_some());
        assert!(score("feat merge", "Merge feature into main").is_some());
        assert!(score("merge zzz", "Merge feature into main").is_none());
    }

    #[test]
    fn prefix_beats_middle() {
        let a = score("push", "Push").unwrap();
        let b = score("push", "Stash and push").unwrap();
        let c = score("push", "Unpushed commits").unwrap();
        assert!(a > b, "{a} > {b}");
        assert!(b > c, "{b} > {c}");
    }

    #[test]
    fn word_starts_beat_scattered_letters() {
        // f…h at word starts versus the same letters inside words.
        let a = score("fh", "file history").unwrap();
        let b = score("fh", "of the hash").unwrap();
        assert!(a > b, "{a} > {b}");
        let a = score("sr", "search results").unwrap();
        let b = score("sr", "surprise").unwrap();
        assert!(a > b, "{a} > {b}");
    }

    #[test]
    fn camel_case_humps() {
        let a = score("fh", "FileHistory").unwrap();
        let b = score("fh", "filehash").unwrap();
        assert!(a > b, "{a} > {b}");
        assert_eq!(idx("fh", "FileHistory"), vec![0, 4]);
    }

    #[test]
    fn consecutive_beats_gaps() {
        let a = score("main", "main").unwrap();
        let b = score("main", "m-a-i-n").unwrap();
        assert!(a > b, "{a} > {b}");
        let a = score("tag", "tags/v1").unwrap();
        let b = score("tag", "the angry goat").unwrap();
        assert!(a > b, "{a} > {b}");
    }

    #[test]
    fn path_segments() {
        // The file name segment of a path wins over scattered letters.
        let a = score("main", "src/main.rs").unwrap();
        let b = score("main", "src/my_animation.rs").unwrap();
        assert!(a > b, "{a} > {b}");
        assert_eq!(idx("main", "src/main.rs"), vec![4, 5, 6, 7]);
    }

    #[test]
    fn indices_pick_the_best_alignment() {
        // Greedy would take the first `a`; the alignment takes the run.
        assert_eq!(idx("ab", "xa_xab"), vec![4, 5]);
        // …unless the first `a` starts the text: a prefix is worth a gap.
        assert_eq!(idx("ab", "a_xab"), vec![0, 4]);
        // Word starts over earlier letters inside a word.
        assert_eq!(idx("sr", "search_results"), vec![0, 7]);
        assert_eq!(idx("gp", "git push"), vec![0, 4]);
        // Two tokens: union of both alignments, sorted.
        assert_eq!(idx("push orig", "Push to origin"), vec![0, 1, 2, 3, 8, 9, 10, 11]);
        // Indices are character indices, not bytes.
        assert_eq!(idx("é", "café"), vec![3]);
        assert_eq!(idx("ü", "ÜBER"), vec![0]);
    }

    #[test]
    fn single_char_prefers_bonus() {
        assert_eq!(idx("h", "push history"), vec![5]);
        assert_eq!(idx("p", "app/Page"), vec![4]);
    }

    #[test]
    fn narrowing() {
        let a = Query::new("ma");
        let b = Query::new("mai");
        let c = Query::new("ma x");
        let d = Query::new("m");
        assert!(b.narrows(&a));
        assert!(c.narrows(&a));
        assert!(!d.narrows(&a));
        assert!(!a.narrows(&Query::new("")));
    }

    #[test]
    fn long_texts_fall_back_to_greedy() {
        let long: String = "x".repeat(5000) + "needle";
        let q = Query::new("needle with spaces and many more letters to fill");
        let h = Haystack::new(&long);
        assert!(Matcher::new().score(&q, &h).is_none());
        let (s, i) = fuzzy_match("needle", &long).unwrap();
        assert!(s > 0);
        assert_eq!(i, (5000..5006).collect::<Vec<_>>());
        let long2: String = "ab".repeat(40_000);
        assert!(fuzzy_match("abababab", &long2).is_some());
    }
}
