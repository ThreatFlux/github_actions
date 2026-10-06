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
    CHORE_ONLY, FEAT_AND_FIX, mock_analysis, mock_head_ref, mock_no_mutations, mock_tags, options,
    publisher, write_fixture_repo,
};
use crate::{ReleaseAction, ReleaseOptions, ReleaseOutcome, ReleasePhase};

/// The fixture manifest is 0.2.3; the highest tag is v0.7.5.
const TAGS_V075_V023: &str = r#"[{"name":"v0.2.3","commit":{"sha":"tag023sha"}},{"name":"v0.7.5","commit":{"sha":"tag075sha"}}]"#;

fn mock_behind_analysis(server: &mut ServerGuard, compare_body: &str) -> Vec<Mock> {
    vec![
        mock_head_ref(server, "basecommitsha", 1),
        mock_tags(server, TAGS_V075_V023),
        server
            .mock("GET", "/repos/acme/demo/compare/v0.7.5...basecommitsha?per_page=100&page=1")
            .with_status(200)
            .with_body(compare_body)
            .create(),
    ]
}

/// Nothing past analysis may run: no candidate-tag lookup, no repository
/// write, and no release-pull-request activity.
fn mock_nothing_after_analysis(server: &mut ServerGuard) -> Vec<Mock> {
    let mut mocks = mock_no_mutations(server);
    mocks.extend([
        server
            .mock("GET", Matcher::Regex(r"^/repos/acme/demo/git/ref/tags/".into()))
            .expect(0)
            .create(),
        server
            .mock("GET", Matcher::Regex(r"^/repos/acme/demo/git/ref/heads/automation/".into()))
            .expect(0)
            .create(),
        server.mock("GET", Matcher::Regex(r"^/repos/acme/demo/pulls".into())).expect(0).create(),
        server.mock("POST", "/repos/acme/demo/pulls").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/git/tags").expect(0).create(),
    ]);
    mocks
}

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

#[test]
fn direct_release_refuses_a_manifest_behind_the_latest_tag() {
    assert_refused(|_| {}, FEAT_AND_FIX);
}

#[test]
fn release_pull_request_mode_refuses_a_manifest_behind_the_latest_tag() {
    assert_refused(|release_options| release_options.create_pr = true, FEAT_AND_FIX);
}

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

#[test]
fn forced_bumps_and_phases_refuse_a_manifest_behind_the_latest_tag() {
    assert_refused(
        |release_options| release_options.bump = Some(crate::BumpLevel::Major),
        CHORE_ONLY,
    );
    assert_refused(|release_options| release_options.phase = ReleasePhase::Bump, FEAT_AND_FIX);
    assert_refused(|release_options| release_options.phase = ReleasePhase::Tag, FEAT_AND_FIX);
}

#[test]
fn a_manifest_behind_the_latest_tag_fails_even_with_nothing_to_release() {
    // Failing only once a releasable commit lands would hide the drift until
    // the worst moment; the run fails on the first push instead.
    assert_refused(|_| {}, CHORE_ONLY);
}

#[test]
fn dry_run_records_the_direct_release_action() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _no_mutations = mock_no_mutations(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.dry_run = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(
        report.action,
        Some(ReleaseAction::CommitAndRelease { branch: String::from("main") })
    );
}

#[test]
fn dry_run_records_the_bump_phase_action() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _no_mutations = mock_no_mutations(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.dry_run = true;
    release_options.phase = ReleasePhase::Bump;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.action, Some(ReleaseAction::CommitBump { branch: String::from("main") }));
}

#[test]
fn skipped_runs_record_no_action() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, r#"[{"name":"v0.2.3","commit":{"sha":"tagsha"}}]"#);
    let _compare = server
        .mock("GET", "/repos/acme/demo/compare/v0.2.3...basecommitsha?per_page=100&page=1")
        .with_status(200)
        .with_body(CHORE_ONLY)
        .create();
    let _no_mutations = mock_no_mutations(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.action, None);
}
