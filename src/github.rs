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
            max_retry_delay: Duration::from_secs(60),
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
struct AddLabelsRequest<'a> {
    labels: &'a [String],
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

        let tags = self.send_with_retry(
            || self.get(&format!("/repos/{owner}/{repository}/tags")).query(&[("per_page", "1")]),
            || format!("fetch tags for {owner}/{repository}"),
        )?;
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
        let response = self.send_with_retry(
            || self.get(&format!("/repos/{owner}/{repository}/commits/{encoded_reference}")),
            || format!("resolve {owner}/{repository}@{reference}"),
        )?;
        let commit = response.json::<CommitResponse>().with_context(|| {
            format!("failed to decode commit response for {owner}/{repository}@{reference}")
        })?;

        Ok(commit.sha)
    }

    fn latest_release_tag(&self, owner: &str, repository: &str) -> Result<Option<String>> {
        let response = self.send_with_retry_allowing_not_found(
            || self.get(&format!("/repos/{owner}/{repository}/releases/latest")),
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
        let response = self.send_with_retry(
            || self.get(&format!("/repos/{owner}/{repository}")),
            || format!("fetch repository metadata for {owner}/{repository}"),
        )?;
        let repository = response.json::<RepositoryResponse>().with_context(|| {
            format!("failed to decode repository response for {owner}/{repository}")
        })?;
        Ok(repository.default_branch)
    }

    pub fn branch_head_sha(&self, owner: &str, repository: &str, branch: &str) -> Result<String> {
        let response = self.send_with_retry(
            || self.get(&format!("/repos/{owner}/{repository}/git/ref/heads/{branch}")),
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
        let response = self.send_with_retry(
            || self.get(&format!("/repos/{owner}/{repository}/git/commits/{commit_sha}")),
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
        let reference = format!("refs/heads/{branch}");
        let payload = CreateReferenceRequest { reference: &reference, sha: base_sha };
        self.post_json(&format!("/repos/{owner}/{repository}/git/refs"), &payload, || {
            format!("create branch {branch} for {owner}/{repository}")
        })?;
        Ok(())
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
        let payload = UpdateReferenceRequest { sha: commit_sha, force: false };
        self.patch_json(
            &format!("/repos/{owner}/{repository}/git/refs/heads/{branch}"),
            &payload,
            || format!("update branch {branch} for {owner}/{repository}"),
        )?;
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
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(token) = self.token.as_deref() {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        request
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

    fn send_with_retry<F, D>(&self, mut build_request: F, describe: D) -> Result<Response>
    where
        F: FnMut() -> RequestBuilder,
        D: Fn() -> String,
    {
        let mut attempt = 0u32;

        loop {
            match build_request().send() {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    if Self::should_retry_response(&response) && attempt < self.max_retries {
                        self.sleep_for_retry(response.headers(), attempt);
                        attempt += 1;
                        continue;
                    }
                    return self.error_from_response(response, &describe());
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

    fn send_with_retry_allowing_not_found<D>(
        &self,
        mut build_request: impl FnMut() -> RequestBuilder,
        describe: D,
    ) -> Result<Option<Response>>
    where
        D: Fn() -> String,
    {
        let mut attempt = 0u32;

        loop {
            match build_request().send() {
                Ok(response) if response.status() == StatusCode::NOT_FOUND => return Ok(None),
                Ok(response) if response.status().is_success() => return Ok(Some(response)),
                Ok(response) => {
                    if Self::should_retry_response(&response) && attempt < self.max_retries {
                        self.sleep_for_retry(response.headers(), attempt);
                        attempt += 1;
                        continue;
                    }
                    return self.error_from_response(response, &describe()).map(Some);
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
