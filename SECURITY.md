# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub: open the repository's **Security**
tab and choose **Report a vulnerability**. Do not open a public issue for them.

Include what you found, how to reproduce it and the version (Settings → About → Copy details).
Never include real tokens, keys or passwords in a report; a description of where one leaks is
enough. You will get an answer as soon as the report has been looked at, and a fix goes into
the next release with credit to you unless you prefer otherwise.

## Supported versions

Fixes go into the latest release. gitvis is at version 0.x, so please update to the newest
release before reporting.

## How gitvis treats your credentials and data

- **GitHub** access goes through your own `gh auth`; gitvis stores no GitHub token. GitLab and
  Azure DevOps go through `glab` and `az` the same way.
- **API keys** gitvis does keep (the Claude API key, Jira and Linear tokens) live in the
  operating system's keychain, service `gitvis`, never in settings files or logs. They reach
  child processes on standard input only, never on a command line.
- **Secrets never reach the activity log**, toasts or error messages. The exposure scanner shows
  findings redacted and logs only counts, paths and the git commands it ran.
- **Network** calls happen only when you act or open the page that needs them. The optional
  services are GitHub / GitLab / Azure DevOps (through their CLIs), osv.dev (dependency check,
  on request), the Claude API (AI assist, on request, after a confirmation that shows exactly
  what is sent, with secrets redacted and files such as `.env` left out) and GitHub releases
  (update checks, on request or on start if you switch that on).
- **Every change outside your machine** (pushes, GitHub writes, cluster and Terraform commands)
  is shown with its exact commands or request bodies before it runs. **Safe mode** (Settings →
  Safety) refuses all of them; task scripts and commands you type in the terminal are not
  covered.
- **Updates** are verified against the release's SHA-256 checksums before they are installed.
- **Crash reports** stay on your machine (`%LOCALAPPDATA%\gitvis\crashes`), with secrets
  removed, and are only sent if you choose to open an issue with one.
