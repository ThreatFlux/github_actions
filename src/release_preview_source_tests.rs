//! Candidate previews must use one immutable commit for history and Cargo data.

use std::{collections::BTreeMap, fs};

use mockito::{Matcher, Mock, Server, ServerGuard};
use tempfile::TempDir;

use super::{TAGS_V023, mock_tag_lookup, mock_tags, options, publisher, write_fixture_repo};
use crate::ReleaseOutcome;

const CANDIDATE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture() -> (TempDir, BTreeMap<String, String>) {
    let repo = write_fixture_repo();
    fs::create_dir(repo.path().join("fuzz")).expect("fuzz directory");
    fs::write(repo.path().join("fuzz/Cargo.toml"),
        "[package]\nname = \"demo-fuzz\"\nversion = \"0.0.0\"\n[dependencies]\ndemo = { path = \"..\", version = \"=0.2.3\" }\n[workspace]\nmembers = [\".\"]\n").expect("consumer manifest");
    let lock = "version = 4\n[[package]]\nname = \"demo\"\nversion = \"0.2.3\"\n[[package]]\nname = \"demo-fuzz\"\nversion = \"0.0.0\"\ndependencies = [\"demo 0.2.3\"]\n";
    fs::write(repo.path().join("Cargo.lock"), lock).expect("root lock");
    fs::write(repo.path().join("fuzz/Cargo.lock"), lock).expect("consumer lock");
    let sources = ["Cargo.toml", "Cargo.lock", "fuzz/Cargo.toml", "fuzz/Cargo.lock"]
        .into_iter()
        .map(|path| (path.to_owned(), fs::read_to_string(repo.path().join(path)).expect("source")))
        .collect();
    (repo, sources)
}

