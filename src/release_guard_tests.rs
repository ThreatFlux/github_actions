//! The manifest-behind-tag guard, and the action each release path records.
//!
//! `threatflux-unifi-sdk` carried a `v0.7.5` tag while its manifest said
//! 0.5.4. The commit range is measured from the highest tag but the bump is
//! applied to the manifest, so every run there would have proposed 0.5.5 or
//! 0.6.0: versions below one already released, re-proposed on every later run
//! because the range never moves past `v0.7.5`. The guard refuses that state in
//! every mode instead of releasing from it.

use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{
    CHORE_ONLY, FEAT_AND_FIX, mock_analysis, mock_head_ref, mock_no_mutations, mock_tag_lookup,
    mock_tags, options, publisher, write_fixture_repo,
};
use crate::{BumpLevel, ReleaseAction, ReleaseOptions, ReleaseOutcome, ReleasePhase};

/// The fixture manifest is 0.2.3; the highest tag is v0.7.5.
const TAGS_V075_V023: &str = r#"[{"name":"v0.2.3","commit":{"sha":"tag023sha"}},{"name":"v0.7.5","commit":{"sha":"tag075sha"}}]"#;

/// Mocks the compare range from `base_tag` to the fixture head.
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

/// Analysis mocks for a repository whose highest tag is ahead of the manifest.
fn mock_behind_analysis(server: &mut ServerGuard, compare_body: &str) -> Vec<Mock> {
    vec![
        mock_head_ref(server, "basecommitsha", 1),
        mock_tags(server, TAGS_V075_V023),
        mock_compare_from(server, "v0.7.5", compare_body),
    ]
}

/// Nothing past analysis may run: no candidate-tag lookup, no repository
/// write, and no release-pull-request activity.
fn mock_nothing_after_analysis(server: &mut ServerGuard) -> Vec<Mock> {
    let mut mocks = mock_no_mutations(server);
    for (method, path) in [
        ("GET", r"^/repos/acme/demo/git/ref/tags/"),
        ("GET", r"^/repos/acme/demo/git/ref/heads/automation/"),
        ("GET", r"^/repos/acme/demo/pulls"),
        ("POST", r"^/repos/acme/demo/pulls$"),
        ("POST", r"^/repos/acme/demo/git/tags$"),
    ] {
        mocks.push(server.mock(method, Matcher::Regex(path.to_owned())).expect(0).create());
    }
    mocks
}

/// Runs a release configured by `configure` against a manifest behind the
/// latest tag and asserts it fails with the guard's error before any write.
fn assert_refused(configure: impl FnOnce(&mut ReleaseOptions), compare_body: &str) {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_behind_analysis(&mut server, compare_body);
    let nothing_after = mock_nothing_after_analysis(&mut server);

    let mut release_options = options(temp_dir.path());
    configure(&mut release_options);
    let error = publisher(&server)
        .release(&release_options)
        .expect_err("a manifest behind the latest tag must not release");

    let message = error.to_string();
    assert!(
        message.contains("Cargo.toml version 0.2.3 is lower than the latest release tag v0.7.5"),
        "unexpected error: {message}"
    );
    assert!(message.contains("Set the manifest version to 0.7.5"), "unexpected error: {message}");
    for mock in &nothing_after {
        mock.assert();
    }
}

/// Push mode would otherwise commit and tag 0.3.0 below v0.7.5.
#[test]
fn direct_release_refuses_a_manifest_behind_the_latest_tag() {
    assert_refused(|_| {}, FEAT_AND_FIX);
}

/// Release-pull-request mode would otherwise propose 0.3.0 below v0.7.5.
#[test]
fn release_pull_request_mode_refuses_a_manifest_behind_the_latest_tag() {
    assert_refused(|release_options| release_options.create_pr = true, FEAT_AND_FIX);
}

