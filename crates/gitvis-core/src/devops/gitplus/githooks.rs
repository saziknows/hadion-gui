//! Git hooks: the hooks folder (`core.hooksPath` or `.git/hooks`, from
//! `git rev-parse --git-path hooks`), each hook file with whether git runs
//! it, enabling and disabling by renaming to and from `NAME.sample`, saving
//! edits, and the gitvis secret check installed as `pre-commit` (an `sh`
//! script that runs `gitvis hook pre-commit`, chaining a hook that was there
//! before).

use std::path::{Path, PathBuf};

use super::{git_probe, resolve_in};

/// Hooks git knows, with when it runs them.
pub const KNOWN: &[(&str, &str)] = &[
    (
        "applypatch-msg",
        "git am, to check or edit the message of a patch",
    ),
    (
        "pre-applypatch",
        "git am, after the patch is applied and before the commit",
    ),
    ("post-applypatch", "git am, after the commit is made"),
    (
        "pre-commit",
        "git commit, before the message is asked for; a failure stops the commit",
    ),
    (
        "pre-merge-commit",
        "git merge, before the merge commit is made",
    ),
    (
        "prepare-commit-msg",
        "git commit, to prepare the default message",
    ),
    (
        "commit-msg",
        "git commit, to check or edit the message; a failure stops the commit",
    ),
    ("post-commit", "git commit, after the commit is made"),
    (
        "pre-rebase",
        "git rebase, before anything changes; a failure stops the rebase",
    ),
    ("post-checkout", "git checkout / switch, after HEAD moved"),
    (
        "post-merge",
        "git merge and git pull, after a successful merge",
    ),
    (
        "pre-push",
        "git push, before anything is sent; a failure stops the push",
    ),
    (
        "post-rewrite",
        "git commit --amend and git rebase, after commits are rewritten",
    ),
    ("reference-transaction", "every reference update"),
    ("pre-auto-gc", "git gc --auto, before it runs"),
    ("post-index-change", "after the index is written"),
    (
        "push-to-checkout",
        "a push into a checked-out branch of this repository",
    ),
    ("sendemail-validate", "git send-email, for each mail"),
    ("fsmonitor-watchman", "the file system monitor integration"),
    ("pre-receive", "a push into this repository (server side)"),
    (
        "update",
        "a push into this repository, once per branch (server side)",
    ),
    ("proc-receive", "a push into this repository (server side)"),
    (
        "post-receive",
        "a push into this repository, afterwards (server side)",
    ),
    (
        "post-update",
        "a push into this repository, afterwards (server side)",
    ),
    ("p4-changelist", "git p4 submit"),
    ("p4-prepare-changelist", "git p4 submit"),
    ("p4-post-changelist", "git p4 submit"),
    ("p4-pre-submit", "git p4 submit"),
];

/// When git runs the hook `name`.
pub fn describe(name: &str) -> Option<&'static str> {
    KNOWN.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// Marks the gitvis secret-check hook script.
pub const MARKER: &str = "gitvis secret check";

/// Where a hook that was there before the gitvis check is kept.
pub const BACKUP: &str = "pre-commit.before-gitvis";

/// One file in the hooks folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookFile {
    /// The file name (`pre-commit`, `pre-push.sample`).
    pub file: String,
    /// The hook it is (`pre-commit`).
    pub hook: String,
    /// Named `NAME.sample`: git ignores it.
    pub sample: bool,
    /// Git would run it: not a sample, and executable (on Windows: a
    /// `#!` script or an `.exe`).
    pub enabled: bool,
    pub executable: bool,
    pub size: u64,
    pub first_line: String,
    pub known: bool,
    /// The gitvis secret check.
    pub gitvis: bool,
    /// A hook kept by the gitvis check installer.
    pub backup: bool,
    pub path: PathBuf,
}

/// The hooks folder and whether it comes from `core.hooksPath`.
pub fn hooks_dir(dir: &Path, cmds: &mut Vec<String>) -> Result<(PathBuf, Option<String>), String> {
    let printed = git_probe(dir, &["rev-parse", "--git-path", "hooks"], cmds)
        .ok_or_else(|| "Not a git repository.".to_owned())?;
    let custom = git_probe(dir, &["config", "--get", "core.hooksPath"], cmds);
    Ok((resolve_in(dir, &printed), custom))
}