fn sources(server: &mut ServerGuard, data: &BTreeMap<String, String>) -> Vec<Mock> {
    let tree: Vec<_> = data
        .keys()
        .enumerate()
        .map(|(index, path)| {
            format!(r#"{{"path":"{path}","type":"blob","mode":"100644","sha":"blob{index}"}}"#)
        })
        .collect();
    let mut mocks = vec![
        server
            .mock("GET", format!("/repos/acme/demo/git/trees/{CANDIDATE}?recursive=1").as_str())
            .with_status(200)
            .with_body(format!(r#"{{"tree":[{}],"truncated":false}}"#, tree.join(",")))
            .create(),
    ];
    mocks.extend(data.values().enumerate().map(|(index, contents)| {
        server
            .mock("GET", format!("/repos/acme/demo/git/blobs/blob{index}").as_str())
            .match_header("accept", "application/vnd.github.raw+json")
            .expect_at_least(0)
            .with_status(200)
            .with_body(contents)
            .create()
    }));
    mocks
}

fn no_main_or_writes(server: &mut ServerGuard) -> Vec<Mock> {
    let mut mocks =
        vec![server.mock("GET", "/repos/acme/demo/git/ref/heads/main").expect(0).create()];
    mocks.extend(
        ["POST", "PUT", "PATCH", "DELETE"]
            .into_iter()
            .map(|method| server.mock(method, Matcher::Any).expect(0).create()),
    );
    mocks
}

fn history(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        mock_tags(server, TAGS_V023),
        server.mock("GET", format!("/repos/acme/demo/compare/v0.2.3...{CANDIDATE}?per_page=100&page=1").as_str())
            .with_status(200)
            .with_body(r#"{"total_commits":1,"commits":[{"sha":"candidatefix","commit":{"message":"fix: repair candidate"},"parents":[{}]}]}"#)
            .create(),
        mock_tag_lookup(server, "v0.2.4", 404, r#"{"message":"Not Found"}"#),
    ]
}

#[test]
fn candidate_history_and_nested_rewrite_use_the_same_immutable_source_without_writes() {
    let (repo, data) = fixture();
    fs::create_dir(repo.path().join("untracked")).expect("ambient directory");
    fs::write(repo.path().join("untracked/Cargo.toml"), "not valid TOML").expect("ambient data");
    let mut server = Server::new();
    let mut mocks = no_main_or_writes(&mut server);
    mocks.extend(sources(&mut server, &data));
    mocks.extend(history(&mut server));
    let mut opts = options(repo.path());
    opts.dry_run = true;
    let report = publisher(&server).preview_at_commit(&opts, CANDIDATE).expect("candidate preview");
    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert_eq!(report.current_version, "0.2.3");
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    assert_eq!(report.files_updated.len(), 4);
    for (path, original) in data {
        assert_eq!(fs::read_to_string(repo.path().join(path)).expect("unchanged source"), original);
    }
    for mock in mocks {
        mock.assert();
    }
}

#[test]
fn candidate_no_release_report_keeps_the_validated_version_when_checkout_changes() {
    let (repo, data) = fixture();
    let mut server = Server::new();
    let mut mocks = no_main_or_writes(&mut server);
    mocks.extend(sources(&mut server, &data));
    let root_blob = data.keys().position(|path| path == "Cargo.toml").expect("root blob");
    let manifest = repo.path().join("Cargo.toml");
    let validated = data["Cargo.toml"].clone();
    let changed = validated.replace("0.2.3", "0.2.9");
    mocks.push(
        server
            .mock("GET", format!("/repos/acme/demo/git/blobs/blob{root_blob}").as_str())
            .match_header("accept", "application/vnd.github.raw+json")
            .with_status(200)
            .with_chunked_body(move |writer| {
                fs::write(&manifest, &changed)?;
                writer.write_all(validated.as_bytes())
            })
            .create(),
    );
    mocks.push(mock_tags(&mut server, TAGS_V023));
    mocks.push(
        server.mock("GET", format!("/repos/acme/demo/compare/v0.2.3...{CANDIDATE}?per_page=100&page=1").as_str())
            .with_status(200)
            .with_body(r#"{"total_commits":1,"commits":[{"sha":"candidatechore","commit":{"message":"chore: update workflow"},"parents":[{}]}]}"#)
            .create(),
    );
    let mut opts = options(repo.path());
    opts.dry_run = true;
    let report = publisher(&server).preview_at_commit(&opts, CANDIDATE).expect("snapshot report");
    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.current_version, "0.2.3");
    assert!(report.next_version.is_none());
    assert!(
        fs::read_to_string(repo.path().join("Cargo.toml"))
            .expect("changed checkout")
            .contains("0.2.9")
    );
    for mock in mocks {
        mock.assert();
    }
}

#[test]
fn candidate_preview_rejects_dirty_root_before_reading_history() {
    let (repo, data) = fixture();
    fs::write(repo.path().join("Cargo.toml"), data["Cargo.toml"].replace("0.2.3", "0.2.9"))
        .expect("dirty manifest");
    let mut server = Server::new();
    let mut mocks = no_main_or_writes(&mut server);
    mocks.extend(sources(&mut server, &data));
    mocks.push(
        server
            .mock("GET", Matcher::Regex("^/repos/acme/demo/(tags|compare)".to_owned()))
            .expect(0)
            .create(),
    );
    let mut opts = options(repo.path());
    opts.dry_run = true;
    let error = publisher(&server)
        .preview_at_commit(&opts, CANDIDATE)
        .expect_err("ambient manifest refused");
    assert!(error.to_string().contains("differs from the analyzed commit"));
    for mock in mocks {
        mock.assert();
    }
}

#[test]
fn candidate_preview_rejects_dirty_nested_lock_without_mutation() {
    let (repo, data) = fixture();
    fs::write(repo.path().join("fuzz/Cargo.lock"), data["fuzz/Cargo.lock"].clone() + "# ambient\n")
        .expect("dirty consumer");
    let mut server = Server::new();
    let mut mocks = no_main_or_writes(&mut server);
    mocks.extend(sources(&mut server, &data));
    mocks.extend(history(&mut server));
    let mut opts = options(repo.path());
    opts.dry_run = true;
    let error = publisher(&server)
        .preview_at_commit(&opts, CANDIDATE)
        .expect_err("ambient nested lock refused");
    assert!(error.to_string().contains("differs from the analyzed commit"));
    for mock in mocks {
        mock.assert();
    }
}

#[test]
fn candidate_source_cannot_enable_a_writer_or_accept_a_mutable_ref() {
    let repo = write_fixture_repo();
    let mut server = Server::new();
    let no_request = server.mock("GET", Matcher::Any).expect(0).create();
    assert!(publisher(&server).preview_at_commit(&options(repo.path()), CANDIDATE).is_err());
    let mut opts = options(repo.path());
    opts.dry_run = true;
    for source in [
        "",
        "main",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    ] {
        let error = publisher(&server)
            .preview_at_commit(&opts, source)
            .expect_err("immutable SHA required");
        assert!(error.to_string().contains("full immutable"));
    }
    no_request.assert();
}
