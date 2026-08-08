use std::{thread, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{
    StatusCode,
    blocking::{Client, RequestBuilder, Response},
    header::{AUTHORIZATION, HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct GitHubClient {
    base_url: String,
    token: Option<String>,
    client: Client,
    max_retries: u32,
    retry_delay: Duration,
    max_retry_delay: Duration,
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LatestReference {
    pub version: String,
    pub sha: String,
}

#[derive(Debug, Deserialize)]
struct LatestReleaseResponse {
    tag_name: String,
}

#[derive(Debug, Deserialize)]
struct TagResponse {
    name: String,
    commit: Option<CommitResponse>,
}

#[derive(Debug, Clone)]
pub struct GitHubClientOptions {
    pub base_url: String,
    pub token: Option<String>,
    pub timeout: Duration,
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub max_retry_delay: Duration,
}

impl Default for GitHubClientOptions {
    fn default() -> Self {
        Self {
            base_url: String::from("https://api.github.com"),
            token: None,
            timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_mins(1),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TreeEntry {
    pub path: String,
    pub sha: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PullRequestInfo {
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TagInfo {
    pub name: String,
    pub sha: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub is_merge: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CommitRange {
    pub commits: Vec<CommitInfo>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseInfo {
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct RepositoryResponse {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct ReferenceResponse {
    object: ReferenceObject,
}

#[derive(Debug, Deserialize)]
struct ReferenceObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CommitTreeResponse {
    tree: CommitTreeObject,
}

#[derive(Debug, Deserialize)]
struct CommitTreeObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct BlobResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CreatedCommitResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    number: u64,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct UserResponse {
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompareResponse {
    total_commits: u64,
    commits: Vec<RepoCommitResponse>,
}

#[derive(Debug, Deserialize)]
struct RepoCommitResponse {
    sha: String,
    commit: RepoCommitDetail,
    parents: Vec<CommitParent>,
}

#[derive(Debug, Deserialize)]
struct RepoCommitDetail {
    message: String,
}

#[derive(Debug, Deserialize)]
struct CommitParent {}

#[derive(Debug, Deserialize)]
struct CreatedReleaseResponse {
    html_url: String,
}

#[derive(Debug, Serialize)]
struct CreateReferenceRequest<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    sha: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateBlobRequest<'a> {
    content: &'a str,
    encoding: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateTreeRequest<'a> {
    base_tree: &'a str,
    tree: Vec<CreateTreeEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct CreateTreeEntry<'a> {
    path: &'a str,
    mode: &'a str,
    #[serde(rename = "type")]
    object_type: &'a str,
    sha: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateCommitRequest<'a> {
    message: &'a str,
    tree: &'a str,
    parents: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct UpdateReferenceRequest<'a> {
    sha: &'a str,
    force: bool,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestRequest<'a> {
    title: &'a str,
    body: &'a str,
    head: &'a str,
    base: &'a str,
}

#[derive(Debug, Serialize)]
struct UpdatePullRequestRequest<'a> {
    title: &'a str,
    body: &'a str,
}

#[derive(Debug, Serialize)]
struct AddLabelsRequest<'a> {
    labels: &'a [String],
}

#[derive(Debug, Serialize)]
struct CreateReleaseRequest<'a> {
    tag_name: &'a str,
    target_commitish: &'a str,
    name: &'a str,
    body: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateTagObjectRequest<'a> {
    tag: &'a str,
    message: &'a str,
    object: &'a str,
    #[serde(rename = "type")]
    object_type: &'a str,
}

#[derive(Debug, Deserialize)]
struct CreatedTagObjectResponse {
    sha: String,
}

impl GitHubClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Result<Self> {
        Self::with_options(GitHubClientOptions {
            base_url: base_url.into(),
            token,
            ..GitHubClientOptions::default()
        })
    }

    pub fn with_options(options: GitHubClientOptions) -> Result<Self> {
        let GitHubClientOptions {
            base_url,
            token,
            timeout,
            max_retries,
            retry_delay,
            max_retry_delay,
        } = options;
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("github-actions-maintainer"));

        let client = Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.as_deref().and_then(normalize_token),
            client,
            max_retries,
            retry_delay,
            max_retry_delay,
        })
    }

    pub fn latest_reference(&self, owner: &str, repository: &str) -> Result<LatestReference> {
        if let Some(version) = self.latest_release_tag(owner, repository)? {
            let sha = self.resolve_reference(owner, repository, &version)?;
            return Ok(LatestReference { version, sha });
        }

        let tags = self
            .get_with_retry(&format!("/repos/{owner}/{repository}/tags?per_page=1"), || {
                format!("fetch tags for {owner}/{repository}")
            })?;
        let mut tags = tags
            .json::<Vec<TagResponse>>()
            .with_context(|| format!("failed to decode tags response for {owner}/{repository}"))?;

        let tag = tags.pop().ok_or_else(|| {
            anyhow::anyhow!("GitHub did not return any tags for {owner}/{repository}")
        })?;
        let sha = if let Some(commit) = tag.commit {
            commit.sha
        } else {
            self.resolve_reference(owner, repository, &tag.name)?
        };

        Ok(LatestReference { version: tag.name, sha })
    }

    pub fn resolve_reference(
        &self,
        owner: &str,
        repository: &str,
        reference: &str,
    ) -> Result<String> {
        let encoded_reference = urlencoding::encode(reference);
        let response = self.get_with_retry(
            &format!("/repos/{owner}/{repository}/commits/{encoded_reference}"),
            || format!("resolve {owner}/{repository}@{reference}"),
        )?;
        let commit = response.json::<CommitResponse>().with_context(|| {
            format!("failed to decode commit response for {owner}/{repository}@{reference}")
        })?;

        Ok(commit.sha)
    }

    fn latest_release_tag(&self, owner: &str, repository: &str) -> Result<Option<String>> {
        let response = self.get_with_retry_allowing_not_found(
            &format!("/repos/{owner}/{repository}/releases/latest"),
            || format!("fetch latest release for {owner}/{repository}"),
        )?;
        let Some(response) = response else {
            return Ok(None);
        };
        let release = response.json::<LatestReleaseResponse>().with_context(|| {
            format!("failed to decode release response for {owner}/{repository}")
        })?;

        Ok(Some(release.tag_name))
    }

    pub fn validate_token_scopes(&self) -> Result<()> {
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| anyhow!("a GitHub token is required for remote PR creation"))?;

        let response = self.send_with_retry(
            || self.get("/user").header(AUTHORIZATION, format!("Bearer {token}")),
            || String::from("validate GitHub token scopes"),
        )?;
        let headers = response.headers().clone();
        let user =
            response.json::<UserResponse>().context("failed to decode GitHub user response")?;

        if user.login.is_none() {
            bail!("failed to validate GitHub token: authenticated user is missing");
        }

        let Some(scopes) = headers.get("x-oauth-scopes").and_then(|value| value.to_str().ok())
        else {
            return Ok(());
        };

        let has_repo_scope = scopes.contains("repo") || scopes.contains("public_repo");
        if !has_repo_scope {
            bail!("GitHub token is missing the repo or public_repo scope");
        }
        if !scopes.contains("workflow") {
            bail!("GitHub token is missing the workflow scope");
        }

        Ok(())
    }

    pub fn default_branch(&self, owner: &str, repository: &str) -> Result<String> {
        let response = self.get_with_retry(&format!("/repos/{owner}/{repository}"), || {
            format!("fetch repository metadata for {owner}/{repository}")
        })?;
        let repository = response.json::<RepositoryResponse>().with_context(|| {
            format!("failed to decode repository response for {owner}/{repository}")
        })?;
        Ok(repository.default_branch)
    }

    pub fn branch_head_sha(&self, owner: &str, repository: &str, branch: &str) -> Result<String> {
        let response = self.get_with_retry(
            &format!("/repos/{owner}/{repository}/git/ref/heads/{branch}"),
            || format!("fetch branch ref for {owner}/{repository}:{branch}"),
        )?;
        let reference = response.json::<ReferenceResponse>().with_context(|| {
            format!("failed to decode branch ref for {owner}/{repository}:{branch}")
        })?;
        Ok(reference.object.sha)
    }

    pub fn commit_tree_sha(
        &self,
        owner: &str,
        repository: &str,
        commit_sha: &str,
    ) -> Result<String> {
        let response = self.get_with_retry(
            &format!("/repos/{owner}/{repository}/git/commits/{commit_sha}"),
            || format!("fetch commit tree for {owner}/{repository}@{commit_sha}"),
        )?;
        let commit = response.json::<CommitTreeResponse>().with_context(|| {
            format!("failed to decode commit tree for {owner}/{repository}@{commit_sha}")
        })?;
        Ok(commit.tree.sha)
    }

    pub fn create_branch(
        &self,
        owner: &str,
        repository: &str,
        branch: &str,
        base_sha: &str,
    ) -> Result<()> {
        self.create_ref(owner, repository, &format!("heads/{branch}"), base_sha)
    }

    pub fn create_ref(
        &self,
        owner: &str,
        repository: &str,
        ref_path: &str,
        sha: &str,
    ) -> Result<()> {
        let reference = format!("refs/{ref_path}");
        let payload = CreateReferenceRequest { reference: &reference, sha };
        self.post_json(&format!("/repos/{owner}/{repository}/git/refs"), &payload, || {
            format!("create ref {ref_path} for {owner}/{repository}")
        })?;
        Ok(())
    }

    /// Create an annotated tag object pointing at `commit_sha` and return the
    /// tag object's SHA. The tagger identity derives from the token. A ref
    /// must still be created separately to make the tag reachable.
    pub fn create_annotated_tag(
        &self,
        owner: &str,
        repository: &str,
        tag: &str,
        message: &str,
        commit_sha: &str,
    ) -> Result<String> {
        let payload =
            CreateTagObjectRequest { tag, message, object: commit_sha, object_type: "commit" };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/git/tags"), &payload, || {
                format!("create annotated tag {tag} for {owner}/{repository}")
            })?;
        let created = response.json::<CreatedTagObjectResponse>().with_context(|| {
            format!("failed to decode tag object response for {owner}/{repository}")
        })?;
        Ok(created.sha)
    }

    pub fn reference_sha(
        &self,
        owner: &str,
        repository: &str,
        ref_path: &str,
    ) -> Result<Option<String>> {
        let response = self.get_with_retry_allowing_not_found(
            &format!("/repos/{owner}/{repository}/git/ref/{ref_path}"),
            || format!("fetch ref {ref_path} for {owner}/{repository}"),
        )?;
        let Some(response) = response else {
            return Ok(None);
        };
        let reference = response
            .json::<ReferenceResponse>()
            .with_context(|| format!("failed to decode ref {ref_path} for {owner}/{repository}"))?;
        Ok(Some(reference.object.sha))
    }

    pub fn create_blob(&self, owner: &str, repository: &str, content: &str) -> Result<String> {
        let payload = CreateBlobRequest { content, encoding: "utf-8" };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/git/blobs"), &payload, || {
                format!("create blob for {owner}/{repository}")
            })?;
        let blob = response
            .json::<BlobResponse>()
            .with_context(|| format!("failed to decode blob response for {owner}/{repository}"))?;
        Ok(blob.sha)
    }

    pub fn create_tree(
        &self,
        owner: &str,
        repository: &str,
        base_tree_sha: &str,
        entries: &[TreeEntry],
    ) -> Result<String> {
        let payload = CreateTreeRequest {
            base_tree: base_tree_sha,
            tree: entries
                .iter()
                .map(|entry| CreateTreeEntry {
                    path: &entry.path,
                    mode: "100644",
                    object_type: "blob",
                    sha: &entry.sha,
                })
                .collect(),
        };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/git/trees"), &payload, || {
                format!("create tree for {owner}/{repository}")
            })?;
        let tree = response
            .json::<TreeResponse>()
            .with_context(|| format!("failed to decode tree response for {owner}/{repository}"))?;
        Ok(tree.sha)
    }

    pub fn create_commit(
        &self,
        owner: &str,
        repository: &str,
        message: &str,
        tree_sha: &str,
        parent_sha: &str,
    ) -> Result<String> {
        let payload = CreateCommitRequest { message, tree: tree_sha, parents: vec![parent_sha] };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/git/commits"), &payload, || {
                format!("create commit for {owner}/{repository}")
            })?;
        let commit = response.json::<CreatedCommitResponse>().with_context(|| {
            format!("failed to decode commit response for {owner}/{repository}")
        })?;
        Ok(commit.sha)
    }

    pub fn update_branch(
        &self,
        owner: &str,
        repository: &str,
        branch: &str,
        commit_sha: &str,
    ) -> Result<()> {
        self.update_ref(owner, repository, &format!("heads/{branch}"), commit_sha, false)
    }

    pub fn update_ref(
        &self,
        owner: &str,
        repository: &str,
        ref_path: &str,
        sha: &str,
        force: bool,
    ) -> Result<()> {
        let payload = UpdateReferenceRequest { sha, force };
        self.patch_json(
            &format!("/repos/{owner}/{repository}/git/refs/{ref_path}"),
            &payload,
            || format!("update ref {ref_path} for {owner}/{repository}"),
        )?;
        Ok(())
    }

    /// Fast-forward `ref_path` to `sha`; returns `Ok(false)` when GitHub rejects
    /// the update because it is not a fast forward (HTTP 422).
    pub fn update_ref_fast_forward(
        &self,
        owner: &str,
        repository: &str,
        ref_path: &str,
        sha: &str,
    ) -> Result<bool> {
        let payload = UpdateReferenceRequest { sha, force: false };
        let response = self.send_with_retry_allowing_non_fast_forward(
            || {
                self.client
                    .patch(format!(
                        "{}/repos/{owner}/{repository}/git/refs/{ref_path}",
                        self.base_url
                    ))
                    .with_auth(self)
                    .json(&payload)
            },
            || format!("fast-forward ref {ref_path} for {owner}/{repository}"),
        )?;
        Ok(response.is_some())
    }

    pub fn list_tags(&self, owner: &str, repository: &str, max_pages: u32) -> Result<Vec<TagInfo>> {
        let mut tags = Vec::new();
        for page in 1..=max_pages {
            let response = self.get_with_retry(
                &format!("/repos/{owner}/{repository}/tags?per_page=100&page={page}"),
                || format!("list tags for {owner}/{repository}"),
            )?;
            let page_tags = response.json::<Vec<TagResponse>>().with_context(|| {
                format!("failed to decode tags response for {owner}/{repository}")
            })?;
            let page_len = page_tags.len();
            tags.extend(
                page_tags.into_iter().map(|tag| TagInfo {
                    name: tag.name,
                    sha: tag.commit.map(|commit| commit.sha),
                }),
            );
            if page_len < 100 {
                break;
            }
        }
        Ok(tags)
    }

    /// Latest tag whose name is `prefix` followed by a semver version,
    /// scanning up to 1000 tags.
    pub fn latest_semver_tag(
        &self,
        owner: &str,
        repository: &str,
        prefix: &str,
    ) -> Result<Option<TagInfo>> {
        const MAX_TAG_PAGES: u32 = 10;
        let tags = self.list_tags(owner, repository, MAX_TAG_PAGES)?;
        Ok(tags
            .into_iter()
            .filter_map(|tag| {
                let version = tag.name.strip_prefix(prefix)?;
                let parsed = semver::Version::parse(version).ok()?;
                Some((parsed, tag))
            })
            .max_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, tag)| tag))
    }

    pub fn compare_commits(
        &self,
        owner: &str,
        repository: &str,
        base: &str,
        head: &str,
        max_pages: u32,
    ) -> Result<CommitRange> {
        let encoded_base = urlencoding::encode(base);
        let encoded_head = urlencoding::encode(head);
        let mut commits = Vec::new();
        let mut total_commits = 0usize;
        for page in 1..=max_pages {
            let response = self.get_with_retry(
                &format!(
                    "/repos/{owner}/{repository}/compare/{encoded_base}...{encoded_head}?per_page=100&page={page}"
                ),
                || format!("compare {base}...{head} for {owner}/{repository}"),
            )?;
            let compare = response.json::<CompareResponse>().with_context(|| {
                format!("failed to decode compare response for {owner}/{repository}")
            })?;
            total_commits = usize::try_from(compare.total_commits).unwrap_or(usize::MAX);
            // The compare endpoint caps the commit list; once pages come back
            // empty, further requests cannot make progress.
            if compare.commits.is_empty() {
                break;
            }
            commits.extend(compare.commits.into_iter().map(commit_info_from_response));
            if commits.len() >= total_commits {
                break;
            }
        }
        let truncated = commits.len() < total_commits;
        Ok(CommitRange { commits, truncated })
    }

    pub fn list_commits(
        &self,
        owner: &str,
        repository: &str,
        head_sha: &str,
        max_pages: u32,
    ) -> Result<CommitRange> {
        let encoded_head = urlencoding::encode(head_sha);
        let mut commits = Vec::new();
        let mut last_page_full = false;
        for page in 1..=max_pages {
            let response = self.get_with_retry(
                &format!(
                    "/repos/{owner}/{repository}/commits?sha={encoded_head}&per_page=100&page={page}"
                ),
                || format!("list commits for {owner}/{repository}"),
            )?;
            let page_commits = response.json::<Vec<RepoCommitResponse>>().with_context(|| {
                format!("failed to decode commits response for {owner}/{repository}")
            })?;
            last_page_full = page_commits.len() == 100;
            commits.extend(page_commits.into_iter().map(commit_info_from_response));
            if !last_page_full {
                break;
            }
        }
        Ok(CommitRange { commits, truncated: last_page_full })
    }

    pub fn create_release(
        &self,
        owner: &str,
        repository: &str,
        tag_name: &str,
        name: &str,
        body: &str,
        target_commitish: &str,
    ) -> Result<ReleaseInfo> {
        let payload = CreateReleaseRequest { tag_name, target_commitish, name, body };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/releases"), &payload, || {
                format!("create release {tag_name} for {owner}/{repository}")
            })?;
        let release = response.json::<CreatedReleaseResponse>().with_context(|| {
            format!("failed to decode release response for {owner}/{repository}")
        })?;
        Ok(ReleaseInfo { url: release.html_url })
    }

    pub fn ensure_token(&self) -> Result<()> {
        if self.token.is_none() {
            bail!("a GitHub token is required to create releases; provide --token or GITHUB_TOKEN");
        }
        Ok(())
    }

    pub fn create_pull_request(
        &self,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PullRequestInfo> {
        let payload = CreatePullRequestRequest { title, body, head, base };
        let response =
            self.post_json(&format!("/repos/{owner}/{repository}/pulls"), &payload, || {
                format!("create pull request for {owner}/{repository}")
            })?;
        let pull_request = response.json::<PullRequestResponse>().with_context(|| {
            format!("failed to decode pull request response for {owner}/{repository}")
        })?;
        Ok(PullRequestInfo { number: pull_request.number, url: pull_request.html_url })
    }

    pub fn find_open_pull_request(
        &self,
        owner: &str,
        repository: &str,
        head: &str,
        base: &str,
    ) -> Result<Option<PullRequestInfo>> {
        let encoded_head = urlencoding::encode(&format!("{owner}:{head}")).into_owned();
        let encoded_base = urlencoding::encode(base).into_owned();
        let response = self.send_with_retry(
            || self.get(&format!(
                "/repos/{owner}/{repository}/pulls?state=open&head={encoded_head}&base={encoded_base}&per_page=100"
            )),
            || format!("find open release pull request for {owner}/{repository}"),
        )?;
        let pull_requests = response.json::<Vec<PullRequestResponse>>().with_context(|| {
            format!("failed to decode pull request list for {owner}/{repository}")
        })?;
        Ok(pull_requests.into_iter().next().map(|pull_request| PullRequestInfo {
            number: pull_request.number,
            url: pull_request.html_url,
        }))
    }

    pub fn update_pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<PullRequestInfo> {
        let payload = UpdatePullRequestRequest { title, body };
        let response = self.patch_json(
            &format!("/repos/{owner}/{repository}/pulls/{number}"),
            &payload,
            || format!("update pull request {number} for {owner}/{repository}"),
        )?;
        let pull_request = response.json::<PullRequestResponse>().with_context(|| {
            format!("failed to decode updated pull request response for {owner}/{repository}")
        })?;
        Ok(PullRequestInfo { number: pull_request.number, url: pull_request.html_url })
    }

    pub fn add_labels(
        &self,
        owner: &str,
        repository: &str,
        issue_number: u64,
        labels: &[String],
    ) -> Result<()> {
        let payload = AddLabelsRequest { labels };
        self.post_json(
            &format!("/repos/{owner}/{repository}/issues/{issue_number}/labels"),
            &payload,
            || format!("add labels to issue {issue_number} for {owner}/{repository}"),
        )?;
        Ok(())
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.get_anonymous(path).with_auth(self)
    }

    fn get_anonymous(&self, path: &str) -> RequestBuilder {
        self.client.get(format!("{}{}", self.base_url, path))
    }

    fn post_json<T: Serialize, F>(&self, path: &str, payload: &T, describe: F) -> Result<Response>
    where
        F: Fn() -> String,
    {
        self.send_with_retry(
            || self.client.post(format!("{}{}", self.base_url, path)).with_auth(self).json(payload),
            describe,
        )
    }

    fn patch_json<T: Serialize, F>(&self, path: &str, payload: &T, describe: F) -> Result<Response>
    where
        F: Fn() -> String,
    {
        self.send_with_retry(
            || {
                self.client
                    .patch(format!("{}{}", self.base_url, path))
                    .with_auth(self)
                    .json(payload)
            },
            describe,
        )
    }

    /// Send with retry/backoff and return the first non-retryable response,
    /// whatever its status. Status-specific handling lives in the wrappers.
    fn send_raw_with_retry<F, D>(&self, mut build_request: F, describe: &D) -> Result<Response>
    where
        F: FnMut() -> RequestBuilder,
        D: Fn() -> String,
    {
        let mut attempt = 0u32;

        loop {
            match build_request().send() {
                Ok(response) => {
                    if Self::should_retry_response(&response) && attempt < self.max_retries {
                        self.sleep_for_retry(response.headers(), attempt);
                        attempt += 1;
                        continue;
                    }
                    return Ok(response);
                }
                Err(error) => {
                    if (error.is_timeout() || error.is_connect()) && attempt < self.max_retries {
                        thread::sleep(self.calculate_backoff(attempt));
                        attempt += 1;
                        continue;
                    }
                    return Err(error).with_context(describe);
                }
            }
        }
    }

    fn send_with_retry<F, D>(&self, build_request: F, describe: D) -> Result<Response>
    where
        F: FnMut() -> RequestBuilder,
        D: Fn() -> String,
    {
        let response = self.send_raw_with_retry(build_request, &describe)?;
        if response.status().is_success() {
            return Ok(response);
        }
        self.error_from_response(response, &describe())
    }

    /// GET public metadata, returning the response whatever its status.
    ///
    /// GitHub rejects *authenticated* requests with 403 when the owning
    /// organization enables an IP allow list that does not cover the caller,
    /// even though the same data is readable without a token. Retry such a
    /// request once with no `Authorization` header and use the anonymous
    /// response when it succeeds; otherwise report the original 403.
    ///
    /// Only 403 falls back: 401 means the token itself is bad and must stay
    /// loud, and 404 is left to the caller. Rate-limit 403s keep the existing
    /// backoff-then-report path instead of burning an anonymous request that
    /// would face a lower limit.
    fn send_read_with_retry<D>(&self, path: &str, describe: &D) -> Result<Response>
    where
        D: Fn() -> String,
    {
        let response = self.send_raw_with_retry(|| self.get(path), describe)?;
        if self.token.is_none()
            || response.status() != StatusCode::FORBIDDEN
            || Self::should_retry_response(&response)
        {
            return Ok(response);
        }

        let body = response.text().unwrap_or_else(|_| String::from("<response body unavailable>"));
        if body.to_ascii_lowercase().contains("rate limit") {
            bail!("{}: GitHub API returned {} ({body})", describe(), StatusCode::FORBIDDEN);
        }

        let anonymous_outcome =
            match self.send_raw_with_retry(|| self.get_anonymous(path), describe) {
                Ok(anonymous) if anonymous.status().is_success() => return Ok(anonymous),
                Ok(anonymous) => format!("returned {}", anonymous.status()),
                Err(error) => format!("failed: {error}"),
            };
        bail!(
            "{}: GitHub API returned {} ({body}); the anonymous retry without the token also {anonymous_outcome}",
            describe(),
            StatusCode::FORBIDDEN,
        )
    }

    fn get_with_retry<D>(&self, path: &str, describe: D) -> Result<Response>
    where
        D: Fn() -> String,
    {
        let response = self.send_read_with_retry(path, &describe)?;
        if response.status().is_success() {
            return Ok(response);
        }
        self.error_from_response(response, &describe())
    }

    fn get_with_retry_allowing_not_found<D>(
        &self,
        path: &str,
        describe: D,
    ) -> Result<Option<Response>>
    where
        D: Fn() -> String,
    {
        let response = self.send_read_with_retry(path, &describe)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status().is_success() {
            return Ok(Some(response));
        }
        self.error_from_response(response, &describe()).map(Some)
    }

    /// Like `send_with_retry`, but a 422 whose body reports a non-fast-forward
    /// ref update returns `Ok(None)`. Any other 422 is still an error so
    /// validation failures (bad SHA, invalid ref name) surface loudly.
    fn send_with_retry_allowing_non_fast_forward<D>(
        &self,
        build_request: impl FnMut() -> RequestBuilder,
        describe: D,
    ) -> Result<Option<Response>>
    where
        D: Fn() -> String,
    {
        let response = self.send_raw_with_retry(build_request, &describe)?;
        if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
            let body =
                response.text().unwrap_or_else(|_| String::from("<response body unavailable>"));
            if body.to_ascii_lowercase().contains("fast forward") {
                return Ok(None);
            }
            bail!("{}: GitHub API returned 422 Unprocessable Entity ({body})", describe());
        }
        if response.status().is_success() {
            return Ok(Some(response));
        }
        self.error_from_response(response, &describe()).map(Some)
    }

    fn should_retry_response(response: &Response) -> bool {
        if response.status() == StatusCode::TOO_MANY_REQUESTS || response.status().is_server_error()
        {
            return true;
        }

        response.status() == StatusCode::FORBIDDEN
            && (response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok())
                == Some("0")
                || response.headers().contains_key(RETRY_AFTER))
    }

    fn sleep_for_retry(&self, headers: &HeaderMap, attempt: u32) {
        let delay = retry_delay_from_headers(headers)
            .filter(|delay| *delay > Duration::ZERO && *delay <= self.max_retry_delay * 10)
            .unwrap_or_else(|| self.calculate_backoff(attempt));
        thread::sleep(delay);
    }

    fn calculate_backoff(&self, attempt: u32) -> Duration {
        let shift = attempt.min(10);
        let candidate = self.retry_delay.saturating_mul(1u32 << shift);
        candidate.min(self.max_retry_delay)
    }

    fn error_from_response(&self, response: Response, context: &str) -> Result<Response> {
        let status = response.status();
        let body = response.text().unwrap_or_else(|_| String::from("<response body unavailable>"));

        if status == StatusCode::FORBIDDEN
            && body.to_ascii_lowercase().contains("rate limit")
            && self.token.is_none()
        {
            bail!(
                "{context}: GitHub API rate limit exceeded. Provide --token or GITHUB_TOKEN for higher limits."
            )
        }
        if status == StatusCode::NOT_FOUND {
            bail!("{context}: resource not found ({body})");
        }

        bail!("{context}: GitHub API returned {status} ({body})")
    }
}

