//! The cross-repository home page: the GitHub inbox (notifications with
//! reasons, review requests, mentions, assigned issues and pull requests,
//! failed runs on the user's branches; mark as read) and the per-repository
//! summary rows of the dashboard.
//!
//! - [`notify`]: `GET notifications`, the notification model, reasons,
//!   filters, grouping by repository, and the mark-as-read requests.
//! - [`search`]: one GraphQL document with four searches (review requests,
//!   mentions, assigned, my open pull requests) plus the viewer's login.
//! - [`runs`]: failed workflow runs on the user's branches, from the runs
//!   each open repository already loaded (no extra calls).
//! - [`fetch`]: `git fetch --all --prune` in each open repository.

pub mod fetch;
pub mod notify;
pub mod runs;
pub mod search;

/// The items of one repository in a grouped list.
#[derive(Debug)]
pub struct RepoGroup<'a, T> {
    /// `owner/repo` as the first item spelled it.
    pub repo: String,
    pub items: Vec<&'a T>,
}

/// Group items by repository (case-insensitive). Groups come in the order
/// their first item appears, items keep their order: with input sorted
/// newest first, the repository with the newest item leads.
pub fn group_by_repo<'a, T>(items: &[&'a T], repo: impl Fn(&T) -> &str) -> Vec<RepoGroup<'a, T>> {
    let mut out: Vec<RepoGroup<'a, T>> = Vec::new();
    for item in items {
        let name = repo(item);
        match out.iter_mut().find(|g| g.repo.eq_ignore_ascii_case(name)) {
            Some(g) => g.items.push(item),
            None => out.push(RepoGroup {
                repo: name.to_owned(),
                items: vec![item],
            }),
        }
    }
    out
}

/// Days since 1970-01-01 to a civil date (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch seconds as an ISO 8601 UTC timestamp, `2026-10-26T14:05:09Z`.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::parse_time;

    #[test]
    fn groups_keep_first_appearance_order() {
        let items = [("a/x", 1), ("b/y", 2), ("A/X", 3), ("c/z", 4), ("b/y", 5)];
        let refs: Vec<&(&str, i32)> = items.iter().collect();
        let groups = group_by_repo(&refs, |i| i.0);
        let names: Vec<&str> = groups.iter().map(|g| g.repo.as_str()).collect();
        assert_eq!(names, vec!["a/x", "b/y", "c/z"]);
        let firsts: Vec<i32> = groups[0].items.iter().map(|i| i.1).collect();
        assert_eq!(firsts, vec![1, 3]);
        assert_eq!(groups[1].items.len(), 2);
    }

    #[test]
    fn iso_round_trips() {
        for s in [
            "2026-10-26T14:05:09Z",
            "1970-01-01T00:00:00Z",
            "2024-02-29T23:59:59Z",
            "2000-03-01T00:00:01Z",
        ] {
            let secs = parse_time(s).unwrap();
            assert_eq!(iso_utc(secs), s);
        }
    }
}
