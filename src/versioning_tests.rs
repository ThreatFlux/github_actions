use std::fs;

use tempfile::{TempDir, tempdir};

use super::{current_version, matches_member_pattern, plan_version_rewrite};

#[test]
fn current_version_prefers_package_over_workspace() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
    )
    .expect("write Cargo.toml");

    let version = current_version(temp_dir.path()).expect("current version");

    assert_eq!(version, "0.2.3");
}

#[test]
fn current_version_falls_back_to_workspace_package() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"1.4.0\"\n",
    )
    .expect("write Cargo.toml");

    let version = current_version(temp_dir.path()).expect("current version");

    assert_eq!(version, "1.4.0");
}

#[test]
fn current_version_errors_when_no_version_exists() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(temp_dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n")
        .expect("write Cargo.toml");

    let error = current_version(temp_dir.path()).expect_err("missing version");

    assert!(error.to_string().contains("unable to determine current version"));
}

#[test]
fn plan_rewrites_single_package_manifest_preserving_formatting() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n\n# release version\nversion = \"0.2.3\"\nedition = \"2024\"\n\n[dependencies]\nanyhow = \"1.0.95\"\n",
    )
    .expect("write Cargo.toml");

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    assert_eq!(plan.current_version, "0.2.3");
    assert_eq!(plan.internal_packages, vec![String::from("demo")]);
    assert_eq!(plan.file_updates.len(), 1);
    let updated = &plan.file_updates[0].updated_content;
    assert!(updated.contains("# release version\nversion = \"0.3.0\""), "{updated}");
    assert!(updated.contains("anyhow = \"1.0.95\""), "{updated}");
}

