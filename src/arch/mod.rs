//! The authored architecture model: modules and relationships that agents write in
//! `.boundrail/model/` and that Boundrail checks against the code.

pub mod bundle;
pub mod checkpoint;
pub mod claims;
pub mod code;
pub mod compare;
pub mod diagnostic;
pub mod edit;
pub mod footprint;
pub mod github;
pub mod ids;
mod index;
pub mod infra;
pub mod inspect;
pub mod locality;
pub mod lsp;
mod merge;
pub mod picture;
pub mod proposal;
pub mod record;
pub mod render;
pub mod request;
pub mod revision;
pub mod semantic;
mod state;
pub mod types;
pub mod vault;
pub mod view;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};
use serde_json::Value;

/// `<git dir>/boundrail`, the machine-local state folder of the vault folder `vault`. Reading it
/// creates nothing.
pub fn state_dir(vault: &Path) -> anyhow::Result<PathBuf> {
    state::dir(vault)
}

/// Whether `text` is one line of 1 to `max` characters: not blank and free of control characters.
pub fn one_line(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// A repository path's whole segments, ignoring empty and `.` segments.
pub(crate) fn segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// Why a model command declined to act. The CLI prints it on stdout and exits with code 2.
#[derive(Debug)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    NameInvalid,
    CheckoutNotFound,
    VaultNotFound,
    VaultExists,
    ModelInvalid,
    SubjectNotFound,
    SubjectAmbiguous,
    SchemaNotFound,
    DoingInvalid,
    RequestInvalid,
    TargetMismatch,
    StaleRevision,
    ChildrenOrphaned,
    ReferencedByFlow,
    FlowSummaryRequired,
    ReferencedByWalkthrough,
    ReferencedByPicture,
    ReferencedByView,
    VaultLocked,
    OperationReused,
    RecoveryRequired,
    RecoveryConflict,
    OperationAbandoned,
    ClaimNotFound,
    ClaimConflict,
    ClaimNotHeld,
    ClaimRevoked,
    ClaimIncomplete,
    NothingToRetain,
    HistoryMoved,
    CheckpointNotFound,
    SigningFailed,
    NoCurrentState,
    ProposalClosed,
    PredecessorMoved,
    ReconcileConflict,
    ReconcileNoBase,
    ModelMismatch,
    BundleInvalid,
    HistoryDiverged,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NameInvalid => "NAME_INVALID",
            Code::CheckoutNotFound => "CHECKOUT_NOT_FOUND",
            Code::VaultNotFound => "VAULT_NOT_FOUND",
            Code::VaultExists => "VAULT_EXISTS",
            Code::ModelInvalid => "MODEL_INVALID",
            Code::SubjectNotFound => "SUBJECT_NOT_FOUND",
            Code::SubjectAmbiguous => "SUBJECT_AMBIGUOUS",
            Code::SchemaNotFound => "SCHEMA_NOT_FOUND",
            Code::DoingInvalid => "DOING_INVALID",
            Code::RequestInvalid => "REQUEST_INVALID",
            Code::TargetMismatch => "TARGET_MISMATCH",
            Code::StaleRevision => "STALE_REVISION",
            Code::ChildrenOrphaned => "CHILDREN_ORPHANED",
            Code::ReferencedByFlow => "REFERENCED_BY_FLOW",
            Code::FlowSummaryRequired => "FLOW_SUMMARY_REQUIRED",
            Code::ReferencedByWalkthrough => "REFERENCED_BY_WALKTHROUGH",
            Code::ReferencedByPicture => "REFERENCED_BY_PICTURE",
            Code::ReferencedByView => "REFERENCED_BY_VIEW",
            Code::VaultLocked => "VAULT_LOCKED",
            Code::OperationReused => "OPERATION_REUSED",
            Code::RecoveryRequired => "RECOVERY_REQUIRED",
            Code::RecoveryConflict => "RECOVERY_CONFLICT",
            Code::OperationAbandoned => "OPERATION_ABANDONED",
            Code::ClaimNotFound => "CLAIM_NOT_FOUND",
            Code::ClaimConflict => "CLAIM_CONFLICT",
            Code::ClaimNotHeld => "CLAIM_NOT_HELD",
            Code::ClaimRevoked => "CLAIM_REVOKED",
            Code::ClaimIncomplete => "CLAIM_INCOMPLETE",
            Code::NothingToRetain => "NOTHING_TO_RETAIN",
            Code::HistoryMoved => "HISTORY_MOVED",
            Code::CheckpointNotFound => "CHECKPOINT_NOT_FOUND",
            Code::SigningFailed => "SIGNING_FAILED",
            Code::NoCurrentState => "NO_CURRENT_STATE",
            Code::ProposalClosed => "PROPOSAL_CLOSED",
            Code::PredecessorMoved => "PREDECESSOR_MOVED",
            Code::ReconcileConflict => "RECONCILE_CONFLICT",
            Code::ReconcileNoBase => "RECONCILE_NO_BASE",
            Code::ModelMismatch => "MODEL_MISMATCH",
            Code::BundleInvalid => "BUNDLE_INVALID",
            Code::HistoryDiverged => "HISTORY_DIVERGED",
        }
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl Refusal {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}
