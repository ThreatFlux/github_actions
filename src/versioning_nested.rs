//! Update independently locked consumers only through actual owned path edges.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item};

use super::{
    SourceFiles, VersionRewritePlan, dependency_table_paths, matches_member_pattern,
    member_manifests_from_paths, plan_lockfile_update, plan_with_sources, set_dependency_version,
    source_document, workspace_path_list,
};
use crate::{cargo, model::FileUpdate, remote};

pub struct TreeRewritePlan {
    pub plan: VersionRewritePlan,
    pub original_files: SourceFiles,
}

fn nested_lockfiles(repo_root: &Path) -> Vec<PathBuf> {
    cargo::discover_manifest_files(repo_root)
        .into_iter()
        .filter(|manifest| manifest != &repo_root.join("Cargo.toml"))
        .map(|manifest| manifest.with_file_name("Cargo.lock"))
        .filter(|lockfile| {
            std::fs::symlink_metadata(lockfile).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .collect()
}

pub fn tracked_nested_lockfiles(tracked: &BTreeSet<String>) -> impl Iterator<Item = &String> {
    tracked.iter().filter(|relative| {
        relative.as_str() != "Cargo.lock"
            && relative.ends_with("/Cargo.lock")
            && tracked
                .contains(&(relative.trim_end_matches("Cargo.lock").to_owned() + "Cargo.toml"))
    })
}

pub fn plan_read_only_rewrite(repo_root: &Path, new_version: &str) -> Result<VersionRewritePlan> {
    let repo_root = repo_root.canonicalize()?;
    let allowed = cargo::discover_manifest_files(&repo_root)
        .into_iter()
        .chain(nested_lockfiles(&repo_root))
        .chain([repo_root.join("Cargo.lock")])
        .filter_map(|file| remote::relative_repository_path(&repo_root, &file).ok())
        .collect();
    Ok(plan_version_rewrite_in_tree(&repo_root, new_version, &allowed)?.plan)
}

pub fn plan_version_rewrite_in_tree(
    repo_root: &Path,
    new_version: &str,
    tracked: &BTreeSet<String>,
) -> Result<TreeRewritePlan> {
    let repo_root = repo_root.canonicalize()?;
    let (mut plan, mut sources) = plan_with_sources(&repo_root, new_version)?;
    let owned = owned_manifests(&repo_root, tracked, &mut sources)?;
    let mut updates: BTreeMap<_, _> =
        plan.file_updates.into_iter().map(|update| (update.file, update.updated_content)).collect();
    for relative in tracked_nested_lockfiles(tracked) {
        let lockfile = repo_root.join(relative);
        let manifest = lockfile.with_file_name("Cargo.toml");
        if !is_tracked(&repo_root, &lockfile, tracked)
            || !is_tracked(&repo_root, &manifest, tracked)
        {
            bail!(
                "tracked nested release source '{relative}' is missing, non-regular or outside its analyzed path"
            );
        }
        let mut context = ConsumerContext::new(&repo_root, tracked, &owned, &sources, &updates);
        context.update_consumer(&lockfile, new_version)?;
        // Negative ownership decisions need the same exact-source evidence.
        sources = context.sources;
        updates = context.updates;
    }
    updates.retain(|file, content| sources.get(file) != Some(content));
    plan.file_updates = updates
        .into_iter()
        .map(|(file, updated_content)| FileUpdate { file, updated_content })
        .collect();
    Ok(TreeRewritePlan { plan, original_files: sources })
}

fn owned_manifests(
    repo_root: &Path,
    tracked: &BTreeSet<String>,
    sources: &mut SourceFiles,
) -> Result<BTreeMap<PathBuf, (String, String)>> {
    let root = source_document(&repo_root.join("Cargo.toml"), sources)?;
    let mut owned = BTreeMap::new();
    for manifest in tracked_members(repo_root, repo_root, &root, tracked) {
        let document = source_document(&manifest, sources)?;
        if let Some(name) = document
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|package| package.get("name"))
            .and_then(Item::as_str)
        {
            let version = owned_package_version(&document, &root)
                .context("owned package has no release version")?;
            owned.insert(manifest.canonicalize()?, (name.to_owned(), version));
        }
    }
    Ok(owned)
}

fn tracked_members(
    scope_root: &Path,
    repo_root: &Path,
    document: &DocumentMut,
    tracked: &BTreeSet<String>,
) -> Vec<PathBuf> {
    member_manifests_from_paths(
        scope_root,
        document,
        tracked
            .iter()
            .filter(|relative| {
                relative.as_str() == "Cargo.toml" || relative.ends_with("/Cargo.toml")
            })
            .map(|relative| repo_root.join(relative))
            .filter(|file| file.starts_with(scope_root)),
    )
}

fn owned_package_version(document: &DocumentMut, root: &DocumentMut) -> Option<String> {
    let version = document
        .get("package")
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))?;
    if let Some(explicit) = version.as_str() {
        return Some(explicit.to_owned());
    }
    if version.as_table_like()?.get("workspace").and_then(Item::as_bool) != Some(true) {
        return None;
    }
    root.get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("package"))
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str)
        .map(ToOwned::to_owned)
}

