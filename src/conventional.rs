//! Conventional-commit classification and semantic-version bump computation.
//!
//! Subjects are matched against the Conventional Commits grammar
//! (`type(scope)!: description`) on the first message line only. Breaking
//! changes are detected from a `!` marker in the subject or a
//! `BREAKING CHANGE` / `BREAKING-CHANGE` footer at the start of a body line.

use std::sync::LazyLock;

use regex::Regex;
use semver::Version;

use crate::github::CommitInfo;

static SUBJECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<type>[A-Za-z][A-Za-z0-9-]*)(?:\((?P<scope>[^)]+)\))?(?P<bang>!)?:\s?(?P<description>.+)$",
    )
    .expect("subject regex is valid")
});

static BREAKING_BODY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^BREAKING[- ]CHANGE\b").expect("breaking regex is valid"));

/// Strength of a semantic-version bump, ordered weakest to strongest.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum BumpLevel {
    Patch,
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CommitKind {
    Breaking,
    Feature,
    Fix,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConventionalCommit {
    pub sha: String,
    pub kind: CommitKind,
    pub subject: String,
}

/// Classify commits by their conventional-commit subject, skipping merge
/// commits entirely.
pub fn classify_commits(commits: &[CommitInfo]) -> Vec<ConventionalCommit> {
    commits
        .iter()
        .filter(|commit| !commit.is_merge)
        .map(|commit| {
            let subject = commit.message.lines().next().unwrap_or_default().trim().to_owned();
            ConventionalCommit {
                sha: commit.sha.clone(),
                kind: classify_message(&subject, &commit.message),
                subject,
            }
        })
        .collect()
}

fn classify_message(subject: &str, message: &str) -> CommitKind {
    let captures = SUBJECT_RE.captures(subject);
    let breaking = captures.as_ref().is_some_and(|captures| captures.name("bang").is_some())
        || BREAKING_BODY_RE.is_match(message);
    if breaking {
        return CommitKind::Breaking;
    }

    match captures.as_ref().map(|captures| &captures["type"]) {
        Some("feat") => CommitKind::Feature,
        Some("fix") => CommitKind::Fix,
        _ => CommitKind::Other,
    }
}

/// Determine the strongest bump the commits require, or `None` when no commit
/// warrants a release.
pub fn required_bump(commits: &[ConventionalCommit]) -> Option<BumpLevel> {
    commits
        .iter()
        .filter_map(|commit| match commit.kind {
            CommitKind::Breaking => Some(BumpLevel::Major),
            CommitKind::Feature => Some(BumpLevel::Minor),
            CommitKind::Fix => Some(BumpLevel::Patch),
            CommitKind::Other => None,
        })
        .max()
}

/// Apply `level` to `current`, clearing any pre-release or build metadata.
#[must_use]
pub fn bump_version(current: &Version, level: BumpLevel) -> Version {
    let mut next = match level {
        BumpLevel::Major => Version::new(current.major + 1, 0, 0),
        BumpLevel::Minor => Version::new(current.major, current.minor + 1, 0),
        BumpLevel::Patch => Version::new(current.major, current.minor, current.patch + 1),
    };
    next.pre = semver::Prerelease::EMPTY;
    next.build = semver::BuildMetadata::EMPTY;
    next
}

/// True when `subject` follows the Conventional Commits grammar
/// (`type(scope)!: description`), whatever its type.
#[must_use]
pub fn is_conventional_subject(subject: &str) -> bool {
    SUBJECT_RE.is_match(subject)
}

/// Render markdown release notes grouped by commit kind, listing breaking
/// changes first and omitting empty sections.
///
/// A release with no breaking change, feature, or fix (a forced or manual
/// bump over `ci:`, `build:`, `chore:`, `docs:` and similar commits) would
/// otherwise get an empty body, so its other conventional commits are listed
/// under "Maintenance" instead. Releases that do have features or fixes keep
/// listing only those, and subjects that are not conventional commits are
/// never listed.
#[must_use]
pub fn release_notes(tag: &str, commits: &[ConventionalCommit], truncated: bool) -> String {
    let mut notes = format!("## Release {tag}\n");
    let sections = [
        (CommitKind::Breaking, "Breaking Changes"),
        (CommitKind::Feature, "Features"),
        (CommitKind::Fix, "Bug Fixes"),
    ];

    for (kind, title) in sections {
        let section: Vec<&ConventionalCommit> =
            commits.iter().filter(|commit| commit.kind == kind).collect();
        push_section(&mut notes, title, &section);
    }

    if commits.iter().all(|commit| commit.kind == CommitKind::Other) {
        let maintenance: Vec<&ConventionalCommit> =
            commits.iter().filter(|commit| is_conventional_subject(&commit.subject)).collect();
        push_section(&mut notes, "Maintenance", &maintenance);
    }

    if truncated {
        notes.push_str("\n_Note: the commit list was truncated; some changes may be missing._\n");
    }

    notes
}

/// Append a `### {title}` section listing `commits` with short SHAs; nothing
/// when `commits` is empty.
fn push_section(notes: &mut String, title: &str, commits: &[&ConventionalCommit]) {
    use std::fmt::Write as _;

    if commits.is_empty() {
        return;
    }
    notes.push('\n');
    writeln!(notes, "### {title}").expect("writing to a String cannot fail");
    for commit in commits {
        let short_sha = commit.sha.get(..7).unwrap_or(&commit.sha);
        writeln!(notes, "- {} ({short_sha})", commit.subject)
            .expect("writing to a String cannot fail");
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{
        BumpLevel, CommitKind, bump_version, classify_commits, is_conventional_subject,
        release_notes, required_bump,
    };
    use crate::github::CommitInfo;

    fn commit(message: &str) -> CommitInfo {
        CommitInfo {
            sha: String::from("0123456789abcdef"),
            message: message.to_owned(),
            is_merge: false,
        }
    }

    #[test]
    fn classify_commits_maps_conventional_types() {
        let commits = [
            commit("feat: add release command"),
            commit("fix(parser): handle empty scope"),
            commit("docs: update readme"),
            commit("chore: bump dependencies"),
        ];

        let classified = classify_commits(&commits);

        assert_eq!(classified[0].kind, CommitKind::Feature);
        assert_eq!(classified[1].kind, CommitKind::Fix);
        assert_eq!(classified[2].kind, CommitKind::Other);
        assert_eq!(classified[3].kind, CommitKind::Other);
    }

    #[test]
    fn classify_commits_does_not_treat_feature_prefix_as_feat() {
        let classified = classify_commits(&[commit("feature: not conventional feat")]);

        assert_eq!(classified[0].kind, CommitKind::Other);
    }

    #[test]
    fn classify_commits_detects_breaking_bang_marker() {
        let classified = classify_commits(&[
            commit("feat!: drop legacy flags"),
            commit("refactor(core)!: rework internals"),
        ]);

        assert_eq!(classified[0].kind, CommitKind::Breaking);
        assert_eq!(classified[1].kind, CommitKind::Breaking);
    }

    #[test]
    fn classify_commits_detects_breaking_change_footer() {
        let classified = classify_commits(&[
            commit("fix: adjust defaults\n\nBREAKING CHANGE: defaults changed"),
            commit("chore: cleanup\n\nBREAKING-CHANGE: removed helper"),
        ]);

        assert_eq!(classified[0].kind, CommitKind::Breaking);
        assert_eq!(classified[1].kind, CommitKind::Breaking);
    }

    #[test]
    fn classify_commits_ignores_mid_line_breaking_mentions() {
        let classified =
            classify_commits(&[commit("docs: describe the BREAKING CHANGE process in text")]);

        assert_eq!(classified[0].kind, CommitKind::Other);
    }

    #[test]
    fn classify_commits_skips_merge_commits() {
        let merge = CommitInfo {
            sha: String::from("mergesha"),
            message: String::from("Merge pull request #1 from acme/feat"),
            is_merge: true,
        };

        let classified = classify_commits(&[merge, commit("fix: real change")]);

        assert_eq!(classified.len(), 1);
        assert_eq!(classified[0].kind, CommitKind::Fix);
    }

    #[test]
    fn classify_commits_uses_first_line_as_subject() {
        let classified = classify_commits(&[commit("feat: multi line\n\nbody detail")]);

        assert_eq!(classified[0].subject, "feat: multi line");
    }

    #[test]
    fn required_bump_prefers_the_strongest_level() {
        let commits = classify_commits(&[
            commit("fix: patch level"),
            commit("feat: minor level"),
            commit("feat!: major level"),
        ]);

        assert_eq!(required_bump(&commits), Some(BumpLevel::Major));
    }

    #[test]
    fn required_bump_returns_none_for_chore_only_history() {
        let commits = classify_commits(&[commit("chore: tidy"), commit("docs: notes")]);

        assert_eq!(required_bump(&commits), None);
    }

    #[test]
    fn bump_version_increments_and_resets_components() {
        let current = Version::parse("1.2.3").expect("version");

        assert_eq!(
            bump_version(&current, BumpLevel::Major),
            Version::parse("2.0.0").expect("version")
        );
        assert_eq!(
            bump_version(&current, BumpLevel::Minor),
            Version::parse("1.3.0").expect("version")
        );
        assert_eq!(
            bump_version(&current, BumpLevel::Patch),
            Version::parse("1.2.4").expect("version")
        );
    }

    #[test]
    fn bump_version_clears_prerelease_metadata() {
        let current = Version::parse("1.2.3-rc.1+build.5").expect("version");

        assert_eq!(
            bump_version(&current, BumpLevel::Patch),
            Version::parse("1.2.4").expect("version")
        );
    }

    #[test]
    fn release_notes_group_sections_and_short_shas() {
        let commits = classify_commits(&[
            commit("feat!: drop old flags"),
            commit("feat: add release command"),
            commit("fix: handle empty tags"),
            commit("chore: noise"),
        ]);

        let notes = release_notes("v1.0.0", &commits, false);

        assert_eq!(
            notes,
            "## Release v1.0.0\n\n### Breaking Changes\n- feat!: drop old flags (0123456)\n\n### Features\n- feat: add release command (0123456)\n\n### Bug Fixes\n- fix: handle empty tags (0123456)\n"
        );
    }

    #[test]
    fn release_notes_flag_truncated_commit_ranges() {
        let notes = release_notes("v1.0.0", &[], true);

        assert!(notes.contains("truncated"), "{notes}");
    }

    #[test]
    fn release_notes_list_maintenance_commits_when_nothing_else_qualifies() {
        let commits = classify_commits(&[
            commit("ci(automation): audit the App weekly"),
            commit("build(docker): move to trixie"),
            commit("chore(deps): bump serde"),
            commit("docs: explain trusted publishing"),
            commit("Update README.md"),
        ]);

        let notes = release_notes("v0.7.8", &commits, false);

        assert_eq!(
            notes,
            "## Release v0.7.8\n\n### Maintenance\n- ci(automation): audit the App weekly (0123456)\n- build(docker): move to trixie (0123456)\n- chore(deps): bump serde (0123456)\n- docs: explain trusted publishing (0123456)\n"
        );
    }

    #[test]
    fn release_notes_leave_maintenance_out_beside_features_or_fixes() {
        for headline in ["fix: repair thing", "feat: add thing", "refactor!: drop thing"] {
            let commits = classify_commits(&[commit(headline), commit("ci: tidy workflows")]);

            let notes = release_notes("v1.0.0", &commits, false);

            assert!(!notes.contains("Maintenance"), "{notes}");
            assert!(!notes.contains("ci: tidy workflows"), "{notes}");
        }
    }

    #[test]
    fn release_notes_without_conventional_commits_stay_bare() {
        let commits = classify_commits(&[commit("Update README.md"), commit("wip")]);

        assert_eq!(release_notes("v1.0.1", &commits, false), "## Release v1.0.1\n");
        assert_eq!(
            release_notes("v1.0.1", &commits, true),
            "## Release v1.0.1\n\n_Note: the commit list was truncated; some changes may be missing._\n"
        );
    }

    #[test]
    fn conventional_subjects_are_recognized_by_grammar_not_type() {
        assert!(is_conventional_subject("chore(release): v1"));
        assert!(is_conventional_subject("perf!: faster"));
        assert!(!is_conventional_subject("Merge branch main"));
        assert!(!is_conventional_subject("(ci): missing type"));
    }
}
