use std::{collections::BTreeSet, fs, path::Path};

use tempfile::{TempDir, tempdir};

use super::{TreeRewritePlan, plan_version_rewrite_in_tree};

const ROOT: &str = "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n";
const LOCK: &str = r#"# keep this lock comment
version = 4

[[package]]
name = "demo"
version = "0.2.3"

[[package]]
name = "demo"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "retain-registry-checksum"

[[package]]
name = "demo-fuzz"
version = "0.0.0"
dependencies = [
 "demo 0.2.3", # actual owned path reference
 "demo 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
]
"#;

fn write(root: &Path, relative: &str, contents: &str) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().expect("parent")).expect("directories");
    fs::write(file, contents).expect("fixture source");
}

fn fixture(dependencies: &str) -> (TempDir, BTreeSet<String>) {
    let repo = tempdir().expect("temporary repository");
    write(repo.path(), "Cargo.toml", ROOT);
    write(repo.path(), "Cargo.lock", LOCK);
    write(
        repo.path(),
        "fuzz/Cargo.toml",
        &format!(
            "[package]\nname = \"demo-fuzz\"\nversion = \"0.0.0\"\npublish = false\n{dependencies}\n[workspace]\nmembers = [\".\"]\n"
        ),
    );
    write(repo.path(), "fuzz/Cargo.lock", LOCK);
    let tracked = ["Cargo.toml", "Cargo.lock", "fuzz/Cargo.toml", "fuzz/Cargo.lock"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    (repo, tracked)
}

fn updated<'a>(rewrite: &'a TreeRewritePlan, relative: &str) -> &'a str {
    &rewrite
        .plan
        .file_updates
        .iter()
        .find(|update| update.file.ends_with(relative))
        .expect("planned update")
        .updated_content
}

#[test]
fn nested_fuzz_bump_updates_owned_path_requirements_and_lock_references() {
    let (repo, tracked) = fixture(
        "[dependencies]\ndemo = { path = \"..\", version = \"=0.2.3\", default-features = false }",
    );
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    let manifest = updated(&rewrite, "fuzz/Cargo.toml");
    assert!(manifest.contains("version = \"0.0.0\""));
    assert!(manifest.contains("version = \"0.3.0\", default-features = false"));
    for relative in ["Cargo.lock", "fuzz/Cargo.lock"] {
        let lock = updated(&rewrite, relative);
        assert!(lock.contains("# keep this lock comment"));
        assert!(lock.contains("name = \"demo\"\nversion = \"0.3.0\""));
        assert!(lock.contains("\"demo 0.3.0\", # actual owned path reference"));
        assert!(lock.contains("name = \"demo-fuzz\"\nversion = \"0.0.0\""));
        assert!(lock.contains("name = \"demo\"\nversion = \"0.1.0\"\nsource ="));
        assert!(
            lock.contains("\"demo 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)\"")
        );
        assert!(lock.contains("retain-registry-checksum"));
    }
    assert_eq!(
        fs::read_to_string(repo.path().join("fuzz/Cargo.lock")).expect("unchanged source"),
        LOCK
    );
}

#[test]
fn target_renamed_and_table_shaped_path_requirements_are_rewritten() {
    let (repo, tracked) = fixture(
        "[target.'cfg(unix)'.dev-dependencies.renamed]\npackage = \"demo\"\npath = \"..\"\nversion = \"~0.2.3\"\nfeatures = [\"preserve\"]",
    );
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    let manifest = updated(&rewrite, "fuzz/Cargo.toml");
    assert!(manifest.contains("version = \"0.3.0\""));
    assert!(manifest.contains("features = [\"preserve\"]"));
    assert!(manifest.contains("package = \"demo\""));
}

#[test]
fn inherited_alias_updates_declaring_workspace_requirement_only() {
    let (repo, tracked) = fixture("[dependencies]\nrenamed.workspace = true");
    let file = repo.path().join("fuzz/Cargo.toml");
    let content = fs::read_to_string(&file).expect("manifest")
        + "\n[workspace.dependencies]\nrenamed = { package = \"demo\", path = \"..\", version = \"=0.2.3\" }\n";
    fs::write(file, content).expect("inherited manifest");
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    let manifest = updated(&rewrite, "fuzz/Cargo.toml");
    assert!(manifest.contains("renamed.workspace = true"));
    assert!(manifest.contains("version = \"0.0.0\""));
    assert!(manifest.contains("path = \"..\", version = \"0.3.0\""));
}

