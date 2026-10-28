//! The file list the `/` scope searches.

use std::path::Path;

/// The command line [`list`] runs, for the activity log.
pub const COMMAND: &str = "git ls-files -z";

/// Paths of the files in the index (`git ls-files -z`: no quoting, so
/// non-ASCII names come through as they are).
pub fn list(workdir: &Path, commands: &mut Vec<String>) -> Result<Vec<String>, String> {
    commands.push(COMMAND.to_owned());
    let out = crate::cli::run(workdir, &["ls-files", "-z"], None).map_err(|e| e.to_string())?;
    Ok(parse(&out.stdout))
}

/// Split `-z` output into paths.
pub fn parse(out: &[u8]) -> Vec<String> {
    out.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_paths() {
        assert_eq!(
            parse(b"a.rs\0src/b c.rs\0d\xc3\xa9j\xc3\xa0.md\0"),
            ["a.rs", "src/b c.rs", "déjà.md"]
        );
        assert!(parse(b"").is_empty());
    }
}
