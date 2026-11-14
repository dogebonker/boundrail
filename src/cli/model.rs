use super::*;
use std::io::Read as _;
use std::iter;

use boundrail::arch::claims::{self, Claim};
use boundrail::arch::edit::{self, Receipt};
use boundrail::arch::request::INPUT_MAX;
use boundrail::arch::types::{self, SCHEMAS};
use boundrail::arch::vault::{self, Model, ModuleView, RelationshipView};
use boundrail::arch::{Code, Refusal};
use serde_json::{Value, json};

const MODEL_SCHEMA: &str = "boundrail/model/v1";

#[derive(Args)]
pub(super) struct ModelArgs {
    /// Directory inside the Git checkout that holds the model
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[arg(long, global = true, value_enum, default_value_t = Format::Json)]
    format: Format,
    #[command(subcommand)]
    command: ModelCmd,
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Create the model vault in .boundrail/model at the checkout root
    Init {
        #[arg(long)]
        name: String,
    },
    /// Print the model, or one module with its parts and relationships
    Read {
        /// Qualified ID, local ID, or a unique current or former module name
        subject: Option<String>,
    },
    /// List the vault file and request schemas, or print one as JSON Schema
    Schema { name: Option<String> },
    /// Apply a batch of module and relationship changes (schema `model-edit`)
    Edit {
        /// Request file; `-` or absent reads standard input
        #[arg(long)]
        input: Option<PathBuf>,
        /// One line saying why the model changes, 1 to 140 characters
        #[arg(long)]
        doing: String,
    },
    /// Finish edits that a crash or failed write left partly applied
    Recover {
        /// Drop this interrupted operation instead, writing nothing to the vault
        #[arg(long, value_name = "OPERATION_ID")]
        abandon: Option<String>,
    },
    /// Reserve modules, relationships or the model's structure for one writer (schema `model-claim`)
    Claim {
        /// Request file; `-` or absent reads standard input
        #[arg(long)]
        input: Option<PathBuf>,
        /// One line saying why, 1 to 140 characters
        #[arg(long)]
        doing: String,
    },
    /// Release your own claim (schema `model-release`), or revoke another writer's with --revoke
    Release {
        /// Request file; `-` or absent reads standard input
        #[arg(long, conflicts_with = "revoke")]
        input: Option<PathBuf>,
        /// Claim ID to revoke, so it stops blocking anyone
        #[arg(long, requires_all = ["writer", "reason"])]
        revoke: Option<String>,
        /// Who revokes the claim
        #[arg(long, requires = "revoke")]
        writer: Option<String>,
        /// One line saying why the claim is revoked
        #[arg(long, requires = "revoke")]
        reason: Option<String>,
    },
    /// List active claims and revoked ones not yet released
    Claims,
}

/// A refusal is printed on stdout and exits with code 2; other errors go to main's handler.
pub(super) fn run(args: ModelArgs) -> Result<ExitCode> {
    let markdown = matches!(args.format, Format::Markdown);
    match execute(args) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => match error.downcast_ref::<Refusal>() {
            Some(refusal) => {
                print_refusal(refusal, markdown)?;
                Ok(ExitCode::from(2))
            }
            None => Err(error),
        },
    }
}

