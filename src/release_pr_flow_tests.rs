//! Regression tests for the release-pull-request (`create_pr`) cycle.
//!
//! Before v0.7.5 a merged release pull request was never tagged: the next run
//! measured the range from the previous tag, still found the fix the merged
//! pull request had already covered, bumped the merged manifest version again,
//! and opened another release pull request (`threatflux-atlassian` merged #86
//! for 0.5.1, and the next run opened #87 for 0.5.2). These tests replay that
//! sequence and assert the cycle converges: the merged version is tagged
//! exactly once, the following run is a no-op, and a dry run of either step
//! writes nothing.

use std::path::Path;

use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{
    mock_build_chain, mock_head_ref, mock_tag_lookup, mock_tags, options, publisher,
    write_fixture_repo,
};
use crate::{ReleaseOptions, ReleaseOutcome, ReleaseReport};

/// The range a run sees right after the 0.2.3 release pull request merged:
/// the fix it shipped, then its squash-merged release commit.
const FIX_THEN_MERGED_RELEASE: &str = r#"{"total_commits":2,"commits":[{"sha":"fixbbbbbbbb","commit":{"message":"fix(testkit): stop log capture racing"},"parents":[{}]},{"sha":"releasecccc","commit":{"message":"chore: release v0.2.3 (#86)"},"parents":[{}]}]}"#;
/// What lands after the merged release is tagged: tooling-only commits.
const CI_ONLY: &str = r#"{"total_commits":1,"commits":[{"sha":"ciddddddddd","commit":{"message":"ci: repin release automation"},"parents":[{}]}]}"#;
const NEXT_FIX: &str = r#"{"total_commits":1,"commits":[{"sha":"fixeeeeeeee","commit":{"message":"fix: repair another thing"},"parents":[{}]}]}"#;
const EMPTY_RANGE: &str = r#"{"total_commits":0,"commits":[]}"#;
const TAGS_V022: &str = r#"[{"name":"v0.2.2","commit":{"sha":"tag022sha"}}]"#;
const TAGS_V023_V022: &str = r#"[{"name":"v0.2.3","commit":{"sha":"basecommitsha"}},{"name":"v0.2.2","commit":{"sha":"tag022sha"}}]"#;
const NOT_FOUND: &str = r#"{"message":"Not Found"}"#;

/// Read-side mocks: the head ref (read `head_hits` times), the tag listing,
/// and the range from `base_tag` to the head.
fn mock_range(
    server: &mut ServerGuard,
    head_hits: usize,
    tags: &str,
    base_tag: &str,
    range: &str,
) -> Vec<Mock> {
    vec![
        mock_head_ref(server, "basecommitsha", head_hits),
        mock_tags(server, tags),
        server
            .mock(
                "GET",
                format!("/repos/acme/demo/compare/{base_tag}...basecommitsha?per_page=100&page=1")
                    .as_str(),
            )
            .with_status(200)
            .with_body(range)
            .create(),
    ]
}

/// Main holds the merged 0.2.3 bump, only v0.2.2 is tagged, and v0.2.3 does
/// not exist yet.
fn mock_merged_release_state(server: &mut ServerGuard, head_hits: usize) -> Vec<Mock> {
    let mut mocks = mock_range(server, head_hits, TAGS_V022, "v0.2.2", FIX_THEN_MERGED_RELEASE);
    mocks.push(mock_tag_lookup(server, "v0.2.3", 404, NOT_FOUND));
    mocks
}

/// Mocks each `(method, path)` endpoint expecting zero hits.
fn mock_never(server: &mut ServerGuard, endpoints: &[(&str, &str)]) -> Vec<Mock> {
    endpoints.iter().map(|(method, path)| server.mock(method, *path).expect(0).create()).collect()
}

/// Every endpoint that would write to the repository, each expected zero
/// times.
fn mock_no_writes(server: &mut ServerGuard) -> Vec<Mock> {
    mock_never(
        server,
        &[
            ("POST", "/repos/acme/demo/git/blobs"),
            ("POST", "/repos/acme/demo/git/trees"),
            ("POST", "/repos/acme/demo/git/commits"),
            ("POST", "/repos/acme/demo/git/tags"),
            ("POST", "/repos/acme/demo/git/refs"),
            ("PATCH", "/repos/acme/demo/git/refs/heads/main"),
            ("PATCH", "/repos/acme/demo/git/refs/heads/automation/release"),
            ("POST", "/repos/acme/demo/releases"),
            ("POST", "/repos/acme/demo/pulls"),
        ],
    )
}

/// Tagging the merged release needs no rewrite, so it creates no commit and
/// writes neither branch nor pull request.
fn mock_no_commit_or_branch_writes(server: &mut ServerGuard) -> Vec<Mock> {
    mock_never(
        server,
        &[
            ("POST", "/repos/acme/demo/git/blobs"),
            ("POST", "/repos/acme/demo/git/commits"),
            ("PATCH", "/repos/acme/demo/git/refs/heads/main"),
            ("PATCH", "/repos/acme/demo/git/refs/heads/automation/release"),
            ("POST", "/repos/acme/demo/pulls"),
        ],
    )
}

