use super::*;
use std::io::Read as _;
use std::iter;

use boundrail::arch::claims::{self, Claim};
use boundrail::arch::code::{self, CodeReport, NoScan, Skip};
use boundrail::arch::diagnostic::{Diagnostic, Severity};
use boundrail::arch::edit::{self, Receipt};
use boundrail::arch::request::INPUT_MAX;
use boundrail::arch::semantic;
use boundrail::arch::types::{self, Collection, Flow, SCHEMAS, Step};
use boundrail::arch::vault::{self, CollectionView, Found, Model, ModuleView, RelationshipView};
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
    command: Command,
}

/// `model check` exits with its own verdict code; every other command exits 0 or refuses.
#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Model(ModelCmd),
    /// Check the model's own shape and compare it with the code; exits 1 when any warning is found
    Check {
        /// Run one check only; the code check scans the checkout
        #[arg(long, value_enum)]
        only: Option<CheckOnly>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CheckOnly {
    Semantic,
    Code,
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Create the model vault in .boundrail/model at the checkout root
    Init {
        #[arg(long)]
        name: String,
    },
    /// Print the model, one module with its parts and relationships, a collection, or a flow
    Read {
        /// Qualified ID, local ID, or a unique current or former module name; or a collection or
        /// flow ID
        #[arg(conflicts_with = "flow")]
        subject: Option<String>,
        /// Print one flow with its steps: its qualified or local ID, or its unique name
        #[arg(long)]
        flow: Option<String>,
    },
    /// List the vault file and request schemas, or print one as JSON Schema
    Schema { name: Option<String> },
    /// Apply a batch of module, relationship, collection, flow and step changes (schema `model-edit`)
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
    /// Reserve modules, relationships, collections, flows or the model's structure for one writer (schema `model-claim`)
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
    let result = match args.command {
        Command::Check { only } => check(&args.root, markdown, only),
        Command::Model(command) => {
            execute(&args.root, markdown, command).map(|()| ExitCode::SUCCESS)
        }
    };
    match result {
        Ok(code) => Ok(code),
        Err(error) => match error.downcast_ref::<Refusal>() {
            Some(refusal) => {
                print_refusal(refusal, markdown)?;
                Ok(ExitCode::from(2))
            }
            None => Err(error),
        },
    }
}

fn execute(root: &Path, markdown: bool, command: ModelCmd) -> Result<()> {
    match command {
        ModelCmd::Init { name } => {
            let model = vault::init(root, &name)?;
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
        ModelCmd::Read {
            subject: None,
            flow: None,
        } => {
            let (model, state, pending) = load_for_read(root)?;
            let reading = model.read();
            let collections = model.collection_views();
            let active = state.as_deref().and_then(active_claims);
            if markdown {
                print_reading(&model, &reading);
                print_collections(&collections);
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
                "collections": collections,
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
        ModelCmd::Read { subject, flow } => {
            let (model, state, pending) = load_for_read(root)?;
            let found = match &flow {
                Some(flow) => {
                    let (collection, flow) = model.flow(flow)?;
                    Some(Found::Flow(collection, flow))
                }
                None => subject.as_deref().and_then(|s| model.find_grouping(s)),
            };
            if let Some(found) = found {
                let read = GroupingRead {
                    model: &model,
                    state,
                    pending,
                    markdown,
                };
                return read.print(found);
            }
            // The arm above reads the whole model, so a subject is given here.
            let subject = subject.unwrap_or_default();
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
            let dir = vault::locate(root)?;
            let (receipt, finished) = edit::run(&dir, &read_request(input.as_deref())?, &doing)?;
            if finished {
                warn_unloadable(&dir, &[&receipt]);
            }
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
            let dir = vault::locate(root)?;
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
            let dir = vault::locate(root)?;
            let completed = edit::recover(&dir)?;
            warn_unloadable(&dir, &completed.iter().collect::<Vec<_>>());
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
            let dir = vault::locate(root)?;
            let claim = claims::claim(&dir, &read_request(input.as_deref())?, &doing)?;
            print_claim_output(markdown, "claim", &dir, &claim)
        }
        ModelCmd::Release {
            input,
            revoke,
            writer,
            reason,
        } => {
            let dir = vault::locate(root)?;
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
            let dir = vault::locate(root)?;
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

/// A `model read` of one collection or flow.
struct GroupingRead<'a> {
    model: &'a Model,
    state: Option<PathBuf>,
    pending: Option<Value>,
    markdown: bool,
}

impl GroupingRead<'_> {
    /// Prints the collection or flow with the active claims holding it.
    fn print(self, found: Found<'_>) -> Result<()> {
        let model = self.model;
        let (subject, mut output) = match found {
            Found::Collection(collection) => {
                let view = model.collection_view(collection);
                if self.markdown {
                    print_collections(std::slice::from_ref(&view));
                }
                (
                    view.id.clone(),
                    json!({"kind": "read-collection", "collection": view}),
                )
            }
            Found::Flow(collection, flow) => {
                if self.markdown {
                    print_flow(model, collection, flow);
                }
                let view = model.flow_view(flow);
                let id = view.id.clone();
                let output = json!({"kind": "read-flow", "collection": model.qualify(&collection.id), "flow": view});
                (id, output)
            }
        };
        let active = self
            .state
            .as_deref()
            .and_then(active_claims)
            .map(|active| claims::holding(model, active, &[subject.as_str()]));
        if self.markdown {
            print_claims(active.as_deref());
            return Ok(());
        }
        output["schema"] = MODEL_SCHEMA.into();
        output["success"] = true.into();
        output["vault"] = json!(model.dir);
        output["revision"] = json!(model.revision);
        if let Some(active) = active {
            output["claims"] = json!(active);
        }
        if let Some(pending) = self.pending {
            output["pending_operation"] = pending;
        }
        write(None, &output)
    }
}

/// `model check`: exit 0 without warnings, 1 with any. Like `model read`, it names a pending edit.
/// The skips are reported only when the code check ran.
fn check(root: &Path, markdown: bool, only: Option<CheckOnly>) -> Result<ExitCode> {
    let (model, _, pending) = load_for_read(root)?;
    let mut diagnostics = if only == Some(CheckOnly::Code) {
        Vec::new()
    } else {
        semantic::check(&model.manifest.id, &model.modules, &model.collections)
    };
    let mut skips = None;
    if only != Some(CheckOnly::Semantic) {
        let report = code_check(&model)?;
        diagnostics.extend(report.diagnostics);
        skips = Some(report.skips);
    }
    diagnostics.sort();
    let warning = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    let information = diagnostics.len() - warning;
    let code = ExitCode::from(u8::from(warning > 0));
    if markdown {
        print_diagnostics(&diagnostics, skips.as_deref(), warning, information);
        return Ok(code);
    }
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "check",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
        "diagnostics": diagnostics,
        "counts": {"warning": warning, "information": information},
        "passed": warning == 0,
    });
    if let Some(skips) = skips {
        output["skips"] = json!(skips);
    }
    if let Some(pending) = pending {
        output["pending_operation"] = pending;
    }
    write(None, &output)?;
    Ok(code)
}

/// The code check against the model's checkout, scanning it once when a binding or anchor names
/// code there. A failed scan becomes a `SCAN_FAILED` warning.
fn code_check(model: &Model) -> Result<CodeReport> {
    // The vault is `<checkout>/.boundrail/model`.
    let checkout = model
        .dir
        .parent()
        .and_then(Path::parent)
        .context("the model vault has no checkout around it")?;
    let doc = if code::needs_scan(&model.modules, checkout) {
        super::scan_cli::live(checkout, None).map_err(|error| NoScan::Failed(format!("{error:#}")))
    } else {
        Err(NoScan::NotNeeded)
    };
    Ok(code::check(
        &model.manifest.id,
        &model.modules,
        &model.config,
        checkout,
        doc.as_ref().map_err(Clone::clone),
    ))
}

fn print_diagnostics(
    diagnostics: &[Diagnostic],
    skips: Option<&[Skip]>,
    warning: usize,
    information: usize,
) {
    for (severity, heading) in [
        (Severity::Warning, "Warnings:"),
        (Severity::Information, "Information:"),
    ] {
        let mut group = diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .peekable();
        if group.peek().is_some() {
            println!("{heading}");
        }
        for d in group {
            let suggestion = d
                .suggested_binding
                .as_ref()
                .map(|path| format!(" (suggested binding: {path})"))
                .unwrap_or_default();
            println!("- {} {}{suggestion}", d.code.as_str(), d.message);
        }
    }
    let mut skipped = BTreeMap::new();
    for skip in skips.unwrap_or_default() {
        *skipped.entry((skip.site, skip.reason)).or_insert(0) += 1;
    }
    if !skipped.is_empty() {
        println!("Skipped:");
    }
    for ((site, reason), count) in skipped {
        println!("- {} {}: {count}", site.as_str(), reason.as_str());
    }
    let verdict = if warning == 0 { "passed" } else { "failed" };
    println!("Check {verdict}: warnings: {warning}, information: {information}");
}

/// Warns on stderr when the model no longer loads after finishing interrupted edits: a newer
/// build may judge a record an older one planned otherwise. The edits themselves are complete.
fn warn_unloadable(dir: &Path, finished: &[&Receipt]) {
    if finished.is_empty() {
        return;
    }
    if let Err(error) = vault::load_dir(dir.to_path_buf()) {
        for receipt in finished {
            eprintln!(
                "warning: operation {} is complete, but the model does not load: {error:#}",
                receipt.operation_id
            );
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

/// The model for `model read` and `model check`, its machine-local state folder (`None` with a
/// warning on stderr) and the edit pending there. A `MODEL_INVALID` refusal also names the
/// pending edit.
fn load_for_read(root: &Path) -> Result<(Model, Option<PathBuf>, Option<Value>)> {
    let dir = vault::locate(root)?;
    let state = boundrail::arch::state_dir(&dir)
        .inspect_err(|error| eprintln!("warning: claims and pending edits omitted: {error:#}"))
        .ok();
    let pending = state.as_deref().and_then(pending_operation);
    let model = vault::load_dir(dir).map_err(|mut error| {
        if let (Some(pending), Some(refusal)) = (&pending, error.downcast_mut::<Refusal>())
            && refusal.code == Code::ModelInvalid
            && let Some(details) = refusal.details.as_mut()
        {
            details["pending_operation"] = pending.clone();
        }
        error
    })?;
    Ok((model, state, pending))
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

fn print_collections(collections: &[CollectionView<'_>]) {
    if collections.is_empty() {
        return;
    }
    println!("Collections:");
    for collection in collections {
        println!("- {} {}", collection.name, collection.id);
        for flow in &collection.flows {
            let summary = flow.summary.map(|s| format!(": {s}")).unwrap_or_default();
            println!("  - {} {}{summary}", flow.name, flow.id);
        }
    }
}

/// The flow as a numbered sequence, decisions and loops indenting the steps inside them.
fn print_flow(model: &Model, collection: &Collection, flow: &Flow) {
    println!(
        "Flow {} {} in {}",
        flow.name,
        model.qualify(&flow.id),
        collection.name
    );
    if let Some(summary) = &flow.summary {
        println!("{summary}");
    }
    print_steps(&names(model), &flow.steps, 0);
}

fn print_steps(names: &BTreeMap<String, &str>, steps: &[Step], depth: usize) {
    let indent = "  ".repeat(depth);
    for (n, step) in (1..).zip(steps) {
        match step {
            Step::Message {
                from,
                to,
                label,
                reply,
                repeat,
                ..
            } => {
                let repeat = repeat.map(|r| format!(" x{r}")).unwrap_or_default();
                let reply = if *reply { " (reply)" } else { "" };
                let (from, to) = (name_of(names, from), name_of(names, to));
                println!("{indent}{n}. {from} -> {to}: {label}{repeat}{reply}");
            }
            Step::Decision {
                label, branches, ..
            } => {
                println!("{indent}{n}. decision: {label}");
                for branch in branches {
                    println!("{indent}  - {}:", branch.label);
                    print_steps(names, &branch.steps, depth + 2);
                }
            }
            Step::Loop { label, steps, .. } => {
                println!("{indent}{n}. loop: {label}");
                print_steps(names, steps, depth + 1);
            }
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