fn is_tracked(repo_root: &Path, file: &Path, tracked: &BTreeSet<String>) -> bool {
    std::fs::symlink_metadata(file).is_ok_and(|metadata| metadata.file_type().is_file())
        && file
            .canonicalize()
            .is_ok_and(|canonical| canonical == file && canonical.starts_with(repo_root))
        && remote::relative_repository_path(repo_root, file)
            .is_ok_and(|relative| tracked.contains(&relative))
}

#[derive(Clone)]
struct PathDependency {
    declaration: PathBuf,
    item_path: Vec<String>,
    target: PathBuf,
    optional: bool,
}

#[derive(Clone, Copy, Default)]
struct VisitScope {
    include_dev: bool,
    required: bool,
}

struct ConsumerContext<'a> {
    repo_root: &'a Path,
    tracked: &'a BTreeSet<String>,
    owned: &'a BTreeMap<PathBuf, (String, String)>,
    sources: SourceFiles,
    updates: BTreeMap<PathBuf, String>,
    reached: BTreeMap<String, String>,
    required: BTreeSet<String>,
    local_identities: BTreeMap<(String, String), BTreeSet<PathBuf>>,
    declarations: Vec<PathDependency>,
    workspace: Option<PathBuf>,
    workspace_excludes: Vec<String>,
}

impl<'a> ConsumerContext<'a> {
    fn new(
        repo_root: &'a Path,
        tracked: &'a BTreeSet<String>,
        owned: &'a BTreeMap<PathBuf, (String, String)>,
        sources: &SourceFiles,
        updates: &BTreeMap<PathBuf, String>,
    ) -> Self {
        Self {
            repo_root,
            tracked,
            owned,
            sources: sources.clone(),
            updates: updates.clone(),
            reached: BTreeMap::new(),
            required: BTreeSet::new(),
            local_identities: BTreeMap::new(),
            declarations: Vec::new(),
            workspace: None,
            workspace_excludes: Vec::new(),
        }
    }

    fn update_consumer(&mut self, lockfile: &Path, new_version: &str) -> Result<bool> {
        let manifest = lockfile.with_file_name("Cargo.toml");
        super::source_text(lockfile, &mut self.sources)?;
        let document = source_document(&manifest, &mut self.sources)?;
        self.reject_owned_patches(&manifest, &document)?;
        self.workspace = document.get("workspace").map(|_| manifest.clone());
        self.workspace_excludes = workspace_path_list(&document, "exclude");
        let mut visited = BTreeMap::new();
        for member in tracked_members(
            manifest.parent().context("consumer root")?,
            self.repo_root,
            &document,
            self.tracked,
        ) {
            if !is_tracked(self.repo_root, &member, self.tracked) {
                bail!("tracked workspace member '{}' is missing or non-regular", member.display());
            }
            self.visit(&member, VisitScope { include_dev: true, required: true }, &mut visited)?;
        }
        if self.reached.is_empty() {
            return Ok(false);
        }
        self.reject_ambiguous_owned_identities()?;
        let names = self.reached.keys().cloned().collect();
        if let Some(update) = plan_lockfile_update(
            lockfile,
            &names,
            Some(super::LockfileOwnership { versions: &self.reached, required: &self.required }),
            new_version,
            &mut self.sources,
        )? {
            self.updates.insert(update.file, update.updated_content);
        }
        self.rewrite_versions(new_version)?;
        Ok(true)
    }

