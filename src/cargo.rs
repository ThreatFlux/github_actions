use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use semver::Version;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};
use walkdir::{DirEntry, WalkDir};

use crate::{
    crates_io::CratesIoClient,
    model::{FileUpdate, UpdateChange, UpdateChangeKind},
    update::UpdateMode,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CargoUpdateOptions {
    pub repo_root: PathBuf,
    pub mode: UpdateMode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CargoDependencyEntry {
    pub file: PathBuf,
    pub dependency_name: String,
    pub current_requirement: Option<String>,
    pub latest_version: Option<String>,
    pub update_needed: bool,
    pub managed: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct CargoUpdateReport {
    pub manifest_files: usize,
    pub dependencies_scanned: usize,
    pub unmanaged_dependencies: usize,
    pub entries: Vec<CargoDependencyEntry>,
    pub changes: Vec<UpdateChange>,
    pub file_updates: Vec<FileUpdate>,
}

#[derive(Debug, Clone)]
pub struct CargoUpdater {
    crates_io: CratesIoClient,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ManifestDependency {
    file: PathBuf,
    dependency_name: String,
    item_path: Vec<String>,
    current_requirement: Option<String>,
    managed: bool,
    reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ParsedRequirement {
    operator: String,
    version: Version,
}

#[derive(Debug, Default)]
struct ManifestUpdateResult {
    entries: Vec<CargoDependencyEntry>,
    changes: Vec<UpdateChange>,
    file_update: Option<FileUpdate>,
    unmanaged_dependencies: usize,
}

impl CargoUpdater {
    #[must_use]
    pub const fn new(crates_io: CratesIoClient) -> Self {
        Self { crates_io }
    }

    pub fn update(&self, options: &CargoUpdateOptions) -> Result<CargoUpdateReport> {
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;
        let manifest_files = discover_manifest_files(&repo_root);
        let mut latest_version_cache = HashMap::<String, String>::new();
        let mut entries = Vec::new();
        let mut dependency_changes = Vec::new();
        let mut file_updates = Vec::new();
        let mut unmanaged_dependencies = 0usize;

        for manifest in &manifest_files {
            let manifest_result =
                self.process_manifest(manifest, options.mode, &mut latest_version_cache)?;
            unmanaged_dependencies += manifest_result.unmanaged_dependencies;
            entries.extend(manifest_result.entries);
            dependency_changes.extend(manifest_result.changes);
            if let Some(file_update) = manifest_result.file_update {
                file_updates.push(file_update);
            }
        }

        if options.mode == UpdateMode::Apply && !file_updates.is_empty() {
            write_file_updates(&file_updates)?;
            refresh_lockfiles(&repo_root)?;
        }

        Ok(CargoUpdateReport {
            manifest_files: manifest_files.len(),
            dependencies_scanned: entries.len(),
            unmanaged_dependencies,
            entries,
            changes: dependency_changes,
            file_updates,
        })
    }

    fn process_manifest(
        &self,
        manifest: &Path,
        mode: UpdateMode,
        latest_version_cache: &mut HashMap<String, String>,
    ) -> Result<ManifestUpdateResult> {
        let original = fs::read_to_string(manifest)
            .with_context(|| format!("failed to read Cargo manifest '{}'", manifest.display()))?;
        let mut document = original
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse Cargo manifest '{}'", manifest.display()))?;
        let dependencies = collect_dependencies(manifest, &document);
        let mut manifest_result = ManifestUpdateResult::default();
        let mut manifest_changed = false;

        for dependency in dependencies {
            if let Some(entry) = build_unmanaged_entry(&dependency) {
                manifest_result.unmanaged_dependencies += 1;
                manifest_result.entries.push(entry);
                continue;
            }

            let current_requirement = dependency
                .current_requirement
                .clone()
                .expect("managed dependencies always have a requirement");
            let Some(parsed_requirement) = parse_requirement(&current_requirement) else {
                manifest_result.unmanaged_dependencies += 1;
                manifest_result.entries.push(CargoDependencyEntry {
                    file: dependency.file.clone(),
                    dependency_name: dependency.dependency_name.clone(),
                    current_requirement: Some(current_requirement),
                    latest_version: None,
                    update_needed: false,
                    managed: false,
                    reason: Some(String::from("unsupported version requirement")),
                });
                continue;
            };

            let latest_version = cached_latest_version(
                latest_version_cache,
                &self.crates_io,
                &dependency.dependency_name,
            )?;
            let latest_parsed = Version::parse(&latest_version)
                .with_context(|| format!("invalid crates.io version '{latest_version}'"))?;
            let update_needed = latest_parsed > parsed_requirement.version;

            manifest_result.entries.push(CargoDependencyEntry {
                file: dependency.file.clone(),
                dependency_name: dependency.dependency_name.clone(),
                current_requirement: Some(current_requirement.clone()),
                latest_version: Some(latest_version.clone()),
                update_needed,
                managed: true,
                reason: None,
            });

            if update_needed && mode != UpdateMode::Status {
                let new_requirement = rewrite_requirement(&current_requirement, &latest_version)
                    .with_context(|| {
                        format!(
                            "failed to rewrite requirement '{}' for dependency '{}'",
                            current_requirement, dependency.dependency_name
                        )
                    })?;
                update_dependency_requirement(
                    &mut document,
                    &dependency.item_path,
                    &new_requirement,
                )?;
                manifest_result.changes.push(UpdateChange {
                    kind: UpdateChangeKind::CargoDependency,
                    file: dependency.file.clone(),
                    line_number: None,
                    subject: dependency.dependency_name.clone(),
                    from_version: current_requirement,
                    to_version: new_requirement,
                });
                manifest_changed = true;
            }
        }

        if manifest_changed {
            manifest_result.file_update = Some(FileUpdate {
                file: manifest.to_path_buf(),
                updated_content: document.to_string(),
            });
        }

        Ok(manifest_result)
    }
}

fn discover_manifest_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut files = WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(should_scan_entry)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "Cargo.toml")
        .map(DirEntry::into_path)
        .collect::<Vec<_>>();

    files.sort();
    files
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if !entry.file_type().is_dir() {
        return true;
    }

    !matches!(entry.file_name().to_str(), Some("target" | ".git" | ".hg" | ".svn" | "node_modules"))
}

fn collect_dependencies(file: &Path, document: &DocumentMut) -> Vec<ManifestDependency> {
    let mut dependencies = Vec::new();

    collect_dependency_table(
        file,
        &mut dependencies,
        document.get("dependencies"),
        &["dependencies"],
    );
    collect_dependency_table(
        file,
        &mut dependencies,
        document.get("dev-dependencies"),
        &["dev-dependencies"],
    );
    collect_dependency_table(
        file,
        &mut dependencies,
        document.get("build-dependencies"),
        &["build-dependencies"],
    );

    if let Some(workspace_item) = document.get("workspace")
        && let Some(workspace_table) = workspace_item.as_table()
    {
        collect_dependency_table(
            file,
            &mut dependencies,
            workspace_table.get("dependencies"),
            &["workspace", "dependencies"],
        );
    }

    if let Some(target_item) = document.get("target")
        && let Some(target_table) = target_item.as_table()
    {
        for (target_name, target_config) in target_table {
            let Some(target_config) = target_config.as_table() else {
                continue;
            };
            collect_dependency_table(
                file,
                &mut dependencies,
                target_config.get("dependencies"),
                &["target", target_name, "dependencies"],
            );
            collect_dependency_table(
                file,
                &mut dependencies,
                target_config.get("dev-dependencies"),
                &["target", target_name, "dev-dependencies"],
            );
            collect_dependency_table(
                file,
                &mut dependencies,
                target_config.get("build-dependencies"),
                &["target", target_name, "build-dependencies"],
            );
        }
    }

    dependencies
}

fn collect_dependency_table(
    file: &Path,
    dependencies: &mut Vec<ManifestDependency>,
    item: Option<&Item>,
    table_path: &[&str],
) {
    let Some(item) = item else {
        return;
    };
    let Some(table) = item.as_table() else {
        return;
    };

    for (name, dependency_item) in table {
        let analysis = analyze_dependency_item(dependency_item);
        let mut item_path =
            table_path.iter().map(|segment| (*segment).to_owned()).collect::<Vec<_>>();
        item_path.push(name.to_owned());
        dependencies.push(ManifestDependency {
            file: file.to_path_buf(),
            dependency_name: name.to_owned(),
            item_path,
            current_requirement: analysis.current_requirement,
            managed: analysis.managed,
            reason: analysis.reason,
        });
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DependencyAnalysis {
    current_requirement: Option<String>,
    managed: bool,
    reason: Option<String>,
}

fn analyze_dependency_item(item: &Item) -> DependencyAnalysis {
    if let Some(value) = item.as_value()
        && let Some(requirement) = value.as_str()
    {
        return DependencyAnalysis {
            current_requirement: Some(requirement.to_owned()),
            managed: true,
            reason: None,
        };
    }

    if let Some(inline_table) = item.as_inline_table() {
        return analyze_inline_table(inline_table);
    }

    if let Some(table) = item.as_table() {
        return analyze_table(table);
    }

    DependencyAnalysis {
        current_requirement: None,
        managed: false,
        reason: Some(String::from("unsupported dependency declaration")),
    }
}

fn analyze_inline_table(table: &InlineTable) -> DependencyAnalysis {
    if table.contains_key("path") {
        return unmanaged_reason("path dependency");
    }
    if table.contains_key("git") {
        return unmanaged_reason("git dependency");
    }
    if table.contains_key("workspace") {
        return unmanaged_reason("workspace dependency");
    }

    let current_requirement = table.get("version").and_then(Value::as_str).map(ToOwned::to_owned);
    if current_requirement.is_some() {
        return DependencyAnalysis { current_requirement, managed: true, reason: None };
    }

    unmanaged_reason("missing version requirement")
}

fn analyze_table(table: &Table) -> DependencyAnalysis {
    if table.contains_key("path") {
        return unmanaged_reason("path dependency");
    }
    if table.contains_key("git") {
        return unmanaged_reason("git dependency");
    }
    if table.contains_key("workspace") {
        return unmanaged_reason("workspace dependency");
    }

    let current_requirement = table
        .get("version")
        .and_then(Item::as_value)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if current_requirement.is_some() {
        return DependencyAnalysis { current_requirement, managed: true, reason: None };
    }

    unmanaged_reason("missing version requirement")
}

fn unmanaged_reason(reason: &str) -> DependencyAnalysis {
    DependencyAnalysis {
        current_requirement: None,
        managed: false,
        reason: Some(reason.to_owned()),
    }
}

fn build_unmanaged_entry(dependency: &ManifestDependency) -> Option<CargoDependencyEntry> {
    (!dependency.managed).then(|| CargoDependencyEntry {
        file: dependency.file.clone(),
        dependency_name: dependency.dependency_name.clone(),
        current_requirement: dependency.current_requirement.clone(),
        latest_version: None,
        update_needed: false,
        managed: false,
        reason: dependency.reason.clone(),
    })
}

fn cached_latest_version(
    latest_version_cache: &mut HashMap<String, String>,
    crates_io: &CratesIoClient,
    dependency_name: &str,
) -> Result<String> {
    if let Some(version) = latest_version_cache.get(dependency_name) {
        return Ok(version.clone());
    }

    let version = crates_io.latest_stable_version(dependency_name)?;
    latest_version_cache.insert(dependency_name.to_owned(), version.clone());
    Ok(version)
}

fn parse_requirement(raw: &str) -> Option<ParsedRequirement> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains(',')
        || trimmed.contains('*')
        || trimmed.contains('>')
        || trimmed.contains('<')
        || trimmed.contains(' ')
    {
        return None;
    }

    let (operator, version_text) = [
        ("^", trimmed.strip_prefix('^')),
        ("~", trimmed.strip_prefix('~')),
        ("=", trimmed.strip_prefix('=')),
    ]
    .into_iter()
    .find_map(|(operator, version)| version.map(|version| (operator, version)))
    .unwrap_or(("", trimmed));

    let normalized = normalize_version(version_text.trim())?;
    let version = Version::parse(&normalized).ok()?;
    Some(ParsedRequirement { operator: operator.to_owned(), version })
}

fn normalize_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return None;
    }

    let dots = trimmed.matches('.').count();
    let normalized = match dots {
        0 => format!("{trimmed}.0.0"),
        1 => format!("{trimmed}.0"),
        _ => trimmed.to_owned(),
    };

    Some(normalized)
}

fn rewrite_requirement(current_requirement: &str, latest_version: &str) -> Option<String> {
    let parsed = parse_requirement(current_requirement)?;
    Some(format!("{}{}", parsed.operator, latest_version))
}

fn update_dependency_requirement(
    document: &mut DocumentMut,
    item_path: &[String],
    new_requirement: &str,
) -> Result<()> {
    let item = get_item_mut(document.as_item_mut(), item_path).ok_or_else(|| {
        anyhow::anyhow!("failed to find dependency item '{}'", item_path.join("."))
    })?;

    if let Some(inline_table) = item.as_inline_table_mut() {
        inline_table.insert("version", Value::from(new_requirement));
        return Ok(());
    }

    if let Some(table) = item.as_table_mut() {
        table["version"] = value(new_requirement);
        return Ok(());
    }

    if item.is_value() {
        *item = value(new_requirement);
        return Ok(());
    }

    bail!("unsupported dependency item for '{}'", item_path.join("."))
}

fn get_item_mut<'a>(item: &'a mut Item, path: &[String]) -> Option<&'a mut Item> {
    if path.is_empty() {
        return Some(item);
    }

    let table_like = item.as_table_like_mut()?;
    let next = table_like.get_mut(&path[0])?;
    get_item_mut(next, &path[1..])
}