fn execute(args: ModelArgs) -> Result<()> {
    let markdown = matches!(args.format, Format::Markdown);
    match args.command {
        ModelCmd::Init { name } => {
            let model = vault::init(&args.root, &name)?;
            if markdown {
                println!(
                    "Created model {} ({}) at {}",
                    model.manifest.name,
                    model.manifest.id,
                    model.dir.display()
                );
                println!("revision {}", model.revision.digest);
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "init",
                    "success": true,
                    "vault": model.dir,
                    "manifest": model.manifest,
                    "revision": model.revision,
                }),
            )
        }
        ModelCmd::Read { subject: None } => {
            let state = state_dir(&args.root);
            let pending = state.as_deref().and_then(pending_operation);
            let model = load_for_read(&args.root, pending.as_ref())?;
            let reading = model.read();
            let active = state.as_deref().and_then(active_claims);
            if markdown {
                print_reading(&model, &reading);
                print_claims(active.as_deref());
                return Ok(());
            }
            let mut output = json!({
                "schema": MODEL_SCHEMA,
                "kind": "read",
                "success": true,
                "vault": model.dir,
                "manifest": model.manifest,
                "config": model.config,
                "roots": reading.roots,
                "modules": reading.modules,
                "relationships": reading.relationships,
                "revision": model.revision,
            });
            if let Some(active) = active {
                output["claims"] = json!(active);
            }
            if let Some(pending) = pending {
                output["pending_operation"] = pending;
            }
            write(None, &output)
        }
        ModelCmd::Read {
            subject: Some(subject),
        } => {
            let state = state_dir(&args.root);
            let pending = state.as_deref().and_then(pending_operation);
            let model = load_for_read(&args.root, pending.as_ref())?;
            let selection = model.select(&subject)?;
            let subjects: Vec<&str> = iter::once(&selection.module.id)
                .chain(selection.parts.iter().map(|m| &m.id))
                .chain(selection.incoming.iter().map(|r| &r.id))
                .chain(selection.outgoing.iter().map(|r| &r.id))
                .map(String::as_str)
                .collect();
            let active = state
                .as_deref()
                .and_then(active_claims)
                .map(|active| claims::holding(&model, active, &subjects));
            if markdown {
                let names = names(&model);
                print_module(&selection.module, 0);
                if let Some(parent) = selection.module.parent {
                    println!("  part of {}", name_of(&names, parent));
                }
                if !selection.parts.is_empty() {
                    println!("Parts:");
                    selection.parts.iter().for_each(|m| print_module(m, 1));
                }
                for (heading, list) in [
                    ("Outgoing", &selection.outgoing),
                    ("Incoming", &selection.incoming),
                ] {
                    if !list.is_empty() {
                        println!("{heading}:");
                        list.iter().for_each(|r| print_relationship(&names, r));
                    }
                }
                print_claims(active.as_deref());
                return Ok(());
            }
            let mut output = json!({
                "schema": MODEL_SCHEMA,
                "kind": "read-module",
                "success": true,
                "vault": model.dir,
                "module": selection.module,
                "parts": selection.parts,
                "incoming": selection.incoming,
                "outgoing": selection.outgoing,
                "revision": model.revision,
            });
            if let Some(active) = active {
                output["claims"] = json!(active);
            }
            if let Some(pending) = pending {
                output["pending_operation"] = pending;
            }
            write(None, &output)
        }
        ModelCmd::Schema { name: None } => {
            let names: Vec<&str> = SCHEMAS.iter().map(|(name, _)| *name).collect();
            if markdown {
                names.iter().for_each(|name| println!("{name}"));
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "schemas",
                    "success": true,
                    "schemas": names,
                }),
            )
        }
        ModelCmd::Schema { name: Some(name) } => {
            let schema = types::schema(&name).ok_or_else(|| {
                let names: Vec<&str> = SCHEMAS.iter().map(|(name, _)| *name).collect();
                Refusal::new(Code::SchemaNotFound, format!("no schema named `{name}`"))
                    .with_details(json!({ "schemas": names }))
            })?;
            write(None, &schema)
        }
        ModelCmd::Edit { input, doing } => {
            let dir = vault::locate(&args.root)?;
            let receipt = edit::run(&dir, &read_request(input.as_deref())?, &doing)?;
            if markdown {
                print_receipt(&receipt);
                return Ok(());
            }
            let mut output = json!({
                "schema": MODEL_SCHEMA,
                "kind": "edit",
                "success": true,
                "vault": dir,
            });
            if let (Some(envelope), Value::Object(fields)) =
                (output.as_object_mut(), serde_json::to_value(&receipt)?)
            {
                envelope.extend(fields);
            }
            write(None, &output)
        }
        ModelCmd::Recover {
            abandon: Some(operation),
        } => {
            let dir = vault::locate(&args.root)?;
            let abandoned = edit::abandon(&dir, &operation)?;
            if markdown {
                println!("Abandoned operation {}", abandoned.operation_id);
                if !abandoned.landed.is_empty() {
                    println!("already written: {}", abandoned.landed.join(", "));
                }
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "abandon",
                    "success": true,
                    "vault": dir,
                    "operation_id": abandoned.operation_id,
                    "landed": abandoned.landed,
                }),
            )
        }
        ModelCmd::Recover { abandon: None } => {
            let dir = vault::locate(&args.root)?;
            let completed = edit::recover(&dir)?;
            if markdown {
                if completed.is_empty() {
                    println!("No interrupted edits");
                }
                completed.iter().for_each(print_receipt);
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "recover",
                    "success": true,
                    "vault": dir,
                    "completed": completed,
                }),
            )
        }
        ModelCmd::Claim { input, doing } => {
            let dir = vault::locate(&args.root)?;
            let claim = claims::claim(&dir, &read_request(input.as_deref())?, &doing)?;
            print_claim_output(markdown, "claim", &dir, &claim)
        }
        ModelCmd::Release {
            input,
            revoke,
            writer,
            reason,
        } => {
            let dir = vault::locate(&args.root)?;
            if let Some(claim) = revoke {
                let (Some(writer), Some(reason)) = (writer, reason) else {
                    anyhow::bail!("--revoke needs --writer and --reason");
                };
                let claim = claims::revoke(&dir, &claim, &writer, &reason)?;
                return print_claim_output(markdown, "revoke", &dir, &claim);
            }
            let claim = claims::release(&dir, &read_request(input.as_deref())?)?;
            print_claim_output(markdown, "release", &dir, &claim)
        }
        ModelCmd::Claims => {
            let dir = vault::locate(&args.root)?;
            let list = claims::list(&dir)?;
            if markdown {
                if list.is_empty() {
                    println!("No claims");
                }
                list.iter().for_each(print_claim);
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "claims",
                    "success": true,
                    "vault": dir,
                    "claims": list,
                }),
            )
        }
    }
}

