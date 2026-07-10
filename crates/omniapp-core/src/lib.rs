//! Filesystem-first services shared by the OmniApp CLI and web application.

mod cache;
mod document;
mod output;
mod query;
mod record;
mod relationship;
mod watcher;
mod workspace;
mod yaml_edit;

pub use cache::{Cache, CacheError, SearchHit};
pub use output::{GeneratedOutput, OutputSet};
pub use query::{Page, execute_query};
pub use record::{Record, RecordInput};
pub use relationship::{RelationshipLink, RelationshipSet};
pub use watcher::{WatcherError, WorkspaceWatcher};
pub use workspace::{Diagnostic, LoadedWorkspace, ValidationReport, Workspace, WorkspaceError};