fn refresh_lockfiles(repo_root: &Path) -> Result<()> {
    let manifest_paths = discover_lockfile_manifests(repo_root);

    for manifest_path in manifest_paths {
        let output = Command::new("cargo")
            .arg("update")
            .arg("--workspace")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .current_dir(
                manifest_path
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("manifest has no parent directory"))?,
            )
            .output()
            .with_context(|| {
                format!("failed to refresh Cargo.lock for manifest '{}'", manifest_path.display())
            })?;

        if !output.status.success() {
            bail!(
                "cargo update failed for '{}': {}",
                manifest_path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }

    Ok(())
}

fn write_file_updates(file_updates: &[FileUpdate]) -> Result<()> {
    for file_update in file_updates {
        fs::write(&file_update.file, &file_update.updated_content).with_context(|| {
            format!("failed to write updated Cargo manifest '{}'", file_update.file.display())
        })?;
    }

    Ok(())
}

fn discover_lockfile_manifests(repo_root: &Path) -> Vec<PathBuf> {
    let mut manifests = BTreeSet::new();

    for entry in WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(should_scan_entry)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "Cargo.lock")
    {
        let manifest = entry.path().with_file_name("Cargo.toml");
        if manifest.exists() {
            manifests.insert(manifest);
        }
    }

    manifests.into_iter().collect()
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::fs;

    use mockito::Server;
    use tempfile::tempdir;

    use super::{CargoUpdateOptions, CargoUpdater};
    use crate::{CratesIoClient, UpdateMode};

    #[test]
    fn status_reports_registry_dependencies_and_skips_unmanaged_entries() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1.0.95"
serde = { version = "^1.0.200", features = ["derive"] }
local-crate = { path = "../local-crate" }
git-crate = { git = "https://github.com/example/git-crate" }

[target.'cfg(unix)'.dependencies]
regex = "~1.10.0"
"#,
        )
        .expect("write Cargo.toml");

        let mut server = Server::new();
        let _anyhow = server
            .mock("GET", "/crates/anyhow")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"anyhow","name":"anyhow","max_version":"1.0.100","max_stable_version":"1.0.100","newest_version":"1.0.100"}}"#,
            )
            .create();
        let _serde = server
            .mock("GET", "/crates/serde")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"serde","name":"serde","max_version":"1.0.219","max_stable_version":"1.0.219","newest_version":"1.0.219"}}"#,
            )
            .create();
        let _regex = server
            .mock("GET", "/crates/regex")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"regex","name":"regex","max_version":"1.11.1","max_stable_version":"1.11.1","newest_version":"1.11.1"}}"#,
            )
            .create();

        let update_manager =
            CargoUpdater::new(CratesIoClient::new(server.url()).expect("crates.io client"));
        let report = update_manager
            .update(&CargoUpdateOptions {
                repo_root: temp_dir.path().to_path_buf(),
                mode: UpdateMode::Status,
            })
            .expect("cargo status");

        assert_eq!(report.manifest_files, 1);
        assert_eq!(report.dependencies_scanned, 5);
        assert_eq!(report.unmanaged_dependencies, 2);
        assert_eq!(report.entries.len(), 5);

        let anyhow = report
            .entries
            .iter()
            .find(|entry| entry.dependency_name == "anyhow")
            .expect("anyhow entry");
        assert_eq!(anyhow.current_requirement.as_deref(), Some("1.0.95"));
        assert_eq!(anyhow.latest_version.as_deref(), Some("1.0.100"));
        assert!(anyhow.managed);
        assert!(anyhow.update_needed);

        let local = report
            .entries
            .iter()
            .find(|entry| entry.dependency_name == "local-crate")
            .expect("local entry");
        assert!(!local.managed);
        assert_eq!(local.reason.as_deref(), Some("path dependency"));
    }

    #[test]
    fn apply_rewrites_supported_dependency_versions() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest = temp_dir.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1.0.95"
