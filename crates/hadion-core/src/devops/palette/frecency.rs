//! Frecency: recently and frequently used palette items rank higher.
//!
//! Every use adds one point that halves every [`HALF_LIFE_SECS`]; an
//! item's value is the decayed sum of its uses, kept as one number per item
//! (`score` at `last`), so recording and reading are O(1) and the list stays
//! small ([`MAX_USES`], the weakest entries are dropped first).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A use counts half as much after a week.
pub const HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 3600.0;
/// Entries kept in the settings file.
pub const MAX_USES: usize = 200;
/// Most points frecency adds to a match score.
pub const MAX_BONUS: i32 = 36;

/// One remembered item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Use {
    /// Item key (`ref:refs/heads/main`), plus [`VERB_SEP`] and the action
    /// when one other than the item's default was run.
    pub key: String,
    /// Working directory of the repository the item belongs to; empty for
    /// app-wide items (commands, repositories).
    pub repo: String,
    /// What the row showed when it was used.
    pub title: String,
    pub subtitle: String,
    pub count: u32,
    /// Decayed value at `last`.
    pub score: f64,
    /// Unix seconds of the last use.
    pub last: i64,
}

impl Use {
    /// The decayed value at `now`.
    pub fn value(&self, now: i64) -> f64 {
        decay(self.score, now - self.last)
    }

    /// The item key without the action suffix.
    pub fn entity(&self) -> &str {
        entity(&self.key)
    }

    /// The action suffix, if any.
    pub fn verb(&self) -> Option<&str> {
        self.key.split_once(VERB_SEP).map(|(_, v)| v)
    }

    /// Belongs to the repository `repo` (or to the whole app).
    pub fn visible_in(&self, repo: &str) -> bool {
        self.repo.is_empty() || self.repo == repo
    }
}

/// `score` after `age` seconds.
pub fn decay(score: f64, age: i64) -> f64 {
    let age = age.max(0) as f64;
    score * 0.5f64.powf(age / HALF_LIFE_SECS)
}

/// Separates an item key from the action that was run on it. A control
/// character, because branch names and paths may contain `#`.
pub const VERB_SEP: char = '\u{1f}';

/// The key of action `verb` on item `key`.
pub fn with_verb(key: &str, verb: &str) -> String {
    format!("{key}{VERB_SEP}{verb}")
}

/// The item key without the action suffix.
pub fn entity(key: &str) -> &str {
    key.split_once(VERB_SEP).map(|(e, _)| e).unwrap_or(key)
}

/// Record a use at `now`.
pub fn record(uses: &mut Vec<Use>, key: &str, repo: &str, title: &str, subtitle: &str, now: i64) {
    match uses.iter_mut().find(|u| u.key == key && u.repo == repo) {
        Some(u) => {
            u.score = u.value(now) + 1.0;
            u.count = u.count.saturating_add(1);
            u.last = now;
            u.title = title.to_owned();
            u.subtitle = subtitle.to_owned();
        }
        None => uses.push(Use {
            key: key.to_owned(),
            repo: repo.to_owned(),
            title: title.to_owned(),
            subtitle: subtitle.to_owned(),
            count: 1,
            score: 1.0,
            last: now,
        }),
    }
    if uses.len() > MAX_USES {
        // Drop the weakest, never the one just used.
        let weakest = uses
            .iter()
            .enumerate()
            .filter(|(_, u)| !(u.key == key && u.repo == repo))
            .min_by(|(_, a), (_, b)| a.value(now).total_cmp(&b.value(now)))
            .map(|(i, _)| i);
        if let Some(i) = weakest {
            uses.remove(i);
        }
    }
}

/// Forget one item (every action of it).
pub fn forget(uses: &mut Vec<Use>, entity_key: &str, repo: &str) {
    uses.retain(|u| !(u.entity() == entity_key && u.repo == repo));
}

/// Points added to a match score for an item used with this value.
pub fn bonus(value: f64) -> i32 {
    if value <= 0.0 {
        return 0;
    }
    ((10.0 * (1.0 + value).ln()) as i32).min(MAX_BONUS)
}

/// The most recent uses visible in `repo`, newest first.
pub fn recent<'a>(uses: &'a [Use], repo: &str, n: usize) -> Vec<&'a Use> {
    let mut v: Vec<&Use> = uses.iter().filter(|u| u.visible_in(repo)).collect();
    v.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.key.cmp(&b.key)));
    v.truncate(n);
    v
}

