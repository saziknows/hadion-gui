//! Query prefixes: the first character picks what the palette searches.

/// What a query searches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// No prefix: everything, ranked together.
    #[default]
    All,
    /// `>` pages, app actions, repository actions, tasks.
    Commands,
    /// `@` branches and tags.
    Refs,
    /// `#` pull requests and issues.
    Prs,
    /// `/` files of HEAD.
    Files,
    /// `:` commits (SHA prefix or summary).
    Commits,
    /// `!` workflow runs.
    Runs,
    /// `~` open and recent repositories.
    Repos,
    /// `?` the prefix help.
    Help,
}

/// A prefix and what it searches, for the hint and the help list.
pub struct Prefix {
    pub ch: char,
    pub scope: Scope,
    /// `Commands`, `Branches and tags`, …
    pub label: &'static str,
    /// One line about what is listed.
    pub help: &'static str,
}

pub const PREFIXES: [Prefix; 8] = [
    Prefix {
        ch: '>',
        scope: Scope::Commands,
        label: "Commands",
        help: "Pages, app and repository actions, tasks",
    },
    Prefix {
        ch: '@',
        scope: Scope::Refs,
        label: "Branches and tags",
        help: "Check out, jump to, merge, rebase",
    },
    Prefix {
        ch: '#',
        scope: Scope::Prs,
        label: "Pull requests and issues",
        help: "Open on the Review page or on GitHub",
    },
    Prefix {
        ch: '/',
        scope: Scope::Files,
        label: "Files",
        help: "Files of HEAD: history, open, copy the path",
    },
    Prefix {
        ch: ':',
        scope: Scope::Commits,
        label: "Commits",
        help: "SHA prefix or words of the summary",
    },
    Prefix {
        ch: '!',
        scope: Scope::Runs,
        label: "Workflow runs",
        help: "Runs loaded from GitHub Actions",
    },
    Prefix {
        ch: '~',
        scope: Scope::Repos,
        label: "Repositories",
        help: "Switch to an open tab or open a recent one",
    },
    Prefix {
        ch: '?',
        scope: Scope::Help,
        label: "Help",
        help: "These prefixes",
    },
];

impl Scope {
    /// The prefix character (`None` for [`Scope::All`]).
    pub fn prefix(self) -> Option<char> {
        PREFIXES.iter().find(|p| p.scope == self).map(|p| p.ch)
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "Everything",
            s => PREFIXES
                .iter()
                .find(|p| p.scope == s)
                .map(|p| p.label)
                .unwrap_or(""),
        }
    }
}

/// Split the input into its scope and the search text (the prefix and the
/// spaces after it removed). Leading spaces before a prefix are ignored.
pub fn parse(input: &str) -> (Scope, &str) {
    let t = input.trim_start();
    let Some(c) = t.chars().next() else {
        return (Scope::All, "");
    };
    match PREFIXES.iter().find(|p| p.ch == c) {
        Some(p) => (p.scope, t[c.len_utf8()..].trim_start()),
        None => (Scope::All, t),
    }
}

/// The input for `scope` with `text` (`>push`).
pub fn compose(scope: Scope, text: &str) -> String {
    match scope.prefix() {
        Some(c) => format!("{c}{text}"),
        None => text.to_owned(),
    }
}

/// The hint shown in the empty input.
pub fn hint() -> String {
    let parts: Vec<String> = PREFIXES
        .iter()
        .filter(|p| p.scope != Scope::Help)
        .map(|p| format!("{} {}", p.ch, short_label(p.scope)))
        .collect();
    format!("Search everything, or start with  {}", parts.join("   "))
}

/// One word per scope, for the hint.
pub fn short_label(scope: Scope) -> &'static str {
    match scope {
        Scope::All => "everything",
        Scope::Commands => "commands",
        Scope::Refs => "branches",
        Scope::Prs => "PRs",
        Scope::Files => "files",
        Scope::Commits => "commits",
        Scope::Runs => "runs",
        Scope::Repos => "repos",
        Scope::Help => "help",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(parse(""), (Scope::All, ""));
        assert_eq!(parse("push"), (Scope::All, "push"));
        assert_eq!(parse(">push"), (Scope::Commands, "push"));
        assert_eq!(parse("> push "), (Scope::Commands, "push "));
        assert_eq!(parse("  @main"), (Scope::Refs, "main"));
        assert_eq!(parse("#12"), (Scope::Prs, "12"));
        assert_eq!(parse("/src/main"), (Scope::Files, "src/main"));
        assert_eq!(parse(":a1b2"), (Scope::Commits, "a1b2"));
        assert_eq!(parse("!ci"), (Scope::Runs, "ci"));
        assert_eq!(parse("~gitvis"), (Scope::Repos, "gitvis"));
        assert_eq!(parse("?"), (Scope::Help, ""));
        assert_eq!(parse(">"), (Scope::Commands, ""));
        // Only the first character is a prefix.
        assert_eq!(parse("a>b"), (Scope::All, "a>b"));
    }

    #[test]
    fn compose_round_trips() {
        for p in &PREFIXES {
            let s = compose(p.scope, "x y");
            assert_eq!(parse(&s), (p.scope, "x y"));
        }
        assert_eq!(compose(Scope::All, "x"), "x");
        assert_eq!(Scope::Files.prefix(), Some('/'));
        assert_eq!(Scope::All.prefix(), None);
    }

    #[test]
    fn hint_names_every_prefix() {
        let h = hint();
        for p in PREFIXES.iter().filter(|p| p.scope != Scope::Help) {
            assert!(h.contains(p.ch), "{h}");
        }
    }
}
