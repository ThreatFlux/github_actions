//! Read-only source identity checks for independently locked release consumers.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::GitHubClient;
use crate::{remote, versioning::SourceFiles};

#[derive(Deserialize)]
struct SourceTree {
    tree: Vec<SourceEntry>,
    truncated: bool,
}

#[derive(Deserialize)]
struct SourceEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

impl GitHubClient {
    pub(crate) fn regular_files_at_commit(
        &self,
        owner: &str,
        repository: &str,
        commit_sha: &str,
    ) -> Result<BTreeMap<String, String>> {
        let response = self.get_with_retry(
            &format!("/repos/{owner}/{repository}/git/trees/{commit_sha}?recursive=1"),
            || format!("read tracked release source at {commit_sha}"),
        )?;
        let source = response.json::<SourceTree>().context("decode tracked release source tree")?;
        ensure!(
            !source.truncated,
            "tracked release source tree is truncated; refusing partial ownership evidence"
        );
        Ok(source
            .tree
            .into_iter()
            .filter(|entry| {
                entry.kind == "blob" && matches!(entry.mode.as_str(), "100644" | "100755")
            })
            .map(|entry| (entry.path, entry.sha))
            .collect())
    }

    pub(crate) fn validate_release_sources(
        &self,
        owner: &str,
        repository: &str,
        repo_root: &Path,
        tracked: &BTreeMap<String, String>,
        originals: &SourceFiles,
    ) -> Result<()> {
        for (file, original) in originals {
            let relative = remote::relative_repository_path(repo_root, file)?;
            let sha = tracked.get(&relative).with_context(|| {
                format!("release source '{relative}' is not a tracked regular file")
            })?;
            let description = || format!("read exact tracked release blob for '{relative}'");
            let path = format!("/repos/{owner}/{repository}/git/blobs/{sha}");
            let response = self.send_raw_with_retry(
                || self.get(&path).header("Accept", "application/vnd.github.raw+json"),
                &description,
            )?;
            if !response.status().is_success() {
                return self.error_from_response(response, &description()).map(|_| ());
            }
            let contents = response.bytes().with_context(description)?;
            ensure!(
                contents.as_ref() == original.as_bytes(),
                "release source '{relative}' differs from the analyzed commit; refusing ambient changes"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "github_source_tests.rs"]
mod tests;
