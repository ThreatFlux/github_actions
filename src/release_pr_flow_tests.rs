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

use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{
    mock_build_chain, mock_head_ref, mock_tag_lookup, mock_tags, options, publisher,
    write_fixture_repo,
};
use crate::ReleaseOutcome;

/// The range a run sees right after the 0.2.3 release pull request merged:
/// the fix it shipped, then its squash-merged release commit.
const FIX_THEN_MERGED_RELEASE: &str = r#"{"total_commits":2,"commits":[{"sha":"fixbbbbbbbb","commit":{"message":"fix(testkit): stop log capture racing"},"parents":[{}]},{"sha":"releasecccc","commit":{"message":"chore: release v0.2.3 (#86)"},"parents":[{}]}]}"#;
/// What lands after the merged release is tagged: tooling-only commits.
const CI_ONLY: &str = r#"{"total_commits":1,"commits":[{"sha":"ciddddddddd","commit":{"message":"ci: repin release automation"},"parents":[{}]}]}"#;
const NEXT_FIX: &str = r#"{"total_commits":1,"commits":[{"sha":"fixeeeeeeee","commit":{"message":"fix: repair another thing"},"parents":[{}]}]}"#;
const TAGS_V022: &str = r#"[{"name":"v0.2.2","commit":{"sha":"tag022sha"}}]"#;
const TAGS_V023_V022: &str = r#"[{"name":"v0.2.3","commit":{"sha":"basecommitsha"}},{"name":"v0.2.2","commit":{"sha":"tag022sha"}}]"#;

fn mock_compare_from(server: &mut ServerGuard, base_tag: &str, body: &str) -> Mock {
    server
        .mock(
            "GET",
            format!("/repos/acme/demo/compare/{base_tag}...basecommitsha?per_page=100&page=1")
                .as_str(),
        )
        .with_status(200)
        .with_body(body)
        .create()
}