/// A request from `input`, or standard input for `-` or none. One byte past the limit is enough
/// for the command to refuse an oversized request.
fn read_request(input: Option<&Path>) -> Result<Vec<u8>> {
    let mut request = Vec::new();
    match input {
        Some(path) if path != Path::new("-") => fs::File::open(path)
            .and_then(|file| file.take(INPUT_MAX + 1).read_to_end(&mut request))
            .with_context(|| format!("reading {}", path.display()))?,
        _ => std::io::stdin()
            .take(INPUT_MAX + 1)
            .read_to_end(&mut request)
            .context("reading the request from standard input")?,
    };
    Ok(request)
}

fn print_claim_output(markdown: bool, kind: &str, dir: &Path, claim: &Claim) -> Result<()> {
    if markdown {
        print_claim(claim);
        return Ok(());
    }
    write(
        None,
        &json!({
            "schema": MODEL_SCHEMA,
            "kind": kind,
            "success": true,
            "vault": dir,
            "claim": claim,
        }),
    )
}

/// The machine-local state folder for a reading, or `None` with a warning on stderr. Without a
/// vault there is none, and `load_for_read` reports the missing vault.
fn state_dir(root: &Path) -> Option<PathBuf> {
    let dir = vault::locate(root).ok()?;
    boundrail::arch::state_dir(&dir)
        .inspect_err(|error| eprintln!("warning: claims and pending edits omitted: {error:#}"))
        .ok()
}

/// The edit waiting in the journal of `state`, as `{operation_id, in_progress, hint}`, with a
/// warning on stderr. Reading the journal never makes a reading fail, and never creates it.
fn pending_operation(state: &Path) -> Option<Value> {
    let edit::Interrupted {
        operation_id,
        in_progress,
    } = edit::interrupted(state)
        .inspect_err(|error| eprintln!("warning: pending edits not checked: {error:#}"))
        .ok()??;
    let (status, hint) = if in_progress {
        ("is in progress", "read again when it finishes".to_owned())
    } else {
        (
            "was interrupted",
            format!(
                "run `boundrail model recover` to finish it, or `boundrail model recover --abandon {operation_id}` to drop it"
            ),
        )
    };
    eprintln!("warning: operation {operation_id} {status}; {hint}");
    Some(json!({ "operation_id": operation_id, "in_progress": in_progress, "hint": hint }))
}