/// Release-pull-request reads a run that only tags, or only analyzes, must
/// never reach: it has no release branch to refresh and no pull request to
/// find.
fn mock_no_pull_request_lookups(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server.mock("GET", "/repos/acme/demo/git/ref/heads/automation/release").expect(0).create(),
        server.mock("GET", Matcher::Regex(r"^/repos/acme/demo/pulls".into())).expect(0).create(),
    ]
}

/// A write expected exactly once, with a body matching every pattern.
fn mock_once(
    server: &mut ServerGuard,
    method: &str,
    path: &str,
    patterns: &[&str],
    response: &str,
) -> Mock {
    server
        .mock(method, path)
        .match_body(Matcher::AllOf(
            patterns.iter().map(|pattern| Matcher::Regex((*pattern).to_owned())).collect(),
        ))
        .expect(1)
        .with_status(201)
        .with_body(response)
        .create()
}

/// The annotated v0.2.3 tag on the merged head, its ref, and its Release.
fn mock_tag_and_release_v023(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        mock_once(
            server,
            "POST",
            "/repos/acme/demo/git/tags",
            &[r#""tag":"v0\.2\.3""#, r#""object":"basecommitsha""#],
            r#"{"sha":"tagobjectsha"}"#,
        ),
        mock_once(
            server,
            "POST",
            "/repos/acme/demo/git/refs",
            &[r#""ref":"refs/tags/v0\.2\.3""#, r#""sha":"tagobjectsha""#],
            r#"{"ref":"refs/tags/v0.2.3"}"#,
        ),
        mock_once(
            server,
            "POST",
            "/repos/acme/demo/releases",
            &[r#""tag_name":"v0\.2\.3""#, r#""target_commitish":"basecommitsha""#, "### Bug Fixes"],
            r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#,
        ),
    ]
}

/// A missing release branch, its creation at the bump commit, and a new
/// pull request titled for v0.2.4.
fn mock_open_release_pull_request_v024(server: &mut ServerGuard) -> Vec<Mock> {
    let find_pull_request =
        "/repos/acme/demo/pulls?state=open&head=acme%3Aautomation%2Frelease&base=main&per_page=100";
    vec![
        server
            .mock("GET", "/repos/acme/demo/git/ref/heads/automation/release")
            .with_status(404)
            .with_body(NOT_FOUND)
            .create(),
        mock_once(
            server,
            "POST",
            "/repos/acme/demo/git/refs",
            &[r#""ref":"refs/heads/automation/release".*"sha":"newcommitsha""#],
            r#"{"ref":"refs/heads/automation/release"}"#,
        ),
        server.mock("GET", find_pull_request).with_status(200).with_body("[]").create(),
        mock_once(
            server,
            "POST",
            "/repos/acme/demo/pulls",
            &[r#""title":"chore\(release\): v0\.2\.4""#],
            r#"{"number":88,"html_url":"https://github.com/acme/demo/pull/88"}"#,
        ),
    ]
}

/// Asserts every mock's hit expectation; mockito does not assert on drop.
fn assert_all(mocks: &[Mock]) {
    for mock in mocks {
        mock.assert();
    }
}

/// Release options for the release-pull-request mode, optionally dry.
fn create_pr_options(repo_root: &Path, dry_run: bool) -> ReleaseOptions {
    let mut release_options = options(repo_root);
    release_options.create_pr = true;
    release_options.dry_run = dry_run;
    release_options
}

/// Runs one release-pull-request mode release against the mock server.
fn run(server: &ServerGuard, repo_root: &Path, dry_run: bool) -> ReleaseReport {
    publisher(server).release(&create_pr_options(repo_root, dry_run)).expect("release report")
}

/// Run 1 of the cycle: the merged 0.2.3 is tagged on the existing head.
fn tag_the_merged_release(repo_root: &Path) {
    let mut server = Server::new();
    let _state = mock_merged_release_state(&mut server, 2);
    let mut forbidden = mock_no_pull_request_lookups(&mut server);
    forbidden.extend(mock_no_commit_or_branch_writes(&mut server));
    let publish = mock_tag_and_release_v023(&mut server);

    let report = run(&server, repo_root, false);

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.current_version, "0.2.3");
    assert_eq!(report.next_version.as_deref(), Some("0.2.3"), "must not bump past 0.2.3");
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(report.commit_sha.as_deref(), Some("basecommitsha"));
    assert_eq!(report.pull_request_number, None);
    assert_eq!(report.release_branch, None);
    assert_all(&forbidden);
    assert_all(&publish);
}

/// Run 2 of the cycle: v0.2.3 exists and only tooling commits followed it.
fn run_after_the_tag(repo_root: &Path) {
    let mut server = Server::new();
    let _range = mock_range(&mut server, 1, TAGS_V023_V022, "v0.2.3", CI_ONLY);
    let mut forbidden = mock_no_writes(&mut server);
    forbidden.extend(mock_no_pull_request_lookups(&mut server));

    let report = run(&server, repo_root, false);

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.next_version, None);
    assert_eq!(report.tag, None);
    assert_eq!(report.pull_request_number, None);
    assert_eq!(report.commits_analyzed, 1);
    assert_all(&forbidden);
}

/// The full cycle: tag the merged release once, then do nothing.
#[test]
fn merged_release_pull_request_is_tagged_once_and_the_next_run_is_a_no_op() {
    let temp_dir = write_fixture_repo();
    tag_the_merged_release(temp_dir.path());
    run_after_the_tag(temp_dir.path());
}

/// A second run for the same head (the CI and Security completions each start
/// one) finds the tag the first run created via the latest-tag listing and
/// treats the manifest as released.
#[test]
fn rerun_after_the_merged_release_is_tagged_skips_instead_of_retagging() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _range = mock_range(&mut server, 1, TAGS_V023_V022, "v0.2.3", EMPTY_RANGE);
    let no_writes = mock_no_writes(&mut server);

    let report = run(&server, temp_dir.path(), false);

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.commits_analyzed, 0);
    assert_all(&no_writes);
}