    fn visit(
        &mut self,
        manifest: &Path,
        mut scope: VisitScope,
        visited: &mut BTreeMap<PathBuf, VisitScope>,
    ) -> Result<()> {
        if let Some(previous) = visited.get(manifest) {
            if (previous.include_dev || !scope.include_dev)
                && (previous.required || !scope.required)
            {
                return Ok(());
            }
            scope.include_dev |= previous.include_dev;
            scope.required |= previous.required;
        }
        visited.insert(manifest.to_path_buf(), scope);
        let document = source_document(manifest, &mut self.sources)?;
        self.record_local_identity(manifest, &document)?;
        for dependency in self.path_dependencies(manifest, &document, scope.include_dev)? {
            let member_scope = self.is_workspace_member(&dependency.target)?;
            let required =
                member_scope || (scope.required && (scope.include_dev || !dependency.optional));
            if let Some((name, version)) = self.owned.get(&dependency.target) {
                self.reached.insert(name.clone(), version.clone());
                if required {
                    self.required.insert(name.clone());
                }
                self.declarations.push(dependency.clone());
            }
            self.visit(
                &dependency.target,
                VisitScope { include_dev: member_scope, required },
                visited,
            )?;
        }
        Ok(())
    }

    fn record_local_identity(&mut self, manifest: &Path, document: &DocumentMut) -> Result<()> {
        let Some(package) = document.get("package").and_then(Item::as_table_like) else {
            return Ok(());
        };
        let Some(name) = package.get("name").and_then(Item::as_str) else {
            return Ok(());
        };
        let version = if let Some(version) = package.get("version").and_then(Item::as_str) {
            Some(version.to_owned())
        } else if let Some((_, version)) = self.owned.get(manifest) {
            Some(version.clone())
        } else {
            self.workspace_manifest(manifest)?
                .and_then(|(_, workspace)| owned_package_version(document, &workspace))
        };
        if let Some(version) = version {
            self.local_identities
                .entry((name.to_owned(), version))
                .or_default()
                .insert(manifest.to_path_buf());
        }
        Ok(())
    }

    fn reject_ambiguous_owned_identities(&self) -> Result<()> {
        for (name, version) in &self.reached {
            if self
                .local_identities
                .get(&(name.clone(), version.clone()))
                .is_some_and(|paths| paths.len() > 1)
            {
                bail!(
                    "ambiguous reachable local path identity '{name} {version}'; refusing to relabel a historical package copy"
                );
            }
        }
        Ok(())
    }