fn commit_info_from_response(commit: RepoCommitResponse) -> CommitInfo {
    CommitInfo {
        sha: commit.sha,
        message: commit.commit.message,
        is_merge: commit.parents.len() > 1,
    }
}

fn normalize_token(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
}

fn retry_delay_from_headers(headers: &HeaderMap) -> Option<Duration> {
    if let Some(retry_after) = headers.get(RETRY_AFTER).and_then(|value| value.to_str().ok())
        && let Ok(seconds) = retry_after.parse::<u64>()
    {
        return Some(Duration::from_secs(seconds));
    }

    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    if remaining == Some(0)
        && let Some(reset) = reset
    {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        if reset > now {
            return Some(Duration::from_secs(reset - now) + Duration::from_millis(100));
        }
    }

    None
}

trait RequestBuilderAuthExt {
    fn with_auth(self, client: &GitHubClient) -> Self;
}

impl RequestBuilderAuthExt for RequestBuilder {
    fn with_auth(self, client: &GitHubClient) -> Self {
        if let Some(token) = client.token.as_deref() {
            self.header(AUTHORIZATION, format!("Bearer {token}"))
        } else {
            self
        }
    }
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use mockito::{Matcher, Server};
    use std::time::Duration;

    use super::{GitHubClient, GitHubClientOptions};