/// A dry run fails the same way, so it predicts the real run.
#[test]
fn dry_run_refuses_a_manifest_behind_the_latest_tag() {
    assert_refused(|release_options| release_options.dry_run = true, FEAT_AND_FIX);
    assert_refused(
        |release_options| {
            release_options.dry_run = true;
            release_options.create_pr = true;
        },
        FEAT_AND_FIX,
    );
}

/// A forced bump and both release phases are refused too.
#[test]
fn forced_bumps_and_phases_refuse_a_manifest_behind_the_latest_tag() {
    assert_refused(|release_options| release_options.bump = Some(BumpLevel::Major), CHORE_ONLY);
    assert_refused(|release_options| release_options.phase = ReleasePhase::Bump, FEAT_AND_FIX);
    assert_refused(|release_options| release_options.phase = ReleasePhase::Tag, FEAT_AND_FIX);
}

/// Failing only once a releasable commit lands would hide the drift until the
/// worst moment; the run fails on the first push instead.
#[test]
fn a_manifest_behind_the_latest_tag_fails_even_with_nothing_to_release() {
    assert_refused(|_| {}, CHORE_ONLY);
}

/// Runs a dry run configured by `configure` and returns the recorded action,
/// asserting the run wrote nothing.
fn dry_run_action(
    configure: impl FnOnce(&mut ReleaseOptions),
    mock_reads: impl FnOnce(&mut ServerGuard) -> Vec<Mock>,
) -> Option<ReleaseAction> {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _reads = mock_reads(&mut server);
    let no_mutations = mock_no_mutations(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.dry_run = true;
    configure(&mut release_options);
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    for mock in &no_mutations {
        mock.assert();
    }
    report.action
}

/// Push mode would commit the bump, tag it, and publish the release.
#[test]
fn dry_run_records_the_direct_release_action() {
    let action = dry_run_action(|_| {}, |server| mock_analysis(server, 1, FEAT_AND_FIX));

    assert_eq!(action, Some(ReleaseAction::CommitAndRelease { branch: String::from("main") }));
}

/// The bump phase would commit the bump and stop.
#[test]
fn dry_run_records_the_bump_phase_action() {
    let action = dry_run_action(
        |release_options| release_options.phase = ReleasePhase::Bump,
        |server| mock_analysis(server, 1, FEAT_AND_FIX),
    );

    assert_eq!(action, Some(ReleaseAction::CommitBump { branch: String::from("main") }));
}

/// Release-pull-request mode would refresh the release branch and its pull
/// request instead of tagging.
#[test]
fn dry_run_records_the_release_pull_request_action() {
    let action = dry_run_action(
        |release_options| release_options.create_pr = true,
        |server| mock_analysis(server, 1, FEAT_AND_FIX),
    );

    assert_eq!(
        action,
        Some(ReleaseAction::ProposeReleasePullRequest {
            release_branch: String::from("automation/release"),
            base: String::from("main"),
        })
    );
}

/// After a merged release pull request (manifest 0.2.3, only v0.2.2 tagged)
/// the run would tag the manifest version instead of proposing another bump.
#[test]
fn dry_run_records_the_merged_release_tag_action() {
    let action = dry_run_action(
        |release_options| release_options.create_pr = true,
        |server| {
            vec![
                mock_head_ref(server, "basecommitsha", 1),
                mock_tags(server, r#"[{"name":"v0.2.2","commit":{"sha":"tagsha"}}]"#),
                mock_compare_from(server, "v0.2.2", FEAT_AND_FIX),
                mock_tag_lookup(server, "v0.2.3", 404, r#"{"message":"Not Found"}"#),
            ]
        },
    );

    assert_eq!(action, Some(ReleaseAction::TagManifestVersion { branch: String::from("main") }));
}

/// A run that finds nothing to release records no action.
#[test]
fn skipped_runs_record_no_action() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, r#"[{"name":"v0.2.3","commit":{"sha":"tagsha"}}]"#);
    let _compare = mock_compare_from(&mut server, "v0.2.3", CHORE_ONLY);
    let _no_mutations = mock_no_mutations(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.action, None);
}