    fn is_workspace_member(&mut self, manifest: &Path) -> Result<bool> {
        let Some(workspace) = self.workspace.clone() else {
            return Ok(false);
        };
        let root = workspace.parent().context("workspace parent")?;
        let Some(directory) = manifest.parent() else {
            return Ok(false);
        };
        let Ok(relative) = directory.strip_prefix(root) else {
            return Ok(false);
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if self.workspace_excludes.iter().any(|pattern| matches_member_pattern(pattern, &relative))
        {
            return Ok(false);
        }
        Ok(self.workspace_manifest(manifest)?.is_some_and(|(owner, _)| owner == workspace))
    }

    fn path_dependencies(
        &mut self,
        manifest: &Path,
        document: &DocumentMut,
        include_dev: bool,
    ) -> Result<Vec<PathDependency>> {
        let mut dependencies = Vec::new();
        for table_path in dependency_table_paths(document) {
            if table_path.first().is_some_and(|part| part == "workspace")
                || (!include_dev
                    && table_path.last().is_some_and(|part| part == "dev-dependencies"))
            {
                continue;
            }
            let table = table_path
                .iter()
                .try_fold(document.as_item(), |item, key| item.as_table_like()?.get(key))
                .and_then(Item::as_table_like);
            let Some(table) = table else {
                continue;
            };
            for (alias, item) in table.iter() {
                let mut item_path = table_path.clone();
                item_path.push(alias.to_owned());
                if let Some(dependency) = self.resolve_dependency(manifest, item_path, item)? {
                    dependencies.push(dependency);
                }
            }
        }
        Ok(dependencies)
    }

    fn resolve_dependency(
        &mut self,
        manifest: &Path,
        mut item_path: Vec<String>,
        item: &Item,
    ) -> Result<Option<PathDependency>> {
        let Some(table) = item.as_table_like() else {
            return Ok(None);
        };
        let optional = table.get("optional").and_then(Item::as_bool).unwrap_or(false);
        let mut declaration = manifest.to_path_buf();
        let path = if table.get("workspace").and_then(Item::as_bool) == Some(true) {
            let Some((workspace, document)) = self.workspace_manifest(manifest)? else {
                return Ok(None);
            };
            declaration = workspace;
            item_path = vec![
                "workspace".to_owned(),
                "dependencies".to_owned(),
                item_path.pop().expect("dependency alias"),
            ];
            item_at(&document, &item_path)
                .and_then(Item::as_table_like)
                .and_then(|spec| spec.get("path"))
                .and_then(Item::as_str)
                .map(ToOwned::to_owned)
        } else {
            table.get("path").and_then(Item::as_str).map(ToOwned::to_owned)
        };
        let Some(path) = path else {
            return Ok(None);
        };
        let Some(target) = self.dependency_target(&declaration, &path)? else {
            return Ok(None);
        };
        Ok(Some(PathDependency { declaration, item_path, target, optional }))
    }

    fn dependency_target(&self, declaration: &Path, path: &str) -> Result<Option<PathBuf>> {
        let target = declaration.parent().context("manifest parent")?.join(path).join("Cargo.toml");
        let mut normalized = PathBuf::new();
        for component in target.components() {
            if component == std::path::Component::ParentDir {
                normalized.pop();
            } else {
                normalized.push(component.as_os_str());
            }
        }
        let relative = remote::relative_repository_path(self.repo_root, &normalized).ok();
        if !relative.is_some_and(|relative| self.tracked.contains(&relative)) {
            return Ok(None);
        }
        if !is_tracked(self.repo_root, &normalized, self.tracked) {
            bail!(
                "tracked path dependency '{}' is missing, non-regular or redirected",
                normalized.display()
            );
        }
        Ok(Some(normalized))
    }

    fn workspace_manifest(&mut self, manifest: &Path) -> Result<Option<(PathBuf, DocumentMut)>> {
        let member = source_document(manifest, &mut self.sources)?;
        if let Some(explicit) = member
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|package| package.get("workspace"))
            .and_then(Item::as_str)
        {
            let Some(candidate) = self.dependency_target(manifest, explicit)? else {
                return Ok(None);
            };
            let document = source_document(&candidate, &mut self.sources)?;
            if document.get("workspace").is_some() {
                return Ok(Some((candidate, document)));
            }
            return Ok(None);
        }
        for directory in manifest.parent().into_iter().flat_map(Path::ancestors) {
            if !directory.starts_with(self.repo_root) {
                break;
            }
            let candidate = directory.join("Cargo.toml");
            if !is_tracked(self.repo_root, &candidate, self.tracked) {
                continue;
            }
            let document = source_document(&candidate, &mut self.sources)?;
            if document.get("workspace").is_some() {
                return Ok(Some((candidate, document)));
            }
        }
        Ok(None)
    }

    fn reject_owned_patches(&self, manifest: &Path, document: &DocumentMut) -> Result<()> {
        for key in ["patch", "replace"] {
            let Some(table) = document.get(key).and_then(Item::as_table_like) else {
                continue;
            };
            for (_, item) in table.iter() {
                let Some(specs) = item.as_table_like() else {
                    continue;
                };
                for spec in std::iter::once(item).chain(specs.iter().map(|(_, spec)| spec)) {
                    let Some(path) = spec
                        .as_table_like()
                        .and_then(|entry| entry.get("path"))
                        .and_then(Item::as_str)
                    else {
                        continue;
                    };
                    let target =
                        manifest.parent().context("manifest parent")?.join(path).join("Cargo.toml");
                    if target.canonicalize().is_ok_and(|target| self.owned.contains_key(&target)) {
                        bail!(
                            "owned path package in nested [{key}] at '{}' requires an explicit release update",
                            manifest.display()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn rewrite_item(&mut self, dependency: &PathDependency, new_version: &str) -> Result<()> {
        let original = super::source_text(&dependency.declaration, &mut self.sources)?;
        self.updates.entry(dependency.declaration.clone()).or_insert(original);
        // Apply only to a verified owned path declaration, retaining all other
        // nested package versions and dependency specifications.
        let mut document = self.updates[&dependency.declaration].parse::<DocumentMut>()?;
        if let Some(item) = cargo::get_item_mut(document.as_item_mut(), &dependency.item_path) {
            set_dependency_version(item, new_version);
        }
        self.updates.insert(dependency.declaration.clone(), document.to_string());
        Ok(())
    }

    fn rewrite_versions(&mut self, new_version: &str) -> Result<()> {
        for dependency in std::mem::take(&mut self.declarations) {
            self.rewrite_item(&dependency, new_version)?;
        }
        Ok(())
    }
}

fn item_at<'a>(document: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    path.iter().try_fold(document.as_item(), |item, key| item.as_table_like()?.get(key))
}

#[cfg(test)]
#[path = "versioning_nested_tests.rs"]
mod tests;