/// Would git run this file? Unix: an executable bit; Windows (Git for
/// Windows' rule): an `.exe`, or a file that starts with `#!`.
pub fn runnable(file: &str, head: &[u8], unix_mode: Option<u32>) -> bool {
    match unix_mode {
        Some(mode) => mode & 0o111 != 0,
        None => file.ends_with(".exe") || head.starts_with(b"#!"),
    }
}

fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// The hook a file name stands for.
pub fn hook_name(file: &str) -> (String, bool) {
    match file.strip_suffix(".sample") {
        Some(h) => (h.to_owned(), true),
        None => (file.to_owned(), false),
    }
}

/// Every file in the hooks folder (sub-folders skipped), hooks first.
pub fn list(hooks: &Path) -> Result<Vec<HookFile>, String> {
    let rd = match std::fs::read_dir(hooks) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Could not read {}: {e}", hooks.display())),
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let head = read_head(&path, 4096);
        let first_line = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(160)
            .collect::<String>();
        let (hook, sample) = hook_name(&file);
        let executable = runnable(&file, &head, mode_of(&meta));
        let text = String::from_utf8_lossy(&head);
        let backup = file == BACKUP;
        out.push(HookFile {
            known: describe(&hook).is_some(),
            gitvis: !sample && text.contains(MARKER),
            enabled: !sample && !backup && executable && describe(&hook).is_some(),
            executable,
            size: meta.len(),
            first_line,
            sample,
            backup,
            hook,
            file,
            path,
        });
    }
    out.sort_by(|a, b| {
        (a.sample, !a.enabled, &a.hook, &a.file).cmp(&(b.sample, !b.enabled, &b.hook, &b.file))
    });
    Ok(out)
}

