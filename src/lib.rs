//! GitHub Actions maintenance primitives.
//!
//! The first shipped capability is secure action pinning: resolve floating
//! `uses:` references to immutable commit SHAs while keeping the original ref in
//! an inline comment.

pub mod cargo;
pub mod crates_io;
pub mod github;
pub mod model;
pub mod pinning;
pub mod remote;
pub mod update;
pub mod workflow;

pub use cargo::{CargoDependencyEntry, CargoUpdateOptions, CargoUpdateReport, CargoUpdater};
pub use crates_io::CratesIoClient;
pub use github::GitHubClient;
pub use model::{FileUpdate, PinChange, PinReport, UpdateChange, UpdateChangeKind, WorkflowAction};
pub use pinning::{PinMode, PinOptions, WorkflowPinner};
pub use remote::{PullRequestOptions, PullRequestResult, RemoteUpdatePublisher};
pub use update::{UpdateMode, UpdateOptions, UpdateReport, VersionEntry, WorkflowUpdater};
