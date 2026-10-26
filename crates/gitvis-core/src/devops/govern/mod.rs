//! Security and administration on GitHub: acting on Dependabot and
//! code-scanning alerts (dismiss with a reason, reopen), SBOM (local
//! CycloneDX / SPDX from the lockfiles, GitHub's dependency-graph SBOM),
//! container images (Dockerfile lint, `trivy` / `grype` / `docker scout`
//! scans), branch protection (read, edit, remove; rulesets read-only),
//! CODEOWNERS (parse, validate, match a path, GitHub's error list),
//! environments (create, edit reviewers / wait timer / branch policy,
//! delete), Actions secrets (names only; set through `gh secret set` with
//! the value on standard input; delete) and variables (create, edit,
//! delete), at repository and environment level.
