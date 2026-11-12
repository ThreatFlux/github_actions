//! Full publisher regressions for independent active path-consumer lockfiles.
use std::{collections::BTreeMap, fs, path::Path};

use mockito::{Matcher, Mock, Server, ServerGuard};
use tempfile::TempDir;

use super::{FEAT_AND_FIX, mock_analysis, options, publisher, write_fixture_repo};
use crate::{ReleaseOutcome, ReleasePhase};

fn nested_fixture() -> (TempDir, BTreeMap<String, String>) {
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

fn source_mocks(
    server: &mut ServerGuard,
    sources: &BTreeMap<String, String>,
    truncated: bool,
) -> Vec<Mock> {
    let entries: Vec<_> = sources
        .keys()
        .enumerate()
        .map(|(index, path)| {
            format!(r#"{{"path":"{path}","type":"blob","mode":"100644","sha":"source{index}"}}"#)
        })
        .collect();
    let mut mocks = vec![
        server
            .mock("GET", "/repos/acme/demo/git/trees/basecommitsha?recursive=1")
            .expect(1)
            .with_status(200)
            .with_body(format!(r#"{{"tree":[{}],"truncated":{truncated}}}"#, entries.join(",")))
            .create(),
    ];
    if !truncated {
        mocks.extend(sources.values().enumerate().map(|(index, contents)| {
            server
                .mock("GET", format!("/repos/acme/demo/git/blobs/source{index}").as_str())
                .match_header("accept", "application/vnd.github.raw+json")
                .with_status(200)
                .with_body(contents)
                .create()
        }));
    }
    mocks
}

fn no_writes(server: &mut ServerGuard) -> Vec<Mock> {
    ["POST", "PATCH"]
        .into_iter()
        .map(|method| {
            server.mock(method, Matcher::Regex("^/repos/acme/demo/".to_owned())).expect(0).create()
        })
        .collect()
}

fn bump_write_mocks(server: &mut ServerGuard) -> Vec<Mock> {
    let paths = ["Cargo.lock", "Cargo.toml", "fuzz/Cargo.lock", "fuzz/Cargo.toml"];
    let tree: Vec<_> = paths
        .into_iter()
        .map(|path| format!(r#"{{"path":"{path}","mode":"100644","type":"blob","sha":"blobsha"}}"#))
        .collect();
    vec![
        server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"tree":{"sha":"basetreesha"}}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .expect(4)
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/trees")
            .match_body(Matcher::JsonString(format!(
                r#"{{"base_tree":"basetreesha","tree":[{}]}}"#,
                tree.join(",")
            )))
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create(),
        server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .match_body(Matcher::JsonString(r#"{"sha":"newcommitsha","force":false}"#.to_owned()))
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create(),
    ]
}

#[test]
fn real_bump_commit_contains_both_lockfiles_and_only_owned_dependency_requirement() {
    let (repo, sources) = nested_fixture();
    let mut server = Server::new();
    let mut mocks = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    mocks.extend(source_mocks(&mut server, &sources, false));
    mocks.extend(bump_write_mocks(&mut server));
    let mut options = options(repo.path());
    options.phase = ReleasePhase::Bump;
    let report = publisher(&server).release(&options).expect("tracked source bump");
    assert_eq!(report.outcome, ReleaseOutcome::VersionCommitted);
    assert_eq!(report.files_updated.len(), 4);
    assert_eq!(
        fs::read_to_string(repo.path().join("fuzz/Cargo.lock")).expect("local lock"),
        sources["fuzz/Cargo.lock"]
    );
    for mock in mocks {
        mock.assert();
    }
}

#[test]
fn dirty_nested_source_fails_before_any_repository_write() {
    let (repo, sources) = nested_fixture();
    fs::write(
        repo.path().join("fuzz/Cargo.lock"),
        sources["fuzz/Cargo.lock"].clone() + "# ambient change\n",
    )
    .expect("dirty local source");
    let mut server = Server::new();
    let _reads = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _sources = source_mocks(&mut server, &sources, false);
    let writes = no_writes(&mut server);
    let error =
        publisher(&server).release(&options(repo.path())).expect_err("dirty source rejected");
    assert!(error.to_string().contains("differs from the analyzed commit"));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn truncated_tree_fails_before_any_repository_write() {
    let (repo, sources) = nested_fixture();
    let mut server = Server::new();
    let _reads = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _sources = source_mocks(&mut server, &sources, true);
    let writes = no_writes(&mut server);
    let error =
        publisher(&server).release(&options(repo.path())).expect_err("partial source rejected");
    assert!(error.to_string().contains("source tree is truncated"));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn pr_dry_run_previews_changed_nested_sources_without_main_tree_binding_or_writes() {
    let (repo, sources) = nested_fixture();
    fs::write(
        repo.path().join("fuzz/Cargo.lock"),
        sources["fuzz/Cargo.lock"].clone() + "# PR-only source\n",
    )
    .expect("PR lock");
    let mut server = Server::new();
    let _reads = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let no_tree = server
        .mock("GET", "/repos/acme/demo/git/trees/basecommitsha?recursive=1")
        .expect(0)
        .create();
    let writes = no_writes(&mut server);
    let mut options = options(repo.path());
    options.dry_run = true;
    let report = publisher(&server).release(&options).expect("read-only PR preview");
    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert!(report.files_updated.iter().any(|file| file.ends_with(Path::new("fuzz/Cargo.lock"))));
    assert!(
        fs::read_to_string(repo.path().join("fuzz/Cargo.lock"))
            .expect("local lock")
            .contains("# PR-only source")
    );
    no_tree.assert();
    for mock in writes {
        mock.assert();
    }
}