/// Every endpoint that would write to the repository. Each is expected zero
/// times; callers assert them after the run.
fn mock_no_writes(server: &mut ServerGuard) -> Vec<Mock> {
    [
        ("POST", "/repos/acme/demo/git/blobs"),
        ("POST", "/repos/acme/demo/git/trees"),
        ("POST", "/repos/acme/demo/git/commits"),
        ("POST", "/repos/acme/demo/git/tags"),
        ("POST", "/repos/acme/demo/git/refs"),
        ("PATCH", "/repos/acme/demo/git/refs/heads/main"),
        ("PATCH", "/repos/acme/demo/git/refs/heads/automation/release"),
        ("POST", "/repos/acme/demo/releases"),
        ("POST", "/repos/acme/demo/pulls"),
    ]
    .into_iter()
    .map(|(method, path)| server.mock(method, path).expect(0).create())
    .collect()
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

fn assert_all(mocks: &[Mock]) {
    for mock in mocks {
        mock.assert();
    }
}

fn create_pr_options(repo_root: &std::path::Path) -> crate::ReleaseOptions {
    let mut release_options = options(repo_root);
    release_options.create_pr = true;
    release_options
}

#[test]
fn merged_release_pull_request_is_tagged_once_and_the_next_run_is_a_no_op() {
    let temp_dir = write_fixture_repo();

    // Run 1: main holds the merged 0.2.3 bump; only v0.2.2 is tagged.
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 2);
    let _tags = mock_tags(&mut server, TAGS_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.2", FIX_THEN_MERGED_RELEASE);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.3", 404, r#"{"message":"Not Found"}"#);
    let no_pull_request = mock_no_pull_request_lookups(&mut server);
    // Nothing to rewrite: the tag lands on the merged head, with no new commit
    // and no write to the release branch or the base branch.
    let no_commit_or_branch_writes = [
        server.mock("POST", "/repos/acme/demo/git/blobs").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/git/commits").expect(0).create(),
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create(),
        server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/automation/release")
            .expect(0)
            .create(),
        server.mock("POST", "/repos/acme/demo/pulls").expect(0).create(),
    ];
    let tag_object = server
        .mock("POST", "/repos/acme/demo/git/tags")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""tag":"v0\.2\.3""#.into()),
            Matcher::Regex(r#""object":"basecommitsha""#.into()),
        ]))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create();
    let tag_ref = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""ref":"refs/tags/v0\.2\.3""#.into()),
            Matcher::Regex(r#""sha":"tagobjectsha""#.into()),
        ]))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.2.3"}"#)
        .create();
    let release = server
        .mock("POST", "/repos/acme/demo/releases")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""tag_name":"v0\.2\.3""#.into()),
            Matcher::Regex(r#""target_commitish":"basecommitsha""#.into()),
            Matcher::Regex("### Bug Fixes".into()),
        ]))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#)
        .create();

    let first = publisher(&server)
        .release(&create_pr_options(temp_dir.path()))
        .expect("first release report");

    assert_eq!(first.outcome, ReleaseOutcome::Released);
    assert_eq!(first.current_version, "0.2.3");
    assert_eq!(first.next_version.as_deref(), Some("0.2.3"), "must not bump past 0.2.3");
    assert_eq!(first.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(first.commit_sha.as_deref(), Some("basecommitsha"));
    assert_eq!(first.pull_request_number, None);
    assert_eq!(first.release_branch, None);
    assert_all(&no_pull_request);
    assert_all(&no_commit_or_branch_writes);
    tag_object.assert();
    tag_ref.assert();
    release.assert();

    // Run 2: v0.2.3 now exists and only tooling commits followed it.
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.3", CI_ONLY);
    let no_writes = mock_no_writes(&mut server);
    let no_pull_request = mock_no_pull_request_lookups(&mut server);

    let second = publisher(&server)
        .release(&create_pr_options(temp_dir.path()))
        .expect("second release report");

    assert_eq!(second.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(second.next_version, None);
    assert_eq!(second.tag, None);
    assert_eq!(second.pull_request_number, None);
    assert_eq!(second.commits_analyzed, 1);
    assert_all(&no_writes);
    assert_all(&no_pull_request);
}

#[test]
fn rerun_after_the_merged_release_is_tagged_skips_instead_of_retagging() {
    // A second run for the same head (the CI and Security completions each
    // start one) finds the tag the first run created via the latest-tag
    // listing and treats the manifest as released.
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.3", r#"{"total_commits":0,"commits":[]}"#);
    let no_writes = mock_no_writes(&mut server);

    let report =
        publisher(&server).release(&create_pr_options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.commits_analyzed, 0);
    assert_all(&no_writes);
}

#[test]
fn tagging_skips_when_the_merged_version_was_tagged_by_hand() {
    // The tag listing has not caught up with a v0.2.3 made by hand (or by a
    // concurrent run) moments earlier, so the manifest still looks unreleased.
    // The direct tag lookup must stop the run: no retag, and no pull request
    // for 0.2.4.
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.2", FIX_THEN_MERGED_RELEASE);
    let _tag_exists = mock_tag_lookup(
        &mut server,
        "v0.2.3",
        200,
        r#"{"ref":"refs/tags/v0.2.3","object":{"sha":"handmadesha"}}"#,
    );
    let no_writes = mock_no_writes(&mut server);

    let report =
        publisher(&server).release(&create_pr_options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedTagExists);
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(report.pull_request_number, None);
    assert_all(&no_writes);
}

#[test]
fn a_fix_after_the_tagged_release_opens_the_next_release_pull_request() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 2);
    let _tags = mock_tags(&mut server, TAGS_V023_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.3", NEXT_FIX);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.4", 404, r#"{"message":"Not Found"}"#);
    let _build = mock_build_chain(&mut server);
    let _branch_missing = server
        .mock("GET", "/repos/acme/demo/git/ref/heads/automation/release")
        .with_status(404)
        .with_body(r#"{"message":"Not Found"}"#)
        .create();
    let create_branch = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .match_body(Matcher::Regex(
            r#""ref":"refs/heads/automation/release".*"sha":"newcommitsha""#.into(),
        ))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"ref":"refs/heads/automation/release"}"#)
        .create();
    let _find_pr = server
        .mock(
            "GET",
            "/repos/acme/demo/pulls?state=open&head=acme%3Aautomation%2Frelease&base=main&per_page=100",
        )
        .with_status(200)
        .with_body("[]")
        .create();
    let create_pr = server
        .mock("POST", "/repos/acme/demo/pulls")
        .match_body(Matcher::Regex(r#""title":"chore\(release\): v0\.2\.4""#.into()))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"number":88,"html_url":"https://github.com/acme/demo/pull/88"}"#)
        .create();
    let no_tagging = [
        server.mock("POST", "/repos/acme/demo/git/tags").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/releases").expect(0).create(),
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create(),
    ];

    let report =
        publisher(&server).release(&create_pr_options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::PullRequestCreated);
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    assert_eq!(report.pull_request_number, Some(88));
    assert_eq!(report.release_url, None);
    create_branch.assert();
    create_pr.assert();
    assert_all(&no_tagging);
}

#[test]
fn dry_run_of_the_merged_release_reports_the_tag_without_writing() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.2", FIX_THEN_MERGED_RELEASE);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.3", 404, r#"{"message":"Not Found"}"#);
    let no_writes = mock_no_writes(&mut server);
    let no_pull_request = mock_no_pull_request_lookups(&mut server);

    let mut release_options = create_pr_options(temp_dir.path());
    release_options.dry_run = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.next_version.as_deref(), Some("0.2.3"));
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    assert_eq!(report.commit_sha, None);
    assert_eq!(report.files_updated, [] as [std::path::PathBuf; 0]);
    assert!(report.notes.as_deref().is_some_and(|notes| notes.contains("### Bug Fixes")));
    assert_all(&no_writes);
    assert_all(&no_pull_request);
}

#[test]
fn dry_run_of_a_release_pull_request_reports_the_bump_without_writing() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023_V022);
    let _compare = mock_compare_from(&mut server, "v0.2.3", NEXT_FIX);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.4", 404, r#"{"message":"Not Found"}"#);
    let no_writes = mock_no_writes(&mut server);
    let no_pull_request = mock_no_pull_request_lookups(&mut server);

    let mut release_options = create_pr_options(temp_dir.path());
    release_options.dry_run = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    assert_eq!(report.tag.as_deref(), Some("v0.2.4"));
    assert_eq!(report.pull_request_number, None);
    assert_eq!(report.release_branch, None);
    assert_eq!(report.files_updated.len(), 1);
    assert_all(&no_writes);
    assert_all(&no_pull_request);
}