/// Value per item (actions summed) for the items visible in `repo`.
pub fn values(uses: &[Use], repo: &str, now: i64) -> HashMap<String, f64> {
    let mut map: HashMap<String, f64> = HashMap::new();
    for u in uses.iter().filter(|u| u.visible_in(repo)) {
        *map.entry(u.entity().to_owned()).or_default() += u.value(now);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 3600;

    #[test]
    fn decay_halves_every_week() {
        assert!((decay(1.0, 0) - 1.0).abs() < 1e-9);
        assert!((decay(1.0, 7 * DAY) - 0.5).abs() < 1e-9);
        assert!((decay(4.0, 14 * DAY) - 1.0).abs() < 1e-9);
        // Clock skew never makes a use worth more.
        assert!((decay(1.0, -DAY) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn recording_accumulates_with_decay() {
        let mut uses = Vec::new();
        record(&mut uses, "cmd:fetch", "", "Fetch", "Git", 0);
        record(&mut uses, "cmd:fetch", "", "Fetch", "Git", 7 * DAY);
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].count, 2);
        // 1 decayed to 0.5, plus the new use.
        assert!((uses[0].value(7 * DAY) - 1.5).abs() < 1e-9);
        assert!((uses[0].value(14 * DAY) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn frequent_beats_once_recent_until_it_fades() {
        let mut uses = Vec::new();
        for d in 0..5 {
            record(&mut uses, "cmd:pull", "", "Pull", "", d * DAY);
        }
        record(&mut uses, "cmd:push", "", "Push", "", 5 * DAY);
        let v = values(&uses, "", 5 * DAY);
        assert!(v["cmd:pull"] > v["cmd:push"]);
        // Two months later one fresh use outweighs the old habit.
        record(&mut uses, "cmd:push", "", "Push", "", 65 * DAY);
        let v = values(&uses, "", 65 * DAY);
        assert!(v["cmd:push"] > v["cmd:pull"]);
    }

    #[test]
    fn actions_sum_into_their_item_and_repos_stay_apart() {
        let mut uses = Vec::new();
        record(&mut uses, "ref:refs/heads/main", "C:/a", "main", "", 0);
        let merge = with_verb("ref:refs/heads/main", "merge");
        record(&mut uses, &merge, "C:/a", "main", "", 0);
        record(&mut uses, "ref:refs/heads/main", "C:/b", "main", "", 0);
        record(&mut uses, "cmd:fetch", "", "Fetch", "", 0);
        let a = values(&uses, "C:/a", 0);
        assert!((a["ref:refs/heads/main"] - 2.0).abs() < 1e-9);
        assert!(a.contains_key("cmd:fetch"));
        let b = values(&uses, "C:/b", 0);
        assert!((b["ref:refs/heads/main"] - 1.0).abs() < 1e-9);
        assert_eq!(uses[1].verb(), Some("merge"));
        assert_eq!(uses[1].entity(), "ref:refs/heads/main");
        forget(&mut uses, "ref:refs/heads/main", "C:/a");
        assert_eq!(uses.len(), 2);
    }

    #[test]
    fn recent_is_newest_first_and_scoped() {
        let mut uses = Vec::new();
        record(&mut uses, "cmd:a", "", "A", "", 10);
        record(&mut uses, "cmd:b", "", "B", "", 30);
        record(&mut uses, "file:x", "C:/other", "x", "", 40);
        record(&mut uses, "cmd:c", "", "C", "", 20);
        let r: Vec<&str> = recent(&uses, "C:/repo", 10)
            .iter()
            .map(|u| u.key.as_str())
            .collect();
        assert_eq!(r, ["cmd:b", "cmd:c", "cmd:a"]);
        assert_eq!(recent(&uses, "C:/repo", 2).len(), 2);
    }

    #[test]
    fn capped_by_dropping_the_weakest() {
        let mut uses = Vec::new();
        record(&mut uses, "cmd:keep", "", "", "", 0);
        record(&mut uses, "cmd:keep", "", "", "", 0);
        for i in 0..MAX_USES + 10 {
            record(&mut uses, &format!("cmd:{i}"), "", "", "", 1000 + i as i64);
        }
        assert_eq!(uses.len(), MAX_USES);
        assert!(uses.iter().any(|u| u.key == "cmd:keep"));
        // The newest is always kept.
        assert!(
            uses.iter()
                .any(|u| u.key == format!("cmd:{}", MAX_USES + 9))
        );
    }

    #[test]
    fn bonus_grows_slowly_and_is_capped() {
        assert_eq!(bonus(0.0), 0);
        assert!(bonus(1.0) > 0);
        assert!(bonus(10.0) > bonus(1.0));
        assert_eq!(bonus(1e9), MAX_BONUS);
    }
}