#[test]
fn transitive_path_consumer_preserves_intermediate_package_version() {
    let (repo, mut tracked) = fixture("[dependencies]\nbridge = { path = \"../bridge\" }");
    write(
        repo.path(),
        "bridge/Cargo.toml",
        "[package]\nname = \"bridge\"\nversion = \"8.1.0\"\n[dependencies]\nrenamed = { package = \"demo\", path = \"..\", version = \"0.2.3\" }\n",
    );
    tracked.insert("bridge/Cargo.toml".to_owned());
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    let bridge = updated(&rewrite, "bridge/Cargo.toml");
    assert!(bridge.contains("version = \"8.1.0\""));
    assert!(bridge.contains("path = \"..\", version = \"0.3.0\""));
    assert!(updated(&rewrite, "fuzz/Cargo.lock").contains("name = \"demo\"\nversion = \"0.3.0\""));
}

#[test]
fn tracked_historical_copy_with_identical_name_and_version_is_not_owned() {
    let (repo, mut tracked) =
        fixture("[dependencies]\ndemo = { path = \"../historical\", version = \"=0.2.3\" }");
    write(repo.path(), "historical/Cargo.toml", ROOT);
    tracked.insert("historical/Cargo.toml".to_owned());
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    assert!(rewrite.plan.file_updates.iter().all(
        |update| !update.file.ends_with("fuzz/Cargo.lock")
            && !update.file.ends_with("fuzz/Cargo.toml")
            && !update.file.ends_with("historical/Cargo.toml")
    ));
}

#[test]
fn workspace_inherited_owned_version_bumps_but_excluded_historical_member_does_not() {
    let (repo, mut tracked) =
        fixture("[dependencies]\ndemo = { path = \"../crates/core\", version = \"0.2.3\" }");
    write(
        repo.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/historical\"]\n[workspace.package]\nversion = \"0.2.3\"\n",
    );
    write(
        repo.path(),
        "crates/core/Cargo.toml",
        "[package]\nname = \"demo\"\nversion.workspace = true\n",
    );
    write(repo.path(), "crates/historical/Cargo.toml", ROOT);
    tracked.extend(
        ["crates/core/Cargo.toml", "crates/historical/Cargo.toml"]
            .into_iter()
            .map(ToOwned::to_owned),
    );
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    assert!(updated(&rewrite, "fuzz/Cargo.lock").contains("name = \"demo\"\nversion = \"0.3.0\""));
    assert!(updated(&rewrite, "fuzz/Cargo.toml").contains("version = \"0.3.0\""));
    assert!(
        rewrite
            .plan
            .file_updates
            .iter()
            .all(|update| !update.file.ends_with("crates/historical/Cargo.toml"))
    );
    assert!(
        !rewrite.original_files.keys().any(|file| file.ends_with("crates/historical/Cargo.toml"))
    );
}

#[test]
fn untracked_nested_sources_are_never_proposed() {
    let (repo, mut tracked) = fixture("[dependencies]\ndemo = { path = \"..\" }");
    tracked.remove("fuzz/Cargo.lock");
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    assert!(
        rewrite.plan.file_updates.iter().all(|update| !update.file.ends_with("fuzz/Cargo.lock"))
    );
    assert!(rewrite.original_files.keys().all(|file| !file.ends_with("fuzz/Cargo.toml")));
}

#[test]
fn owned_patch_path_fails_closed_instead_of_leaving_incompatible_requirements() {
    let (repo, tracked) =
        fixture("[dependencies]\ndemo = \"=0.2.3\"\n[patch.crates-io]\ndemo = { path = \"..\" }");
    let Err(error) = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked) else {
        panic!("unsupported owned patch must fail closed");
    };
    assert!(error.to_string().contains("owned path package in nested [patch]"));
}

#[cfg(unix)]
#[test]
fn symlink_lockfile_is_not_a_release_consumer() {
    let (repo, tracked) = fixture("[dependencies]\ndemo = { path = \"..\" }");
    fs::remove_file(repo.path().join("fuzz/Cargo.lock")).expect("remove fixture lock");
    std::os::unix::fs::symlink("../Cargo.lock", repo.path().join("fuzz/Cargo.lock"))
        .expect("symlink");
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("plan");
    assert!(
        rewrite.plan.file_updates.iter().all(|update| !update.file.ends_with("fuzz/Cargo.lock"))
    );
}
