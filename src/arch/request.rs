//! What every write request shares: the size cap, parsing, the target, the writer and `--doing`.

use anyhow::Result;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::vault::{self, Model};
use super::{Code, Refusal, one_line};

/// The largest request a model command reads, and the largest state file it reads, in bytes.
pub const INPUT_MAX: u64 = 4 * 1024 * 1024;
pub const WRITER_MAX: usize = 64;
pub const OPERATION_MAX: usize = 64;
/// The characters an operation ID may use; [`check_operation`] enforces the same rule.
pub const OPERATION_PATTERN: &str = "^[A-Za-z0-9._-]+$";
const DOING_MAX: usize = 140;

/// The model and state a request is written against.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// The model's local ID.
    pub model: String,
    /// The current state's local ID, or the proposal's for a request inside a proposal.
    pub state: String,
}

/// A request of at most [`INPUT_MAX`] bytes, parsed from JSON; `what` names it in a refusal.
pub(super) fn parse<T: DeserializeOwned>(input: &[u8], what: &str) -> Result<T> {
    if input.len() as u64 > INPUT_MAX {
        return Err(invalid(format!(
            "the {what} request is larger than {INPUT_MAX} bytes"
        )));
    }
    serde_json::from_slice(input).map_err(|e| {
        invalid(format!(
            "the {what} request does not parse: {}",
            vault::parse_message(&e)
        ))
    })
}

pub(super) fn check_doing(doing: &str) -> Result<()> {
    if !one_line(doing, DOING_MAX) {
        return Err(Refusal::new(
            Code::DoingInvalid,
            format!("--doing needs one line of 1 to {DOING_MAX} characters"),
        )
        .into());
    }
    Ok(())
}

/// Refuses an operation ID that is not 1 to [`OPERATION_MAX`] characters from `A-Za-z0-9._-`;
/// the ID names a journal file.
pub(super) fn check_operation(operation: &str) -> Result<()> {
    if !operation.is_empty()
        && operation.len() <= OPERATION_MAX
        && operation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Ok(());
    }
    Err(invalid(format!(
        "operation_id needs 1 to {OPERATION_MAX} characters from A-Z, a-z, 0-9, `.`, `_`, `-`"
    )))
}

pub(super) fn check_writer(writer: &str) -> Result<()> {
    if one_line(writer, WRITER_MAX) {
        return Ok(());
    }
    Err(invalid(format!(
        "writer needs 1 to {WRITER_MAX} characters and no control characters"
    )))
}

/// Refuses a request whose target is not the vault's model and the state `model` holds: the
/// current state, or the proposal it was loaded as. A planning model has no current state.
pub(super) fn check_model(model: &Model, target: &Target) -> Result<()> {
    let manifest = &model.manifest;
    let state = match &model.proposal {
        Some(proposal) => Some(proposal.id.as_str()),
        None => manifest.current.as_ref().map(|c| c.id.as_str()),
    };
    if target.model == manifest.id && state.is_none() {
        return Err(Refusal::new(
            Code::NoCurrentState,
            "the model has no current state yet; draw a proposal with `boundrail model propose`, work in it with --proposal and adopt it",
        )
        .into());
    }
    if target.model != manifest.id || Some(target.state.as_str()) != state {
        let state = state.unwrap_or("none");
        return Err(Refusal::new(
            Code::TargetMismatch,
            format!(
                "the request targets model {} state {}, but the vault holds model {} state {state}",
                target.model, target.state, manifest.id
            ),
        )
        .with_details(json!({ "model": manifest.id, "state": state }))
        .into());
    }
    Ok(())
}

pub(super) fn invalid(message: String) -> anyhow::Error {
    Refusal::new(Code::RequestInvalid, message).into()
}
