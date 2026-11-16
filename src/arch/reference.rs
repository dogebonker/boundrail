//! Where a module's references and binding lead: another model's vault or history, and the code
//! a binding names. Following a reference reads the other checkout's files and history ref with
//! the hardened Git runner; it never writes there, fetches, or runs anything from it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::checkpoint::{self, Checkpoint};
use super::lsp::{read_json, user_config};
use super::types::{Binding, ReadingState, Reference};
use super::vault::{self, Model, is_repo_path};
use super::{Code, Refusal};

/// The user's local checkouts by name, below the user's config folder: a JSON array of
/// `{name, path}` with absolute paths. A reference's `<alias>:` and a binding's `repo` name them.
pub const CHECKOUTS: &str = "boundrail/checkouts.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configured {
    name: String,
    path: PathBuf,
}

fn alias(model: &str) -> Option<(&str, &str)> {
    model
        .split_once(':')
        .filter(|(alias, _)| !alias.contains('/'))
}

/// Whether `model` is a reference's model: a repository path, or an alias of ASCII letters,
/// digits, `.`, `_` and `-`, then `:` and an optional repository path.
pub(crate) fn is_model(model: &str) -> bool {
    match alias(model) {
        Some((name, path)) => {
            let named = !name.is_empty()
                && (name.bytes()).all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
            named && (path.is_empty() || is_repo_path(path))
        }
        None => is_repo_path(model),
    }
}

fn not_found(message: String) -> anyhow::Error {
    Refusal::new(Code::CheckoutNotFound, message).into()
}

fn configured(name: &str) -> Result<PathBuf> {
    let file = user_config(CHECKOUTS);
    let shown = (file.as_ref()).map_or_else(
        || format!("$XDG_CONFIG_HOME/{CHECKOUTS}"),
        |p| p.display().to_string(),
    );
    let listed = match &file {
        Some(file) => read_json::<Vec<Configured>>(file, &shown)?.unwrap_or_default(),
        None => Vec::new(),
    };
    let found = (listed.into_iter().find(|c| c.name == name))
        .ok_or_else(|| not_found(format!("no local checkout is named `{name}` in {shown}")))?;
    if !found.path.is_absolute() {
        let message = format!("{shown}: checkout `{name}` needs an absolute path");
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    Ok(found.path)
}

fn below(base: &Path, path: &str) -> Result<PathBuf> {
    let base = fs::canonicalize(base)
        .map_err(|e| not_found(format!("{} cannot be read: {e}", base.display())))?;
    let joined = if path.is_empty() {
        base.clone()
    } else {
        base.join(path)
    };
    match fs::canonicalize(&joined) {
        Ok(target) if target.starts_with(&base) => Ok(target),
        Ok(_) => {
            let message = format!("`{path}` leaves {} through a symbolic link", base.display());
            Err(Refusal::new(Code::RequestInvalid, message).into())
        }
        Err(_) => Ok(joined),
    }
}

/// The folder a reference's `model` names: below the checkout root `root`, or below the
/// configured checkout of its alias; `root` itself when absent.
pub fn checkout_of(root: &Path, model: Option<&str>) -> Result<PathBuf> {
    match model {
        None => Ok(root.to_path_buf()),
        Some(model) => match alias(model) {
            Some((name, path)) => below(&configured(name)?, path),
            None => below(root, model),
        },
    }
}

/// A reference followed: the model in the state read, and the checkpoint it came from.
pub struct Followed {
    /// The checkout the model was read from.
    pub checkout: PathBuf,
    pub model: Model,
    /// The state read: a proposal by its qualified ID, a checkpoint by its ID.
    pub state: ReadingState,
    pub checkpoint: Option<Checkpoint>,
}

/// Reads what `reference` names from the checkout around the root `root`: the checkpoint its
/// `pin` names from the other repository's history, else its state. Refusals say why it cannot
/// be read: no checkout or vault there, an unknown alias, proposal or checkpoint, an invalid model.
pub fn follow(root: &Path, reference: &Reference) -> Result<Followed> {
    let checkout = checkout_of(root, reference.model.as_deref())?;
    let model = match (&reference.pin, &reference.state) {
        (Some(which), _) | (None, ReadingState::Checkpoint(which)) => {
            let (checkpoint, model) = checkpoint::load(&checkout, which)?;
            return Ok(Followed {
                checkout,
                model,
                state: ReadingState::Checkpoint(checkpoint.id.clone()),
                checkpoint: Some(checkpoint),
            });
        }
        (None, ReadingState::Current) => vault::load(&checkout)?,
        (None, ReadingState::Proposal(proposal)) => {
            vault::load_proposal(vault::locate(&checkout)?, proposal)?
        }
    };
    let state = match &model.proposal {
        Some(proposal) => ReadingState::Proposal(model.qualify(&proposal.id)),
        None => ReadingState::Current,
    };
    Ok(Followed {
        checkout,
        model,
        state,
        checkpoint: None,
    })
}

/// Where a binding's code is.
#[derive(Debug, Serialize)]
pub struct CodeTarget {
    pub repo: Option<String>,
    pub path: String,
    pub symbol: Option<String>,
    /// The checkout's commit when the model read was retained: the source of a checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The latest correction `commit` comes from, when one changed the source the checkpoint
    /// recorded. Its `writer` is what the correcting checkpoint's `Writer` trailer asserts; no
    /// signature is checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction: Option<checkpoint::Correction>,
    /// The absolute path on this machine: in this checkout, or for a `repo` in the checkout the
    /// user's [`CHECKOUTS`] names so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<PathBuf>,
    /// Why there is no `local_path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// The code target of `binding` read from the checkout root `root`; `checkpoint` is the one the
/// model read was retained at, whose source is a commit of this checkout, not of another `repo`.
pub fn code_target(root: &Path, binding: &Binding, checkpoint: Option<&Checkpoint>) -> CodeTarget {
    let local = match &binding.repo {
        None => below(root, &binding.path),
        Some(repo) => configured(repo).and_then(|base| below(&base, &binding.path)),
    };
    let (local_path, unavailable) = match local {
        Ok(path) => (Some(path), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let here = checkpoint.filter(|_| binding.repo.is_none());
    CodeTarget {
        repo: binding.repo.clone(),
        path: binding.path.clone(),
        symbol: binding.symbol.clone(),
        commit: here.and_then(|c| c.source.clone()),
        correction: here.and_then(|c| c.corrections.last().cloned()),
        local_path,
        unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_models_are_paths_or_aliased_paths() {
        for good in [
            "ledger",
            "vendor/ledger",
            "ledger:",
            "led-ger_1.x:sub/dir",
            "a/b:c",
        ] {
            assert!(is_model(good), "{good}");
        }
        for bad in [
            "",
            ":",
            ":x",
            "../ledger",
            "/abs",
            "ledger:../x",
            "ledger:/abs",
            "C:\\x",
            "a b:x",
            "vendor/../..",
        ] {
            assert!(!is_model(bad), "{bad}");
        }
    }
}