fn write_workspace_fixture() -> TempDir {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.2.3\"\n\n[workspace.dependencies]\ndemo-core = { path = \"crates/demo-core\", version = \"0.2.3\" }\n",
    )
    .expect("write root Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("crates/demo-core")).expect("create member");
    fs::write(
        temp_dir.path().join("crates/demo-core/Cargo.toml"),
        "[package]\nname = \"demo-core\"\nversion = \"0.2.3\"\n",
    )
    .expect("write member Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("crates/demo-util")).expect("create member");
    fs::write(
        temp_dir.path().join("crates/demo-util/Cargo.toml"),
        "[package]\nname = \"demo-util\"\nversion = \"0.2.3\"\n",
    )
    .expect("write member Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("crates/demo-cli")).expect("create member");
    fs::write(
        temp_dir.path().join("crates/demo-cli/Cargo.toml"),
        "[package]\nname = \"demo-cli\"\nversion = { workspace = true }\n\n[dependencies]\ndemo-core = { path = \"../demo-core\", version = \"0.2.3\" }\nanyhow = \"1.0.95\"\ndemo-extra = { path = \"../../tools/demo-extra\", version = \"0.2.3\" }\n\n[dependencies.demo-util]\npath = \"../demo-util\"\nversion = \"0.2.3\"\n",
    )
    .expect("write member Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("tools/demo-extra")).expect("create non-member");
    fs::write(
        temp_dir.path().join("tools/demo-extra/Cargo.toml"),
        "[package]\nname = \"demo-extra\"\nversion = \"0.2.3\"\n",
    )
    .expect("write non-member Cargo.toml");
    temp_dir
}

#[test]
fn plan_selects_workspace_members_and_rewrites_root_versions() {
    let temp_dir = write_workspace_fixture();

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    let updated_files: Vec<String> = plan
        .file_updates
        .iter()
        .map(|update| update.file.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(updated_files.iter().any(|file| file.ends_with("crates/demo-core/Cargo.toml")));
    assert!(updated_files.iter().any(|file| file.ends_with("crates/demo-cli/Cargo.toml")));
    assert!(
        !updated_files.iter().any(|file| file.ends_with("tools/demo-extra/Cargo.toml")),
        "non-member manifest must not be rewritten"
    );

    let root = plan
        .file_updates
        .iter()
        .find(|update| {
            update.file
                == temp_dir.path().canonicalize().expect("canonical root").join("Cargo.toml")
        })
        .expect("root update");
    assert!(root.updated_content.contains("version = \"0.3.0\""), "{}", root.updated_content);
    assert!(
        root.updated_content
            .contains("demo-core = { path = \"crates/demo-core\", version = \"0.3.0\" }"),
        "{}",
        root.updated_content
    );
}

#[test]
fn plan_rewrites_member_dependency_shapes() {
    let temp_dir = write_workspace_fixture();

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    let cli = plan
        .file_updates
        .iter()
        .find(|update| update.file.ends_with("crates/demo-cli/Cargo.toml"))
        .expect("cli update");
    assert!(
        cli.updated_content
            .contains("demo-core = { path = \"../demo-core\", version = \"0.3.0\" }"),
        "{}",
        cli.updated_content
    );
    assert!(
        cli.updated_content.contains("version = { workspace = true }"),
        "workspace-inherited package version must not be replaced: {}",
        cli.updated_content
    );
    assert!(cli.updated_content.contains("anyhow = \"1.0.95\""), "{}", cli.updated_content);
    // Dependencies on non-member packages keep their pinned version.
    assert!(
        cli.updated_content
            .contains("demo-extra = { path = \"../../tools/demo-extra\", version = \"0.2.3\" }"),
        "{}",
        cli.updated_content
    );
    // Table-shaped internal dependency ([dependencies.demo-util]).
    assert!(
        cli.updated_content.contains("path = \"../demo-util\"\nversion = \"0.3.0\""),
        "{}",
        cli.updated_content
    );
}

#[test]
fn plan_rewrites_bare_string_internal_dependency() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"lib\"]\n\n[workspace.package]\nversion = \"0.2.3\"\n",
    )
    .expect("write root Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("lib")).expect("create member");
    fs::write(
        temp_dir.path().join("lib/Cargo.toml"),
        "[package]\nname = \"demo-lib\"\nversion = \"0.2.3\"\n\n[dev-dependencies]\ndemo-lib = \"0.2.3\"\n",
    )
    .expect("write member Cargo.toml");

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    let lib = plan
        .file_updates
        .iter()
        .find(|update| update.file.ends_with("lib/Cargo.toml"))
        .expect("lib update");
    assert!(lib.updated_content.contains("demo-lib = \"0.3.0\""), "{}", lib.updated_content);
}

#[test]
fn plan_updates_lockfile_entries_for_internal_packages_only() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        temp_dir.path().join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"anyhow\"\nversion = \"1.0.95\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"abc\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"def\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.2.3\"\ndependencies = [\n \"anyhow\",\n]\n",
    )
    .expect("write Cargo.lock");

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    let lockfile = plan
        .file_updates
        .iter()
        .find(|update| update.file.ends_with("Cargo.lock"))
        .expect("lockfile update");
    assert!(
        lockfile.updated_content.contains("# This file is automatically @generated by Cargo."),
        "{}",
        lockfile.updated_content
    );
    assert!(lockfile.updated_content.contains("version = 4"), "{}", lockfile.updated_content);
    assert!(
        lockfile.updated_content.contains("name = \"anyhow\"\nversion = \"1.0.95\""),
        "{}",
        lockfile.updated_content
    );
    // The registry entry sharing the member's name keeps its version.
    assert!(
        lockfile.updated_content.contains("name = \"demo\"\nversion = \"0.1.0\""),
        "{}",
        lockfile.updated_content
    );
    assert!(
        lockfile.updated_content.contains("name = \"demo\"\nversion = \"0.3.0\""),
        "{}",
        lockfile.updated_content
    );
}

#[test]
fn plan_honors_workspace_exclude() {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/fixture\"]\n\n[workspace.package]\nversion = \"0.2.3\"\n",
    )
    .expect("write root Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("crates/demo-core")).expect("create member");
    fs::write(
        temp_dir.path().join("crates/demo-core/Cargo.toml"),
        "[package]\nname = \"demo-core\"\nversion = \"0.2.3\"\n",
    )
    .expect("write member Cargo.toml");
    fs::create_dir_all(temp_dir.path().join("crates/fixture")).expect("create excluded");
    fs::write(
        temp_dir.path().join("crates/fixture/Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.2.3\"\n",
    )
    .expect("write excluded Cargo.toml");

    let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

    let updated_files: Vec<String> = plan
        .file_updates
        .iter()
        .map(|update| update.file.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(updated_files.iter().any(|file| file.ends_with("crates/demo-core/Cargo.toml")));
    assert!(
        !updated_files.iter().any(|file| file.ends_with("crates/fixture/Cargo.toml")),
        "excluded manifest must not be rewritten"
    );
    assert!(!plan.internal_packages.contains(&String::from("fixture")));
}

#[test]
fn member_patterns_respect_segment_boundaries() {
    assert!(matches_member_pattern("crates/*", "crates/demo"));
    assert!(!matches_member_pattern("crates/*", "crates/demo/nested"));
    assert!(matches_member_pattern("crates/**", "crates/demo/nested"));
    assert!(matches_member_pattern("**/demo", "a/b/demo"));
    assert!(matches_member_pattern("crates/demo-?", "crates/demo-a"));
    assert!(!matches_member_pattern("crates/demo-?", "crates/demo-ab"));
    assert!(matches_member_pattern("lib", "lib"));
    assert!(!matches_member_pattern("lib", "libs"));
}
