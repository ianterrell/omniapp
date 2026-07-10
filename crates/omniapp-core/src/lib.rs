//! Filesystem-first services shared by the OmniApp CLI and web application.

mod cache;
mod document;
mod query;
mod record;
mod workspace;

pub use cache::{Cache, CacheError, SearchHit};
pub use query::{Page, execute_query};
pub use record::{Record, RecordInput};
pub use workspace::{Diagnostic, LoadedWorkspace, ValidationReport, Workspace, WorkspaceError};