serde = { version = "^1.0.200", features = ["derive"] }
regex = { version = "~1.10.0" }
reqwest = { version = "=0.12.13", default-features = false }
"#,
        )
        .expect("write Cargo.toml");

        let mut server = Server::new();
        let _anyhow = server
            .mock("GET", "/crates/anyhow")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"anyhow","name":"anyhow","max_version":"1.0.100","max_stable_version":"1.0.100","newest_version":"1.0.100"}}"#,
            )
            .create();
        let _serde = server
            .mock("GET", "/crates/serde")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"serde","name":"serde","max_version":"1.0.219","max_stable_version":"1.0.219","newest_version":"1.0.219"}}"#,
            )
            .create();
        let _regex = server
            .mock("GET", "/crates/regex")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"regex","name":"regex","max_version":"1.11.1","max_stable_version":"1.11.1","newest_version":"1.11.1"}}"#,
            )
            .create();
        let _reqwest = server
            .mock("GET", "/crates/reqwest")
            .with_status(200)
            .with_body(
                r#"{"crate":{"id":"reqwest","name":"reqwest","max_version":"0.12.15","max_stable_version":"0.12.15","newest_version":"0.12.15"}}"#,
            )
            .create();

        let update_manager =
            CargoUpdater::new(CratesIoClient::new(server.url()).expect("crates.io client"));
        update_manager
            .update(&CargoUpdateOptions {
                repo_root: temp_dir.path().to_path_buf(),
                mode: UpdateMode::Apply,
            })
            .expect("cargo update");

        let manifest_contents = fs::read_to_string(&manifest).expect("read updated manifest");
        assert!(manifest_contents.contains(r#"anyhow = "1.0.100""#), "{manifest_contents}");
        assert!(manifest_contents.contains(r#"version = "^1.0.219""#), "{manifest_contents}");
        assert!(manifest_contents.contains(r#"version = "~1.11.1""#), "{manifest_contents}");
        assert!(manifest_contents.contains(r#"version = "=0.12.15""#), "{manifest_contents}");
    }
}