/// The tag listing has not caught up with a v0.2.3 made by hand (or by a
/// concurrent run) moments earlier, so the manifest still looks unreleased.
/// The direct tag lookup must stop the run: no retag, and no pull request for
/// 0.2.4.
#[test]
fn tagging_skips_when_the_merged_version_was_tagged_by_hand() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _range = mock_range(&mut server, 1, TAGS_V022, "v0.2.2", FIX_THEN_MERGED_RELEASE);
    let _tag_exists = mock_tag_lookup(
        &mut server,
        "v0.2.3",
        200,
        r#"{"ref":"refs/tags/v0.2.3","object":{"sha":"handmadesha"}}"#,
    );
    let no_writes = mock_no_writes(&mut server);

    let report = run(&server, temp_dir.path(), false);

    assert_eq!(report.outcome, ReleaseOutcome::SkippedTagExists);
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(report.pull_request_number, None);
    assert_all(&no_writes);
}

/// Once the merged release is tagged, the next fix starts a fresh cycle with a
/// release pull request for 0.2.4 and no tag.
#[test]
fn a_fix_after_the_tagged_release_opens_the_next_release_pull_request() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _range = mock_range(&mut server, 2, TAGS_V023_V022, "v0.2.3", NEXT_FIX);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.4", 404, NOT_FOUND);
    let _build = mock_build_chain(&mut server);
    let pull_request = mock_open_release_pull_request_v024(&mut server);
    let no_tagging = mock_never(
        &mut server,
        &[
            ("POST", "/repos/acme/demo/git/tags"),
            ("POST", "/repos/acme/demo/releases"),
            ("PATCH", "/repos/acme/demo/git/refs/heads/main"),
        ],
    );

    let report = run(&server, temp_dir.path(), false);

    assert_eq!(report.outcome, ReleaseOutcome::PullRequestCreated);
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    assert_eq!(report.pull_request_number, Some(88));
    assert_eq!(report.release_url, None);
    assert_all(&pull_request);
    assert_all(&no_tagging);
}

/// A dry run of run 1 reports v0.2.3 without tagging or touching a branch.
#[test]
fn dry_run_of_the_merged_release_reports_the_tag_without_writing() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _state = mock_merged_release_state(&mut server, 1);
    let mut forbidden = mock_no_writes(&mut server);
    forbidden.extend(mock_no_pull_request_lookups(&mut server));

    let report = run(&server, temp_dir.path(), true);

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.next_version.as_deref(), Some("0.2.3"));
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(report.commit_sha, None);
    assert_eq!(report.files_updated, [] as [std::path::PathBuf; 0]);
    assert!(report.notes.as_deref().is_some_and(|notes| notes.contains("### Bug Fixes")));
    assert_all(&forbidden);
}

/// A dry run of the pull-request step reports v0.2.4 and the files it would
/// rewrite, without refreshing the release branch or opening a pull request.
#[test]
fn dry_run_of_a_release_pull_request_reports_the_bump_without_writing() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _range = mock_range(&mut server, 1, TAGS_V023_V022, "v0.2.3", NEXT_FIX);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.4", 404, NOT_FOUND);
    let mut forbidden = mock_no_writes(&mut server);
    forbidden.extend(mock_no_pull_request_lookups(&mut server));

    let report = run(&server, temp_dir.path(), true);

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    assert_eq!(report.tag.as_deref(), Some("v0.2.4"));
    assert_eq!(report.pull_request_number, None);
    assert_eq!(report.release_branch, None);
    assert_eq!(report.files_updated.len(), 1);
    assert_all(&forbidden);
}