fn read_head(path: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let got = f.read(&mut buf).unwrap_or(0);
            buf.truncate(got);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Make a file executable (Unix; Windows needs `#!` instead).
pub fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        let mut p = meta.permissions();
        p.set_mode(p.mode() | 0o755);
        std::fs::set_permissions(path, p).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// What enabling or disabling does, as typed.
pub fn rename_command(from: &Path, to: &Path) -> String {
    use crate::devops::workbench::shell::sh_path;
    let mut s = format!("mv {} {}", sh_path(from), sh_path(to));
    if cfg!(unix) {
        s.push_str(&format!(" && chmod +x {}", sh_path(to)));
    }
    s
}

/// `NAME.sample` → `NAME`. Fails when `NAME` exists already.
pub fn enable(hooks: &Path, hook: &str) -> Result<String, String> {
    let from = hooks.join(format!("{hook}.sample"));
    let to = hooks.join(hook);
    if to.exists() {
        return Err(format!("{} exists already.", to.display()));
    }
    std::fs::rename(&from, &to).map_err(|e| format!("Could not rename {}: {e}", from.display()))?;
    make_executable(&to)?;
    Ok(format!("Enabled the {hook} hook"))
}

/// `NAME` → `NAME.sample` (replacing git's example of the same name).
pub fn disable(hooks: &Path, hook: &str) -> Result<String, String> {
    let from = hooks.join(hook);
    let to = hooks.join(format!("{hook}.sample"));
    if to.exists() {
        std::fs::remove_file(&to)
            .map_err(|e| format!("Could not replace {}: {e}", to.display()))?;
    }
    std::fs::rename(&from, &to).map_err(|e| format!("Could not rename {}: {e}", from.display()))?;
    Ok(format!(
        "Disabled the {hook} hook (renamed to {hook}.sample)"
    ))
}

/// Write a hook's content, keeping it executable.
pub fn save(path: &Path, content: &str) -> Result<String, String> {
    let mut text = content.replace("\r\n", "\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text.as_bytes())
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    make_executable(path)?;
    Ok(format!(
        "Saved {} ({})",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        crate::devops::plural_bytes(text.len())
    ))
}

/// A new hook's starting content.
pub fn template(hook: &str) -> String {
    format!(
        "#!/bin/sh\n# {hook}: {}\n# Exit with a non-zero status to stop git.\n\nexit 0\n",
        describe(hook).unwrap_or("a git hook")
    )
}

/// Quote for a double-quoted `sh` word: `\`, `"`, `$` and backquotes are
/// escaped.
pub fn sh_double_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The exe path as the script calls it: forward slashes.
pub fn script_exe_path(exe: &Path) -> String {
    exe.to_string_lossy().replace('\\', "/")
}

/// The pre-commit script that runs the gitvis secret check. A hook that
/// was there before is kept as [`BACKUP`] and runs first.
pub fn secret_hook_script(exe: &Path) -> String {
    let exe = sh_double_quote(&script_exe_path(exe));
    format!(
        "#!/bin/sh\n\
# {MARKER}: installed by gitvis.\n\
# Blocks a commit that stages a high or critical secret (API keys, tokens,\n\
# private keys, passwords). Findings are printed redacted, never in full.\n\
# Skip it once with:  git commit --no-verify\n\
\n\
hook_dir=$(dirname \"$0\")\n\
if [ -x \"$hook_dir/{BACKUP}\" ]; then\n\
\t\"$hook_dir/{BACKUP}\" \"$@\" || exit $?\n\
fi\n\
\n\
gitvis={exe}\n\
if [ ! -f \"$gitvis\" ]; then\n\
\techo \"gitvis secret check: $gitvis is missing, the check was skipped. Reinstall it from gitvis (Tools > Hooks).\" >&2\n\
\texit 0\n\
fi\n\
exec \"$gitvis\" hook pre-commit \"$@\"\n"
    )
}

/// What installing the check will do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    pub hooks: PathBuf,
    pub target: PathBuf,
    /// An existing hook (not gitvis's) that is moved to [`BACKUP`].
    pub backup_from: Option<PathBuf>,
    /// The gitvis check is there already (it is updated).
    pub reinstall: bool,
    pub script: String,
}

impl InstallPlan {
    /// The steps as commands, for the confirmation.
    pub fn commands(&self) -> Vec<String> {
        use crate::devops::workbench::shell::sh_path;
        let mut v = Vec::new();
        if let Some(from) = &self.backup_from {
            v.push(format!(
                "mv {} {}",
                sh_path(from),
                sh_path(&self.hooks.join(BACKUP))
            ));
        }
        v.push(format!("write {} (below)", sh_path(&self.target)));
        if cfg!(unix) {
            v.push(format!("chmod +x {}", sh_path(&self.target)));
        }
        v
    }
}

/// Plan the installation into `hooks` for `exe`.
pub fn plan_install(hooks: &Path, exe: &Path) -> Result<InstallPlan, String> {
    let target = hooks.join("pre-commit");
    let existing = std::fs::read_to_string(&target).ok();
    let reinstall = existing.as_deref().is_some_and(|t| t.contains(MARKER));
    let backup_from = match &existing {
        Some(_) if !reinstall => {
            if hooks.join(BACKUP).exists() {
                return Err(format!(
                    "{} exists already: move it away before installing the gitvis check.",
                    hooks.join(BACKUP).display()
                ));
            }
            Some(target.clone())
        }
        _ => None,
    };
    Ok(InstallPlan {
        hooks: hooks.to_path_buf(),
        target,
        backup_from,
        reinstall,
        script: secret_hook_script(exe),
    })
}

/// Carry out an [`InstallPlan`].
pub fn install(plan: &InstallPlan) -> Result<String, String> {
    std::fs::create_dir_all(&plan.hooks)
        .map_err(|e| format!("Could not create {}: {e}", plan.hooks.display()))?;
    if let Some(from) = &plan.backup_from {
        std::fs::rename(from, plan.hooks.join(BACKUP))
            .map_err(|e| format!("Could not keep the existing hook: {e}"))?;
    }
    std::fs::write(&plan.target, plan.script.as_bytes())
        .map_err(|e| format!("Could not write {}: {e}", plan.target.display()))?;
    make_executable(&plan.target)?;
    Ok(match (&plan.backup_from, plan.reinstall) {
        (_, true) => "Updated the gitvis secret check (pre-commit)".to_owned(),
        (Some(_), _) => format!(
            "Installed the gitvis secret check; the previous pre-commit hook is kept as {BACKUP} and runs first"
        ),
        (None, _) => "Installed the gitvis secret check as the pre-commit hook".to_owned(),
    })
}

/// Remove the gitvis check, putting back a hook it had kept.
pub fn uninstall(hooks: &Path) -> Result<String, String> {
    let target = hooks.join("pre-commit");
    let text = std::fs::read_to_string(&target).unwrap_or_default();
    if !text.contains(MARKER) {
        return Err("The pre-commit hook is not the gitvis secret check.".to_owned());
    }
    std::fs::remove_file(&target)
        .map_err(|e| format!("Could not remove {}: {e}", target.display()))?;
    let backup = hooks.join(BACKUP);
    if backup.exists() {
        std::fs::rename(&backup, &target)
            .map_err(|e| format!("Could not restore {BACKUP}: {e}"))?;
        return Ok(
            "Removed the gitvis secret check and restored the previous pre-commit hook".into(),
        );
    }
    Ok("Removed the gitvis secret check".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!(
            "gitvis-hooks-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn script_quotes_paths_with_spaces() {
        let s = secret_hook_script(Path::new(r"C:\Program Files\gitvis $new\gitvis.exe"));
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains(MARKER));
        assert!(s.contains("gitvis=\"C:/Program Files/gitvis \\$new/gitvis.exe\"\n"));
        assert!(s.contains("exec \"$gitvis\" hook pre-commit \"$@\""));
        assert!(s.contains("git commit --no-verify"));
        assert!(s.contains(BACKUP));
        assert!(!s.contains('\r'));
        assert_eq!(sh_double_quote("a\"b`c\\d"), "\"a\\\"b\\`c\\\\d\"");
    }

    #[test]
    fn runnable_rules() {
        assert!(runnable("pre-commit", b"#!/bin/sh\n", None));
        assert!(!runnable("pre-commit", b"echo hi\n", None));
        assert!(runnable("pre-commit.exe", b"MZ", None));
        assert!(runnable("pre-commit", b"echo", Some(0o755)));
        assert!(!runnable("pre-commit", b"#!/bin/sh", Some(0o644)));
        assert_eq!(hook_name("pre-push.sample"), ("pre-push".into(), true));
        assert!(describe("commit-msg").is_some());
        assert!(describe("not-a-hook").is_none());
    }

    #[test]
    fn listing_enable_disable_and_install() {
        let d = temp("list");
        std::fs::write(d.join("pre-push.sample"), "#!/bin/sh\necho sample\n").unwrap();
        std::fs::write(d.join("commit-msg"), "#!/usr/bin/env bash\nexit 0\n").unwrap();
        std::fs::write(d.join("pre-commit"), "#!/bin/sh\necho mine\n").unwrap();
        std::fs::write(d.join("notes.txt"), "not a hook").unwrap();
        make_executable(&d.join("commit-msg")).unwrap();
        make_executable(&d.join("pre-commit")).unwrap();
        let v = list(&d).unwrap();
        assert_eq!(v.len(), 4);
        let commit_msg = v.iter().find(|h| h.file == "commit-msg").unwrap();
        assert!(commit_msg.enabled);
        assert_eq!(commit_msg.first_line, "#!/usr/bin/env bash");
        let sample = v.iter().find(|h| h.file == "pre-push.sample").unwrap();
        assert!(sample.sample && !sample.enabled && sample.known);
        let notes = v.iter().find(|h| h.file == "notes.txt").unwrap();
        assert!(!notes.known && !notes.enabled);
        // Samples sort last.
        assert!(v.last().unwrap().sample);

        enable(&d, "pre-push").unwrap();
        assert!(d.join("pre-push").exists());
        assert!(!d.join("pre-push.sample").exists());
        disable(&d, "pre-push").unwrap();
        assert!(d.join("pre-push.sample").exists());

        // Install keeps the existing pre-commit and chains it.
        let exe = Path::new("C:/Tools/gitvis.exe");
        let plan = plan_install(&d, exe).unwrap();
        assert!(plan.backup_from.is_some());
        assert!(!plan.reinstall);
        assert!(plan.commands()[0].starts_with("mv "));
        install(&plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join(BACKUP)).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        let listed = list(&d).unwrap();
        assert!(
            listed
                .iter()
                .any(|h| h.file == "pre-commit" && h.gitvis && h.enabled)
        );
        assert!(listed.iter().any(|h| h.backup && !h.enabled));
        // A second install updates in place.
        let again = plan_install(&d, exe).unwrap();
        assert!(again.reinstall && again.backup_from.is_none());
        install(&again).unwrap();
        // Uninstall restores the previous hook.
        uninstall(&d).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("pre-commit")).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        assert!(!d.join(BACKUP).exists());
        assert!(uninstall(&d).is_err());
        save(&d.join("post-commit"), "#!/bin/sh\r\necho done").unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("post-commit")).unwrap(),
            "#!/bin/sh\necho done\n"
        );
        assert!(template("pre-push").starts_with("#!/bin/sh\n# pre-push: git push"));
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            list(&d).unwrap().is_empty(),
            "a missing folder lists nothing"
        );
    }
}
