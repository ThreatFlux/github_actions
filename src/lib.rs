//! GitHub Actions maintenance primitives.
//!
//! The first shipped capability is secure action pinning: resolve floating
//! `uses:` references to immutable commit SHAs while keeping the original ref in
//! an inline comment.

pub mod github;
pub mod model;
pub mod pinning;
pub mod remote;
pub mod update;
pub mod workflow;

pub use github::GitHubClient;
pub use model::{PinChange, PinReport, WorkflowAction};
pub use pinning::{PinMode, PinOptions, WorkflowPinner};
pub use remote::{PullRequestOptions, PullRequestResult, RemoteUpdatePublisher};
pub use update::{UpdateMode, UpdateOptions, UpdateReport, VersionEntry, WorkflowUpdater};
