//! The commit that introduced a finding, as a scan document records it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub commit: String,
    pub author: String,
    pub email: String,
    pub authored_at: String,
    pub committed_at: String,
    pub subject: String,
    /// Inferred from the message only. No network request or inferred GitHub identity.
    pub pull_request: Option<u64>,
    pub age_days: u64,
}