    #[test]
    fn resolve_reference_returns_commit_sha() {
        let mut server = Server::new();
        let _mock = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .match_header("user-agent", "github-actions-maintainer")
            .with_status(200)
            .with_body(r#"{"sha":"de0fac2e4500dabe0009e67214ff5f5447ce83dd"}"#)
            .create();

        let client = GitHubClient::new(server.url(), None).expect("github client");
        let sha = client.resolve_reference("actions", "checkout", "v4").expect("resolve reference");

        assert_eq!(sha, "de0fac2e4500dabe0009e67214ff5f5447ce83dd");
    }

    #[test]
    fn resolve_reference_sends_authorization_when_token_is_present() {
        let mut server = Server::new();
        let _mock = server
            .mock("GET", "/repos/actions/cache/commits/v4")
            .match_header("authorization", Matcher::Regex(r"^Bearer\s+ghp_testtoken$".into()))
            .with_status(200)
            .with_body(r#"{"sha":"668228422ae6a00e4ad889ee87cd7109ec5666a7"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let sha = client.resolve_reference("actions", "cache", "v4").expect("resolve reference");

        assert_eq!(sha, "668228422ae6a00e4ad889ee87cd7109ec5666a7");
    }

    #[test]
    fn resolve_reference_retries_after_rate_limit() {
        let mut server = Server::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_secs();

        let _rate_limited = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .expect(1)
            .with_status(403)
            .with_header("x-ratelimit-remaining", "0")
            .with_header("x-ratelimit-reset", &now.to_string())
            .with_body(r#"{"message":"API rate limit exceeded"}"#)
            .create();
        let _success = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .expect(1)
            .with_status(200)
            .with_body(r#"{"sha":"de0fac2e4500dabe0009e67214ff5f5447ce83dd"}"#)
            .create();

        let client = GitHubClient::with_options(GitHubClientOptions {
            base_url: server.url(),
            token: None,
            timeout: Duration::from_secs(5),
            max_retries: 1,
            retry_delay: Duration::from_millis(1),
            max_retry_delay: Duration::from_millis(5),
        })
        .expect("github client");

        let sha = client.resolve_reference("actions", "checkout", "v4").expect("resolve reference");

        assert_eq!(sha, "de0fac2e4500dabe0009e67214ff5f5447ce83dd");
    }

    #[test]
    fn resolve_reference_retries_when_retry_after_is_present() {
        let mut server = Server::new();

        let _rate_limited = server
            .mock("GET", "/repos/actions/cache/commits/v4")
            .expect(1)
            .with_status(403)
            .with_header("retry-after", "0")
            .with_body(r#"{"message":"You have exceeded a secondary rate limit"}"#)
            .create();
        let _success = server
            .mock("GET", "/repos/actions/cache/commits/v4")
            .expect(1)
            .with_status(200)
            .with_body(r#"{"sha":"668228422ae6a00e4ad889ee87cd7109ec5666a7"}"#)
            .create();

        let client = GitHubClient::with_options(GitHubClientOptions {
            base_url: server.url(),
            token: Some(String::from("ghp_testtoken")),
            timeout: Duration::from_secs(5),
            max_retries: 1,
            retry_delay: Duration::from_millis(1),
            max_retry_delay: Duration::from_millis(5),
        })
        .expect("github client");

        let sha = client.resolve_reference("actions", "cache", "v4").expect("resolve reference");

        assert_eq!(sha, "668228422ae6a00e4ad889ee87cd7109ec5666a7");
    }

    const IP_ALLOW_LIST_BODY: &str = r#"{"message":"Although you appear to have the correct authorization credentials, the `aquasecurity` organization has an IP allow list enabled, and your IP address is not permitted to access this resource."}"#;

    #[test]
    fn resolve_reference_falls_back_to_an_anonymous_request_on_403() {
        let mut server = Server::new();
        let forbidden = server
            .mock("GET", "/repos/aquasecurity/trivy-action/commits/0.33.1")
            .match_header("authorization", Matcher::Regex(r"^Bearer\s+ghp_testtoken$".into()))
            .expect(1)
            .with_status(403)
            .with_body(IP_ALLOW_LIST_BODY)
            .create();
        let anonymous = server
            .mock("GET", "/repos/aquasecurity/trivy-action/commits/0.33.1")
            .match_header("authorization", Matcher::Missing)
            .expect(1)
            .with_status(200)
            .with_body(r#"{"sha":"6c175e9c4083a92bbca2f9724c8a5e33bc2d97a5"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let sha = client
            .resolve_reference("aquasecurity", "trivy-action", "0.33.1")
            .expect("resolve reference");

        assert_eq!(sha, "6c175e9c4083a92bbca2f9724c8a5e33bc2d97a5");
        forbidden.assert();
        anonymous.assert();
    }

    #[test]
    fn latest_reference_falls_back_to_an_anonymous_request_on_403() {
        let mut server = Server::new();
        let forbidden = server
            .mock("GET", "/repos/aquasecurity/trivy-action/releases/latest")
            .match_header("authorization", Matcher::Regex(r"^Bearer\s+ghp_testtoken$".into()))
            .expect(1)
            .with_status(403)
            .with_body(IP_ALLOW_LIST_BODY)
            .create();
        let anonymous = server
            .mock("GET", "/repos/aquasecurity/trivy-action/releases/latest")
            .match_header("authorization", Matcher::Missing)
            .expect(1)
            .with_status(200)
            .with_body(r#"{"tag_name":"0.33.1"}"#)
            .create();
        let _commit = server
            .mock("GET", "/repos/aquasecurity/trivy-action/commits/0.33.1")
            .with_status(200)
            .with_body(r#"{"sha":"6c175e9c4083a92bbca2f9724c8a5e33bc2d97a5"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let latest =
            client.latest_reference("aquasecurity", "trivy-action").expect("latest reference");

        assert_eq!(latest.version, "0.33.1");
        assert_eq!(latest.sha, "6c175e9c4083a92bbca2f9724c8a5e33bc2d97a5");
        forbidden.assert();
        anonymous.assert();
    }

    #[test]
    fn resolve_reference_reports_the_original_403_when_the_anonymous_retry_fails() {
        let mut server = Server::new();
        let _forbidden = server
            .mock("GET", "/repos/aquasecurity/trivy-action/commits/0.33.1")
            .match_header("authorization", Matcher::Regex(r"^Bearer\s+ghp_testtoken$".into()))
            .expect(1)
            .with_status(403)
            .with_body(IP_ALLOW_LIST_BODY)
            .create();
        let anonymous = server
            .mock("GET", "/repos/aquasecurity/trivy-action/commits/0.33.1")
            .match_header("authorization", Matcher::Missing)
            .expect(1)
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let error = client
            .resolve_reference("aquasecurity", "trivy-action", "0.33.1")
            .expect_err("forbidden");
        let message = error.to_string();

        assert!(message.contains("403 Forbidden"), "{message}");
        assert!(message.contains("IP allow list"), "{message}");
        assert!(message.contains("anonymous retry"), "{message}");
        assert!(message.contains("404"), "{message}");
        anonymous.assert();
    }

    #[test]
    fn resolve_reference_does_not_fall_back_to_anonymous_on_401() {
        let mut server = Server::new();
        let unauthorized = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .expect(1)
            .with_status(401)
            .with_body(r#"{"message":"Bad credentials"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let error =
            client.resolve_reference("actions", "checkout", "v4").expect_err("bad credentials");
        let message = error.to_string();

        assert!(message.contains("401 Unauthorized"), "{message}");
        assert!(!message.contains("anonymous retry"), "{message}");
        unauthorized.assert();
    }

    #[test]
    fn resolve_reference_without_a_token_sends_a_single_request_on_403() {
        let mut server = Server::new();
        let forbidden = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .match_header("authorization", Matcher::Missing)
            .expect(1)
            .with_status(403)
            .with_body(r#"{"message":"Resource not accessible"}"#)
            .create();

        let client = GitHubClient::new(server.url(), None).expect("github client");
        let error = client.resolve_reference("actions", "checkout", "v4").expect_err("forbidden");
        let message = error.to_string();

        assert!(message.contains("403 Forbidden"), "{message}");
        assert!(!message.contains("anonymous retry"), "{message}");
        forbidden.assert();
    }

    #[test]
    fn create_ref_does_not_fall_back_to_anonymous_on_403() {
        let mut server = Server::new();
        let forbidden = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .expect(1)
            .with_status(403)
            .with_body(r#"{"message":"Resource not accessible by integration"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let error =
            client.create_ref("acme", "demo", "tags/v1.2.3", "commitsha").expect_err("forbidden");

        assert!(error.to_string().contains("403 Forbidden"), "{error}");
        forbidden.assert();
    }

    #[test]
    fn reference_sha_returns_none_when_ref_is_missing() {
        let mut server = Server::new();
        let _mock = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v1.2.3")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let sha = client.reference_sha("acme", "demo", "tags/v1.2.3").expect("reference sha");

        assert_eq!(sha, None);
    }

    #[test]
    fn reference_sha_returns_object_sha() {
        let mut server = Server::new();
        let _mock = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v1.2.3")
            .with_status(200)
            .with_body(r#"{"ref":"refs/tags/v1.2.3","object":{"sha":"tagsha"}}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let sha = client.reference_sha("acme", "demo", "tags/v1.2.3").expect("reference sha");

        assert_eq!(sha.as_deref(), Some("tagsha"));
    }

    #[test]
    fn create_ref_posts_fully_qualified_tag_reference() {
        let mut server = Server::new();
        let _mock = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .match_body(Matcher::Regex(r#""ref":"refs/tags/v1\.2\.3""#.into()))
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v1.2.3"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        client.create_ref("acme", "demo", "tags/v1.2.3", "commitsha").expect("create ref");
    }

    #[test]
    fn update_ref_serializes_force_flag() {
        let mut server = Server::new();
        let _mock = server
            .mock("PATCH", "/repos/acme/demo/git/refs/tags/v1")
            .match_body(Matcher::Regex(r#""force":true"#.into()))
            .with_status(200)
            .with_body(r#"{"ref":"refs/tags/v1"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        client.update_ref("acme", "demo", "tags/v1", "commitsha", true).expect("update ref");
    }

    #[test]
    fn update_ref_fast_forward_reports_non_fast_forward_updates() {
        let mut server = Server::new();
        let _mock = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .match_body(Matcher::Regex(r#""force":false"#.into()))
            .with_status(422)
            .with_body(r#"{"message":"Update is not a fast forward"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let advanced = client
            .update_ref_fast_forward("acme", "demo", "heads/main", "commitsha")
            .expect("fast-forward ref");

        assert!(!advanced);
    }

    #[test]
    fn update_ref_fast_forward_errors_on_unrelated_validation_failures() {
        let mut server = Server::new();
        let _mock = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(422)
            .with_body(r#"{"message":"Object does not exist"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let error = client
            .update_ref_fast_forward("acme", "demo", "heads/main", "commitsha")
            .expect_err("validation failure");

        assert!(error.to_string().contains("Object does not exist"), "{error}");
    }

    #[test]
    fn update_ref_fast_forward_succeeds_when_ref_is_current() {
        let mut server = Server::new();
        let _mock = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let advanced = client
            .update_ref_fast_forward("acme", "demo", "heads/main", "commitsha")
            .expect("fast-forward ref");

        assert!(advanced);
    }

    #[test]
    fn list_tags_paginates_until_a_short_page() {
        let mut server = Server::new();
        let full_page: Vec<String> = (0..100)
            .map(|index| format!(r#"{{"name":"v0.0.{index}","commit":{{"sha":"{index:040}"}}}}"#))
            .collect();
        let _first = server
            .mock("GET", "/repos/acme/demo/tags?per_page=100&page=1")
            .expect(1)
            .with_status(200)
            .with_body(format!("[{}]", full_page.join(",")))
            .create();
        let _second = server
            .mock("GET", "/repos/acme/demo/tags?per_page=100&page=2")
            .expect(1)
            .with_status(200)
            .with_body(r#"[{"name":"v1.0.0","commit":{"sha":"lasttagsha"}}]"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let tags = client.list_tags("acme", "demo", 5).expect("list tags");

        assert_eq!(tags.len(), 101);
        assert_eq!(tags[100].name, "v1.0.0");
        assert_eq!(tags[100].sha.as_deref(), Some("lasttagsha"));
    }

    #[test]
    fn compare_commits_paginates_and_flags_merge_commits() {
        let mut server = Server::new();
        let first_page: Vec<String> = (0..100)
            .map(|index| {
                format!(
                    r#"{{"sha":"{index:040}","commit":{{"message":"feat: change {index}"}},"parents":[{{}}]}}"#
                )
            })
            .collect();
        let _first = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=1")
            .expect(1)
            .with_status(200)
            .with_body(format!(r#"{{"total_commits":101,"commits":[{}]}}"#, first_page.join(",")))
            .create();
        let _second = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=2")
            .expect(1)
            .with_status(200)
            .with_body(
                r#"{"total_commits":101,"commits":[{"sha":"mergesha","commit":{"message":"Merge pull request #1"},"parents":[{},{}]}]}"#,
            )
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let range = client
            .compare_commits("acme", "demo", "v0.1.0", "headsha", 5)
            .expect("compare commits");

        assert_eq!(range.commits.len(), 101);
        assert!(!range.truncated);
        assert!(!range.commits[0].is_merge);
        assert!(range.commits[100].is_merge);
        assert_eq!(range.commits[100].sha, "mergesha");
    }

    #[test]
    fn compare_commits_marks_truncation_at_the_page_cap() {
        let mut server = Server::new();
        let first_page: Vec<String> = (0..100)
            .map(|index| {
                format!(r#"{{"sha":"{index:040}","commit":{{"message":"fix: {index}"}},"parents":[{{}}]}}"#)
            })
            .collect();
        let _first = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=1")
            .expect(1)
            .with_status(200)
            .with_body(format!(r#"{{"total_commits":150,"commits":[{}]}}"#, first_page.join(",")))
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let range = client
            .compare_commits("acme", "demo", "v0.1.0", "headsha", 1)
            .expect("compare commits");

        assert_eq!(range.commits.len(), 100);
        assert!(range.truncated);
    }

    #[test]
    fn compare_commits_stops_when_pages_run_dry() {
        let mut server = Server::new();
        let first_page: Vec<String> = (0..100)
            .map(|index| {
                format!(
                    r#"{{"sha":"{index:040}","commit":{{"message":"fix: {index}"}},"parents":[{{}}]}}"#
                )
            })
            .collect();
        let _first = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=1")
            .expect(1)
            .with_status(200)
            .with_body(format!(r#"{{"total_commits":300,"commits":[{}]}}"#, first_page.join(",")))
            .create();
        let _second = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=2")
            .expect(1)
            .with_status(200)
            .with_body(r#"{"total_commits":300,"commits":[]}"#)
            .create();
        let _third = server
            .mock("GET", "/repos/acme/demo/compare/v0.1.0...headsha?per_page=100&page=3")
            .expect(0)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let range =
            client.compare_commits("acme", "demo", "v0.1.0", "headsha", 10).expect("compare");

        assert_eq!(range.commits.len(), 100);
        assert!(range.truncated);
    }

    #[test]
    fn list_commits_stops_on_a_short_page() {
        let mut server = Server::new();
        let _first = server
            .mock("GET", "/repos/acme/demo/commits?sha=headsha&per_page=100&page=1")
            .expect(1)
            .with_status(200)
            .with_body(r#"[{"sha":"onlysha","commit":{"message":"feat: initial"},"parents":[]}]"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let range = client.list_commits("acme", "demo", "headsha", 3).expect("list commits");

        assert_eq!(range.commits.len(), 1);
        assert!(!range.truncated);
        assert_eq!(range.commits[0].message, "feat: initial");
    }

    #[test]
    fn create_release_posts_tag_and_returns_url() {
        let mut server = Server::new();
        let _mock = server
            .mock("POST", "/repos/acme/demo/releases")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""tag_name":"v1\.2\.3""#.into()),
                Matcher::Regex(r#""target_commitish":"commitsha""#.into()),
                Matcher::Regex(r#""name":"Release v1\.2\.3""#.into()),
            ]))
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v1.2.3"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let release = client
            .create_release("acme", "demo", "v1.2.3", "Release v1.2.3", "notes", "commitsha")
            .expect("create release");

        assert_eq!(release.url, "https://github.com/acme/demo/releases/tag/v1.2.3");
    }

    #[test]
    fn ensure_token_requires_a_token() {
        let client = GitHubClient::new("https://api.github.com", None).expect("github client");
        let error = client.ensure_token().expect_err("missing token");

        assert!(error.to_string().contains("GitHub token"));
    }

    #[test]
    fn validate_token_scopes_requires_workflow_scope() {
        let mut server = Server::new();
        let _user = server
            .mock("GET", "/user")
            .match_header("authorization", Matcher::Regex("^Bearer\\s+ghp_testtoken$".into()))
            .with_status(200)
            .with_header("x-oauth-scopes", "repo")
            .with_body(r#"{"login":"octocat"}"#)
            .create();

        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let error = client.validate_token_scopes().expect_err("missing workflow scope");

        assert!(error.to_string().contains("workflow scope"));
    }
}
