use chrono::{DateTime, Local, TimeZone};

pub fn now_secs() -> i64 {
    Local::now().timestamp()
}

pub fn absolute(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

pub fn absolute_long(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%A, %d %B %Y at %H:%M").to_string(),
        _ => String::new(),
    }
}

/// `3m ago`, `yesterday`, `12 Mar`, `2023-01-05`.
pub fn relative(secs: i64, now: i64) -> String {
    let d = now - secs;
    if d < 60 {
        return "just now".into();
    }
    if d < 3600 {
        return format!("{}m ago", d / 60);
    }
    if d < 86_400 {
        return format!("{}h ago", d / 3600);
    }
    if d < 2 * 86_400 {
        return "yesterday".into();
    }
    if d < 14 * 86_400 {
        return format!("{}d ago", d / 86_400);
    }
    let t: Option<DateTime<Local>> = Local.timestamp_opt(secs, 0).single();
    let now_t: Option<DateTime<Local>> = Local.timestamp_opt(now, 0).single();
    match (t, now_t) {
        (Some(t), Some(n)) if t.format("%Y").to_string() == n.format("%Y").to_string() => {
            t.format("%d %b").to_string()
        }
        (Some(t), _) => t.format("%Y-%m-%d").to_string(),
        _ => String::new(),
    }
}

/// `HH:MM:SS` in the local zone.
pub fn clock(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%H:%M:%S").to_string(),
        _ => String::new(),
    }
}

/// Wall-clock now as `HH:MM:SS.mmm`, for log entries.
pub fn now_clock_ms() -> String {
    Local::now().format("%H:%M:%S%.3f").to_string()
}

/// `4m 08s`, `1h 02m`, `2d 3h`, `12s`.
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h {:02}m", s / 3600, (s / 60) % 60)
    } else {
        format!("{}d {}h", s / 86_400, (s / 3600) % 24)
    }
}

/// `850 ms`, `1.2 s`, `2m 05s`.
pub fn millis(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f32 / 1000.0)
    } else {
        duration((ms / 1000) as i64)
    }
}

/// `1.5 MB`, `12.3 KB`, `640 B`.
pub fn bytes(n: usize) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n < K {
        format!("{n} B")
    } else if n < K * K {
        format!("{:.1} KB", n / K)
    } else {
        format!("{:.1} MB", n / (K * K))
    }
}

/// `12,345` with thousands separators.
pub fn count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn initials(name: &str) -> String {
    let mut parts = name.split_whitespace().filter_map(|p| p.chars().next());
    let first = parts.next();
    let last = parts.next_back();
    match (first, last) {
        (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

/// Split `src/a/b.rs` into (`src/a/`, `b.rs`).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_buckets() {
        let now = 1_700_000_000;
        assert_eq!(relative(now - 10, now), "just now");
        assert_eq!(relative(now - 300, now), "5m ago");
        assert_eq!(relative(now - 7200, now), "2h ago");
        assert_eq!(relative(now - 100_000, now), "yesterday");
        assert_eq!(relative(now - 5 * 86_400, now), "5d ago");
    }

    #[test]
    fn durations_and_sizes() {
        assert_eq!(duration(12), "12s");
        assert_eq!(duration(248), "4m 08s");
        assert_eq!(duration(3720), "1h 02m");
        assert_eq!(duration(90_000), "1d 1h");
        assert_eq!(millis(850), "850 ms");
        assert_eq!(millis(1_234), "1.2 s");
        assert_eq!(millis(125_000), "2m 05s");
        assert_eq!(bytes(640), "640 B");
        assert_eq!(bytes(12_600), "12.3 KB");
        assert_eq!(bytes(1_500_000), "1.4 MB");
        assert_eq!(count(12_345), "12,345");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000_000), "1,000,000");
    }

    #[test]
    fn initials_and_paths() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("ada"), "A");
        assert_eq!(split_path("src/a/b.rs"), ("src/a/", "b.rs"));
        assert_eq!(split_path("README"), ("", "README"));
    }
}