/// The model for `model read`; a `MODEL_INVALID` refusal also names the interrupted edit.
fn load_for_read(root: &Path, pending: Option<&Value>) -> Result<Model> {
    vault::load(root).map_err(|mut error| {
        if let (Some(pending), Some(refusal)) = (pending, error.downcast_mut::<Refusal>())
            && refusal.code == Code::ModelInvalid
            && let Some(details) = refusal.details.as_mut()
        {
            details["pending_operation"] = pending.clone();
        }
        error
    })
}

/// The active claims, or `None` with a warning on stderr when the claims file cannot be read:
/// machine-local coordination state never makes a reading fail.
fn active_claims(state: &Path) -> Option<Vec<Claim>> {
    claims::active(state)
        .inspect_err(|error| eprintln!("warning: claims omitted: {error:#}"))
        .ok()
}

fn print_claims(claims: Option<&[Claim]>) {
    if let Some(claims) = claims.filter(|c| !c.is_empty()) {
        println!("Claims:");
        claims.iter().for_each(print_claim);
    }
}

fn print_claim(claim: &Claim) {
    println!("Claim {} by {}: {}", claim.id, claim.writer, claim.reason);
    println!("  doing {}", claim.doing);
    println!("  holds {}", claim.subjects.join(", "));
    if let Some(revoked) = &claim.revoked {
        println!("  revoked by {}: {}", revoked.by, revoked.reason);
    }
}

fn print_receipt(receipt: &Receipt) {
    println!(
        "Edit {} by {}: {}",
        receipt.operation_id, receipt.writer, receipt.doing
    );
    println!(
        "revision {} -> {}",
        receipt.before.digest, receipt.after.digest
    );
    for (label, list) in [
        ("added", &receipt.subjects.added),
        ("changed", &receipt.subjects.changed),
        ("removed", &receipt.subjects.removed),
    ] {
        if !list.is_empty() {
            println!("{label}: {}", list.join(", "));
        }
    }
    for allocation in &receipt.allocated {
        println!("allocated {} for {}", allocation.id, allocation.name);
    }
}

fn print_refusal(refusal: &Refusal, markdown: bool) -> Result<()> {
    if markdown {
        println!("{}: {}", refusal.code.as_str(), refusal.message);
        if let Some(details) = &refusal.details {
            println!("{details}");
        }
        return Ok(());
    }
    let mut error = json!({"code": refusal.code, "message": refusal.message});
    if let Some(details) = &refusal.details {
        error["details"] = details.clone();
    }
    write(
        None,
        &json!({
            "schema": MODEL_SCHEMA,
            "kind": "refusal",
            "success": false,
            "error": error,
        }),
    )
}

fn names(model: &Model) -> BTreeMap<String, &str> {
    model
        .modules
        .iter()
        .map(|m| (model.qualify(&m.id), m.name.as_str()))
        .collect()
}

fn name_of<'a>(names: &BTreeMap<String, &'a str>, id: &'a str) -> &'a str {
    names.get(id).copied().unwrap_or(id)
}

fn print_reading(model: &Model, reading: &vault::Reading<'_>) {
    println!(
        "Model {} ({}), {} modules, {} relationships, revision {}",
        model.manifest.name,
        model.manifest.id,
        reading.modules.len(),
        reading.relationships.len(),
        model.revision.digest
    );
    for (module, depth) in reading.modules.iter().zip(&reading.depths) {
        print_module(module, *depth);
    }
    if !reading.relationships.is_empty() {
        let names = names(model);
        println!("Relationships:");
        for relationship in &reading.relationships {
            print_relationship(&names, relationship);
        }
    }
}

fn print_module(module: &ModuleView<'_>, depth: usize) {
    println!(
        "{}- {} [{}] {}",
        "  ".repeat(depth),
        module.name,
        module.kind,
        module.id
    );
    if let Some(responsibility) = module.responsibility {
        println!("{}  {responsibility}", "  ".repeat(depth));
    }
}

fn print_relationship(names: &BTreeMap<String, &str>, r: &RelationshipView<'_>) {
    println!(
        "  {} -> {} [{}]{} {}",
        name_of(names, &r.from),
        name_of(names, r.to),
        r.kind,
        r.label.map(|l| format!(" {l}")).unwrap_or_default(),
        r.id
    );
}
