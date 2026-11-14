use super::*;
use std::io::Read as _;
use std::iter;

use boundrail::arch::checkpoint::{self, Checkpoint};
use boundrail::arch::claims::{self, Claim};
use boundrail::arch::code::{self, CodeReport, NoScan, Skip, SkipReason, UnitStatus};
use boundrail::arch::compare::{self, Comparison};
use boundrail::arch::diagnostic::{Diagnostic, Severity};
use boundrail::arch::edit::{self, Receipt};
use boundrail::arch::footprint::{self, CrossingsSkipped, FootprintReport, Range};
use boundrail::arch::infra::{self, Infra};
use boundrail::arch::inspect::{self, InspectReport};
use boundrail::arch::locality::{self, History, LocalityReport};
use boundrail::arch::lsp::{self, Facts};
use boundrail::arch::proposal::{self, CURRENT, Writing};
use boundrail::arch::render::{self, View};
use boundrail::arch::request::INPUT_MAX;
use boundrail::arch::semantic;
use boundrail::arch::types::{
    self, BeatReading, Collection, Flow, ReadingState, ReadingView, SCHEMAS, Step, Walkthrough,
};
use boundrail::arch::vault::{
    self, CollectionView, Found, Model, ModuleView, RelationshipView, Scope, WalkthroughEntry,
};
use boundrail::arch::{Code, Refusal};
use serde_json::{Value, json};

const MODEL_SCHEMA: &str = "boundrail/model/v1";

#[derive(Args)]
pub(super) struct ModelArgs {
    /// Directory inside the Git checkout that holds the model
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    /// Output format; JSON when absent, except `model render`, which then prints the SVG
    #[arg(long, global = true, value_enum)]
    format: Option<Format>,
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
        /// Check this proposal instead of the current state
        #[arg(long)]
        proposal: Option<String>,
        /// Run the language servers .boundrail/language-servers.json names, for call references and symbols.
        /// They run code from the checkout (a workspace tsserver.js under node_modules,
        /// rust-analyzer build scripts and proc macros, jdtls Gradle and Maven imports): use it on
        /// trusted checkouts only, never on untrusted pull request heads in CI. The servers may
        /// leave build output and project metadata in the checkout (rust-analyzer target/, jdtls
        /// .project, .classpath and .settings), while Boundrail itself writes nothing there
        #[arg(long)]
        language_servers: bool,
    },
    /// Draw the model as SVG: the architecture, or a flow as a sequence diagram or flowchart,
    /// optionally marked with what it changes against another state
    Render(RenderArgs),
    /// Serve a live, read-only view of the model in the browser on a local port
    View(super::view_cli::ViewArgs),
}

#[derive(Args)]
struct RenderArgs {
    #[command(flatten)]
    spec: RenderSpec,
    /// Write to this file instead of standard output; its folder must exist
    #[arg(long)]
    output: Option<PathBuf>,
}

/// What `model render` draws, as the command line and the live view ask for it.
#[derive(Args, Default)]
pub(super) struct RenderSpec {
    /// Draw a beat of this walkthrough, as its reading says: its ID or unique name; needs --beat
    /// and no other flag that says what to draw
    #[arg(long)]
    pub(super) walkthrough: Option<String>,
    /// The beat of --walkthrough to draw: its number, from 1, or its ID
    #[arg(long, requires = "walkthrough")]
    pub(super) beat: Option<String>,
    /// Draw this proposal, marked against the current state unless --against names another
    #[arg(long)]
    pub(super) proposal: Option<String>,
    /// Draw the model a checkpoint holds, or with --against compare it: its ID or a unique
    /// commit prefix
    #[arg(long)]
    pub(super) checkpoint: Option<String>,
    /// What to draw; a flow's sequence diagram when --flow is given, else the architecture
    #[arg(long, value_enum)]
    pub(super) view: Option<RenderView>,
    /// The flow a sequence or flowchart view draws: its qualified or local ID, or its unique name
    #[arg(long)]
    pub(super) flow: Option<String>,
    /// Draw only this module's parts: its ID or unique current or former name
    #[arg(long)]
    pub(super) scope: Option<String>,
    /// How many levels of containers the architecture view opens; deeper parts collapse
    /// into their container [default: 2]
    #[arg(long)]
    pub(super) depth: Option<usize>,
    /// Compare with the current state or this proposal; the newer state (checkpoint, then
    /// current, then proposal) is drawn with what it adds, removes and changes marked
    #[arg(long)]
    pub(super) against: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum RenderView {
    Architecture,
    Sequence,
    Flowchart,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum CheckOnly {
    Semantic,
    Code,
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Create the model vault in .boundrail/model at the checkout root
    Init {
        #[arg(long)]
        name: String,
        /// Start without a current state: plan in proposals until the first adoption
        #[arg(long)]
        planning: bool,
    },
    /// Print the model, one module with its parts and relationships, a collection, a flow, or a
    /// walkthrough
    Read {
        /// Qualified ID, local ID, or a unique current or former module name; or a collection,
        /// flow or walkthrough ID
        #[arg(conflicts_with_all = ["flow", "walkthrough"])]
        subject: Option<String>,
        /// Print one flow with its steps: its qualified or local ID, or its unique name
        #[arg(long, conflicts_with = "walkthrough")]
        flow: Option<String>,
        /// Print one walkthrough with its beats: its qualified or local ID, or its unique name;
        /// Markdown prints it as a script
        #[arg(long)]
        walkthrough: Option<String>,
        /// Read the model a checkpoint holds: its ID or a unique commit prefix
        #[arg(long)]
        checkpoint: Option<String>,
        /// Read a proposal instead of the current state: its ID
        #[arg(long, conflicts_with = "checkpoint")]
        proposal: Option<String>,
    },
    /// List the vault file and request schemas, or print one as JSON Schema
    Schema { name: Option<String> },
    /// Apply a batch of module, relationship, collection, flow, step, walkthrough and beat changes (schema `model-edit`)
    Edit {
        /// Request file; `-` or absent reads standard input
        #[arg(long)]
        input: Option<PathBuf>,
        /// One line saying why the model changes, 1 to 140 characters
        #[arg(long)]
        doing: String,
        /// Edit this proposal instead of the current state: its ID; the request's target state
        /// and expected revision are the proposal's
        #[arg(long)]
        proposal: Option<String>,
    },
    /// Draw a proposal: a draft of a future architecture, against the current state
    Propose {
        #[arg(long)]
        name: String,
        /// Start from the current state, or copy this proposal
        #[arg(long, default_value = CURRENT)]
        from: String,
        #[arg(long)]
        description: Option<String>,
        #[command(flatten)]
        writing: WritingArgs,
    },
    /// List proposals with their status
    Proposals,
    /// Report what a proposal adds, removes and changes against the current state or another proposal
    Compare {
        /// The proposal's ID
        proposal: String,
        /// Compare against the current state, or this proposal
        #[arg(long, default_value = CURRENT)]
        against: String,
    },
    /// Make a proposal the current state
    Adopt {
        /// The proposal's ID
        proposal: String,
        /// The current state's revision digest the adoption is for
        #[arg(long)]
        expected_revision: String,
        /// The proposal's revision digest you compared; another revision refuses
        #[arg(long)]
        expected_proposal_revision: String,
        #[command(flatten)]
        writing: WritingArgs,
        /// Your claim covering what the adoption changes
        #[arg(long)]
        claim: Option<String>,
    },
    /// Set a proposal aside; it stays, reading only
    Shelve {
        /// The proposal's ID
        proposal: String,
        /// The proposal's revision digest, as `model read --proposal` shows it; another refuses
        #[arg(long)]
        expected_revision: String,
        #[command(flatten)]
        writing: WritingArgs,
        /// Your claim covering the proposal
        #[arg(long)]
        claim: Option<String>,
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
    /// Retain the current model as a checkpoint on the history ref, with why it changed
    Retain {
        /// One line saying why the model changed, 1 to 140 characters
        #[arg(long)]
        doing: String,
        /// Who retains it; defaults to the Git author name
        #[arg(long)]
        writer: Option<String>,
        /// Repeating an operation ID with the same writer, --doing and --expected-revision returns
        /// the same checkpoint
        #[arg(long)]
        operation_id: Option<String>,
        /// The revision digest the retain is for; a model at another revision refuses
        #[arg(long)]
        expected_revision: Option<String>,
    },
    /// List checkpoints, newest first
    Checkpoints {
        /// List at most this many
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Report each bound module's interface from a scan: entry points, exported, used and unused
    /// symbols, imports past its entry points, and import edges per neighbouring module
    Inspect {
        /// One module: its ID or unique current or former name
        subject: Option<String>,
        /// Inspect this proposal instead of the current state
        #[arg(long)]
        proposal: Option<String>,
        /// Run the language servers .boundrail/language-servers.json names, for references to each interface symbol.
        /// They run code from the checkout (a workspace tsserver.js under node_modules,
        /// rust-analyzer build scripts and proc macros, jdtls Gradle and Maven imports): use it on
        /// trusted checkouts only, never on untrusted pull request heads in CI. The servers may
        /// leave build output and project metadata in the checkout (rust-analyzer target/, jdtls
        /// .project, .classpath and .settings), while Boundrail itself writes nothing there
        #[arg(long)]
        language_servers: bool,
    },
    /// Report from Git history how often commits stay inside each module, which modules change
    /// together, and the pairs that do so with no relationship drawn
    Locality {
        /// Read only commits after this revision
        #[arg(long)]
        since: Option<String>,
        /// Read at most this many commits
        #[arg(long, default_value_t = boundrail::cochange::HISTORY_LIMIT)]
        limit: usize,
    },
    /// Report the modules a branch changes, the relationships its changed imports cross, and
    /// what it changes in the model
    Footprint {
        /// The revision the branch starts from [default: the merge-base of HEAD with
        /// origin/HEAD, main or master, else HEAD]
        #[arg(long)]
        base: Option<String>,
        /// The branch's last commit [default: HEAD with the working tree]
        #[arg(long)]
        head: Option<String>,
    },
}

/// What every proposal write names.
#[derive(Args)]
struct WritingArgs {
    /// One line saying why, 1 to 140 characters
    #[arg(long)]
    doing: String,
    /// Who writes; defaults to the Git author name
    #[arg(long)]
    writer: Option<String>,
    /// Repeating an operation ID with the same arguments returns the same receipt
    #[arg(long)]
    operation_id: String,
}

impl WritingArgs {
    fn writing<'a>(&'a self, claim: Option<&'a str>) -> Writing<'a> {
        Writing {
            doing: &self.doing,
            writer: self.writer.as_deref(),
            operation_id: &self.operation_id,
            claim,
        }
    }
}

/// A refusal is printed on stdout and exits with code 2; other errors go to main's handler.
pub(super) fn run(args: ModelArgs) -> Result<ExitCode> {
    let markdown = matches!(args.format, Some(Format::Markdown));
    let result = match args.command {
        Command::Check {
            only,
            proposal,
            language_servers,
        } => check(
            &args.root,
            markdown,
            only,
            proposal.as_deref(),
            language_servers,
        ),
        Command::Render(render) => {
            render_model(&args.root, args.format, &render).map(|()| ExitCode::SUCCESS)
        }
        Command::View(view) => super::view_cli::run(&args.root, &view).map(|()| ExitCode::SUCCESS),
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
        ModelCmd::Init { name, planning } => {
            let model = vault::init(root, &name, planning)?;
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
            walkthrough: None,
            checkpoint,
            proposal,
        } => {
            let (checkpoint, proposal) = (checkpoint.as_deref(), proposal.as_deref());
            if !markdown {
                return write(None, &read_output(root, checkpoint, proposal)?);
            }
            let Loaded {
                model,
                state,
                checkpoint,
                ..
            } = load_reading(root, checkpoint, proposal)?;
            if let Some(checkpoint) = &checkpoint {
                print_checkpoint(checkpoint);
            }
            print_proposal(&model);
            print_reading(&model, &model.read());
            print_collections(&model.collection_views());
            print_walkthroughs(&model.walkthrough_entries());
            print_claims(state.as_deref().and_then(active_claims).as_deref());
            Ok(())
        }
        ModelCmd::Read {
            subject,
            flow,
            walkthrough,
            checkpoint,
            proposal,
        } => {
            let Loaded {
                model,
                state,
                pending,
                checkpoint,
            } = load_reading(root, checkpoint.as_deref(), proposal.as_deref())?;
            let found = match (&flow, &walkthrough) {
                (Some(flow), _) => {
                    let (collection, flow) = model.flow(flow)?;
                    Some(Found::Flow(collection, flow))
                }
                (None, Some(walkthrough)) => {
                    Some(Found::Walkthrough(model.walkthrough(walkthrough)?))
                }
                (None, None) => subject.as_deref().and_then(|s| model.find_grouping(s)),
            };
            if let Some(found) = found {
                let read = GroupingRead {
                    model: &model,
                    state,
                    pending,
                    checkpoint,
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
                if let Some(checkpoint) = &checkpoint {
                    print_checkpoint(checkpoint);
                }
                print_proposal(&model);
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
            extend(&mut output, json!({ "proposal": model.proposal }))?;
            if let Some(active) = active {
                output["claims"] = json!(active);
            }
            if let Some(pending) = pending {
                output["pending_operation"] = pending;
            }
            if let Some(checkpoint) = checkpoint {
                output["checkpoint"] = json!(checkpoint);
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
        ModelCmd::Edit {
            input,
            doing,
            proposal,
        } => {
            let dir = vault::locate(root)?;
            let request = read_request(input.as_deref())?;
            let (receipt, finished) = edit::run(&dir, &request, &doing, proposal.as_deref())?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            print_written(markdown, "edit", &dir, &receipt, None)
        }
        ModelCmd::Propose {
            name,
            from,
            description,
            writing,
        } => {
            let dir = vault::locate(root)?;
            let writing = writing.writing(None);
            let (receipt, finished) =
                proposal::propose(&dir, &writing, &name, Some(&from), description.as_deref())?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            print_written(markdown, "propose", &dir, &receipt, None)
        }
        ModelCmd::Adopt {
            proposal,
            expected_revision,
            expected_proposal_revision,
            writing,
            claim,
        } => {
            let dir = vault::locate(root)?;
            let writing = writing.writing(claim.as_deref());
            let compared = &expected_proposal_revision;
            let (receipt, finished) =
                proposal::adopt(&dir, &writing, &proposal, &expected_revision, compared)?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            // The receipt's revisions cover the proposal too.
            let current = proposal::current_revision(&receipt);
            print_written(markdown, "adopt", &dir, &receipt, Some(&current))
        }
        ModelCmd::Shelve {
            proposal,
            expected_revision,
            writing,
            claim,
        } => {
            let dir = vault::locate(root)?;
            let writing = writing.writing(claim.as_deref());
            let (receipt, finished) =
                proposal::shelve(&dir, &writing, &proposal, &expected_revision)?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            print_written(markdown, "shelve", &dir, &receipt, None)
        }
        ModelCmd::Proposals => list_proposals(root, markdown),
        ModelCmd::Compare { proposal, against } => {
            compare_proposal(root, markdown, &proposal, &against)
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
            let finished: Vec<_> = completed.iter().map(|c| (&c.scope, &c.receipt)).collect();
            warn_unloadable(&dir, &finished);
            if markdown {
                if completed.is_empty() {
                    println!("No interrupted edits");
                }
                for c in &completed {
                    print_receipt(&label(&c.kind), &c.receipt);
                }
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
        ModelCmd::Retain {
            doing,
            writer,
            operation_id,
            expected_revision,
        } => {
            let dir = vault::locate(root)?;
            let checkpoint = checkpoint::retain(
                &dir,
                &doing,
                writer.as_deref(),
                operation_id.as_deref(),
                expected_revision.as_deref(),
            )?;
            if markdown {
                print_checkpoint(&checkpoint);
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "retain",
                    "success": true,
                    "vault": dir,
                    "checkpoint": checkpoint,
                }),
            )
        }
        ModelCmd::Inspect {
            subject,
            proposal,
            language_servers,
        } => {
            let (subject, proposal) = (subject.as_deref(), proposal.as_deref());
            inspect_model(root, markdown, subject, proposal, language_servers)
        }
        ModelCmd::Locality { since, limit } => {
            model_locality(root, markdown, since.as_deref(), limit)
        }
        ModelCmd::Footprint { base, head } => {
            model_footprint(root, markdown, base.as_deref(), head.as_deref())
        }
        ModelCmd::Checkpoints { limit } => {
            if !markdown {
                return write(None, &checkpoints_output(root, limit)?);
            }
            // Outside a checkout this refuses, as the JSON output does by naming the vault.
            let _vault = vault::path(root)?;
            let list = checkpoint::list(root, limit)?;
            if list.is_empty() {
                println!("No checkpoints");
            }
            list.iter().for_each(print_checkpoint);
            Ok(())
        }
    }
}

/// The JSON output of `model read` of the whole model: the current state, a proposal or a
/// checkpoint.
pub(super) fn read_output(
    root: &Path,
    checkpoint: Option<&str>,
    proposal: Option<&str>,
) -> Result<Value> {
    let Loaded {
        model,
        state,
        pending,
        checkpoint,
    } = load_reading(root, checkpoint, proposal)?;
    let reading = model.read();
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
        "collections": model.collection_views(),
        "walkthroughs": model.walkthrough_entries(),
        "revision": model.revision,
    });
    extend(&mut output, json!({ "proposal": model.proposal }))?;
    if let Some(active) = state.as_deref().and_then(active_claims) {
        output["claims"] = json!(active);
    }
    if let Some(pending) = pending {
        output["pending_operation"] = pending;
    }
    if let Some(checkpoint) = checkpoint {
        output["checkpoint"] = json!(checkpoint);
    }
    Ok(output)
}

/// A `model read` of one collection or flow.
struct GroupingRead<'a> {
    model: &'a Model,
    state: Option<PathBuf>,
    pending: Option<Value>,
    checkpoint: Option<Checkpoint>,
    markdown: bool,
}

impl GroupingRead<'_> {
    /// Prints the collection, flow or walkthrough with the active claims holding it.
    fn print(self, found: Found<'_>) -> Result<()> {
        match self.output(found)? {
            Some(output) => write(None, &output),
            None => Ok(()),
        }
    }

    /// The JSON output of reading `found`, or `None` when it printed Markdown instead.
    fn output(self, found: Found<'_>) -> Result<Option<Value>> {
        let model = self.model;
        if self.markdown {
            if let Some(checkpoint) = &self.checkpoint {
                print_checkpoint(checkpoint);
            }
            print_proposal(model);
        }
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
            Found::Walkthrough(walkthrough) => {
                if self.markdown {
                    print_walkthrough(model, walkthrough);
                }
                let view = model.walkthrough_view(walkthrough);
                let id = view.id.clone();
                (id, json!({"kind": "read-walkthrough", "walkthrough": view}))
            }
        };
        let active = self
            .state
            .as_deref()
            .and_then(active_claims)
            .map(|active| claims::holding(model, active, &[subject.as_str()]));
        if self.markdown {
            print_claims(active.as_deref());
            return Ok(None);
        }
        output["schema"] = MODEL_SCHEMA.into();
        output["success"] = true.into();
        output["vault"] = json!(model.dir);
        output["revision"] = json!(model.revision);
        extend(&mut output, json!({ "proposal": model.proposal }))?;
        if let Some(active) = active {
            output["claims"] = json!(active);
        }
        if let Some(pending) = self.pending {
            output["pending_operation"] = pending;
        }
        if let Some(checkpoint) = self.checkpoint {
            output["checkpoint"] = json!(checkpoint);
        }
        Ok(Some(output))
    }
}

/// The JSON output of `model read --walkthrough`.
pub(super) fn walkthrough_output(root: &Path, subject: &str) -> Result<Value> {
    let Loaded {
        model,
        state,
        pending,
        checkpoint,
    } = load_reading(root, None, None)?;
    let read = GroupingRead {
        model: &model,
        state,
        pending,
        checkpoint,
        markdown: false,
    };
    let output = read.output(Found::Walkthrough(model.walkthrough(subject)?))?;
    output.context("a JSON reading returns its output")
}

/// `model render`: the SVG on stdout or in `--output`; with `--format json`, inside the output
/// envelope.
fn render_model(root: &Path, format: Option<Format>, args: &RenderArgs) -> Result<()> {
    if matches!(format, Some(Format::Markdown)) {
        let message = "model render prints SVG, or JSON with --format json";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    let output = args.output.as_deref();
    let (svg, view) = render_svg(root, &args.spec, output)?;
    if matches!(format, Some(Format::Json)) {
        let envelope = json!({
            "schema": MODEL_SCHEMA,
            "kind": "render",
            "success": true,
            "view": view,
            "svg": svg,
        });
        return write(output, &envelope);
    }
    match output {
        Some(path) => write_file(path, svg.as_bytes()),
        None => {
            print!("{svg}");
            Ok(())
        }
    }
}

/// The SVG of what `spec` asks for, with the name of the view drawn, refusing an `output` it
/// could not write to. A walkthrough beat is drawn as its reading says, with its focus.
pub(super) fn render_svg(
    root: &Path,
    spec: &RenderSpec,
    output: Option<&Path>,
) -> Result<(String, &'static str)> {
    if spec.walkthrough.is_none() && spec.beat.is_none() {
        return draw(root, spec, &[], output, None);
    }
    let current = load_state(root, RenderState::Current)?;
    let (reading, focus) = spec.beat(&current.0)?;
    draw(root, &reading, &focus, output, Some(current))
}

/// The SVG of what `spec` asks for with `focus` highlighted, and the name of the view drawn.
/// `current` is the current state when the caller already loaded it.
fn draw(
    root: &Path,
    spec: &RenderSpec,
    focus: &[String],
    output: Option<&Path>,
    mut current: Option<(Model, Option<String>)>,
) -> Result<(String, &'static str)> {
    let view = spec.view()?;
    let (drawn, base) = spec.states()?;
    let mut load = |state: RenderState<'_>| match (state, current.take()) {
        (RenderState::Current, Some(loaded)) => Ok(loaded),
        (_, loaded) => {
            current = loaded;
            load_state(root, state)
        }
    };
    let (model, name) = load(drawn)?;
    let base = base.map(load).transpose()?;
    // A proposal's local and qualified IDs name the same state.
    let id = |model: &Model| model.proposal.as_ref().map(|p| p.id.clone());
    if let Some((base, _)) = &base
        && id(&model).is_some()
        && id(&model) == id(base)
    {
        let message = "--against names the state being drawn";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    if let Some(output) = output {
        check_output(root, output)?;
    }
    let drawn = render::Side {
        model: &model,
        name,
    };
    let base = (base.as_ref()).map(|(model, name)| render::Side {
        model,
        name: name.clone(),
    });
    let svg = render::render(&drawn, base.as_ref(), &view, focus)?;
    Ok((svg, view.name()))
}

/// What a beat's reading draws, as the flags would ask for it.
fn reading_spec(reading: &BeatReading) -> RenderSpec {
    let state = |state: &ReadingState| match state {
        ReadingState::Current => CURRENT.to_owned(),
        ReadingState::Proposal(id) | ReadingState::Checkpoint(id) => id.clone(),
    };
    RenderSpec {
        proposal: match &reading.state {
            ReadingState::Proposal(p) => Some(p.clone()),
            _ => None,
        },
        checkpoint: match &reading.state {
            ReadingState::Checkpoint(c) => Some(c.clone()),
            _ => None,
        },
        view: Some(match reading.view {
            ReadingView::Architecture => RenderView::Architecture,
            ReadingView::Sequence => RenderView::Sequence,
            ReadingView::Flowchart => RenderView::Flowchart,
        }),
        flow: reading.flow.clone(),
        scope: reading.scope.clone(),
        depth: reading.depth.and_then(|d| usize::try_from(d).ok()),
        against: reading.against.as_ref().map(state),
        ..RenderSpec::default()
    }
}

/// A state `model render` draws or compares with, oldest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RenderState<'a> {
    Checkpoint(&'a str),
    Current,
    Proposal(&'a str),
}

/// The model `state` holds, and how a title names it: `None` for the current state.
fn load_state(root: &Path, state: RenderState<'_>) -> Result<(Model, Option<String>)> {
    let (checkpoint, proposal) = match state {
        RenderState::Checkpoint(c) => (Some(c), None),
        RenderState::Current => (None, None),
        RenderState::Proposal(p) => (None, Some(p)),
    };
    let Loaded {
        model, checkpoint, ..
    } = load_reading(root, checkpoint, proposal)?;
    let name = match (&checkpoint, &model.proposal) {
        (Some(c), _) => Some(format!("checkpoint {}", c.id)),
        (None, Some(p)) => Some(format!("proposal {}", p.name)),
        (None, None) => None,
    };
    Ok((model, name))
}

impl RenderSpec {
    /// What the beat `--beat` of `--walkthrough` reads in `model`, the current state, as flags,
    /// with its focus. Refuses `REQUEST_INVALID` without both flags or with any other flag that
    /// says what to draw.
    fn beat(&self, model: &Model) -> Result<(RenderSpec, Vec<String>)> {
        let invalid = |message: &str| Refusal::new(Code::RequestInvalid, message);
        let (walkthrough, beat) = match (&self.walkthrough, &self.beat) {
            (Some(walkthrough), Some(beat)) => (walkthrough, beat),
            (None, _) => return Err(invalid("--beat needs --walkthrough").into()),
            (_, None) => return Err(invalid("--walkthrough needs --beat").into()),
        };
        let drawing = [
            &self.proposal,
            &self.checkpoint,
            &self.flow,
            &self.scope,
            &self.against,
        ];
        if drawing.iter().any(|flag| flag.is_some()) || self.view.is_some() || self.depth.is_some()
        {
            let message = "--walkthrough draws what its beat reads; leave out the other flags";
            return Err(invalid(message).into());
        }
        let walkthrough = model.walkthrough(walkthrough)?;
        let by_number = (beat.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| walkthrough.beats.get(n));
        let found = by_number.or_else(|| {
            (walkthrough.beats.iter()).find(|b| b.id == *beat || model.qualify(&b.id) == *beat)
        });
        let found = found.ok_or_else(|| {
            let id = model.qualify(&walkthrough.id);
            let message =
                format!("walkthrough {id} has no beat `{beat}`; name one by number or ID");
            Refusal::new(Code::SubjectNotFound, message)
        })?;
        Ok((reading_spec(&found.reading), found.reading.focus.clone()))
    }

    /// The state to draw and the one to compare it with. The newer of the two is drawn; of two
    /// proposals, the one `--proposal` names. A proposal alone is compared with the current state.
    fn states(&self) -> Result<(RenderState<'_>, Option<RenderState<'_>>)> {
        let invalid = |message: &str| Refusal::new(Code::RequestInvalid, message);
        let named = match (self.proposal.as_deref(), self.checkpoint.as_deref()) {
            (Some(_), Some(_)) => {
                let message = "--proposal and --checkpoint each name a state; give one";
                return Err(invalid(message).into());
            }
            (Some(p), None) => RenderState::Proposal(p),
            (None, Some(c)) => RenderState::Checkpoint(c),
            (None, None) => RenderState::Current,
        };
        let other = match self.against.as_deref() {
            Some(CURRENT) => Some(RenderState::Current),
            Some(p) => Some(RenderState::Proposal(p)),
            None => matches!(named, RenderState::Proposal(_)).then_some(RenderState::Current),
        };
        let Some(other) = other else {
            return Ok((named, None));
        };
        if other == named {
            return Err(invalid("--against names the state being drawn").into());
        }
        Ok(match (named, other) {
            (RenderState::Proposal(_), RenderState::Proposal(_)) => (named, Some(other)),
            _ if other > named => (other, Some(named)),
            _ => (named, Some(other)),
        })
    }

    /// The view the flags ask for, refusing `REQUEST_INVALID` for flags that do not go together.
    fn view(&self) -> Result<View<'_>> {
        let invalid = |message: &str| Refusal::new(Code::RequestInvalid, message);
        let flow = self.flow.as_deref();
        let kind = self.view.unwrap_or(match flow {
            Some(_) => RenderView::Sequence,
            None => RenderView::Architecture,
        });
        let architecture_only = self.scope.is_some() || self.depth.is_some();
        match (kind, flow) {
            (RenderView::Architecture, None) => Ok(View::Architecture {
                scope: self.scope.as_deref(),
                depth: self.depth.unwrap_or(render::DEFAULT_DEPTH),
            }),
            (RenderView::Architecture, Some(_)) => Err(invalid(
                "--flow draws a sequence or flowchart view, not the architecture",
            )
            .into()),
            (_, None) => Err(invalid("the sequence and flowchart views need --flow").into()),
            (_, Some(_)) if architecture_only => {
                Err(invalid("--scope and --depth apply to the architecture view only").into())
            }
            (RenderView::Sequence, Some(flow)) => Ok(View::Sequence(flow)),
            (RenderView::Flowchart, Some(flow)) => Ok(View::Flowchart(flow)),
        }
    }
}

/// Refuses an `--output` whose folder is missing or lies inside the model vault.
fn check_output(root: &Path, output: &Path) -> Result<()> {
    let parent = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let folder = fs::canonicalize(parent).map_err(|error| {
        Refusal::new(
            Code::RequestInvalid,
            format!(
                "--output {}: its folder cannot be read: {error}",
                output.display()
            ),
        )
    })?;
    let vault = vault::path(root).and_then(|dir| Ok(fs::canonicalize(dir)?));
    if vault.is_ok_and(|vault| folder.starts_with(vault)) {
        return Err(Refusal::new(
            Code::RequestInvalid,
            format!(
                "--output {} is inside the model vault; write it elsewhere",
                output.display()
            ),
        )
        .into());
    }
    Ok(())
}

/// `model check`: exit 0 without warnings, 1 with any. Like `model read`, it names a pending edit.
/// The skips are reported only when the code check ran.
fn check(
    root: &Path,
    markdown: bool,
    only: Option<CheckOnly>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<ExitCode> {
    let checked = run_check(root, only, proposal, language_servers)?;
    let code = ExitCode::from(u8::from(checked.warning > 0));
    if markdown {
        let Checked {
            model,
            diagnostics,
            skips,
            units,
            warning,
            information,
            ..
        } = &checked;
        print_proposal(model);
        let skipped = Skipped {
            skips: skips.as_deref().unwrap_or_default(),
            units: units.as_deref().unwrap_or_default(),
        };
        print_diagnostics(diagnostics, skipped, *warning, *information);
        return Ok(code);
    }
    write(None, &checked.output()?)?;
    Ok(code)
}

/// What `model check` found.
pub(super) struct Checked {
    model: Model,
    diagnostics: Vec<Diagnostic>,
    /// `None` when the code check did not run.
    skips: Option<Vec<Skip>>,
    /// The infrastructure units with their owners; `None` when the code check did not run.
    units: Option<Vec<UnitStatus>>,
    pending: Option<Value>,
    warning: usize,
    information: usize,
}

/// Runs the checks `only` names, or both, on the current state or `proposal`, the code check
/// with the configured language servers when `language_servers` is set.
pub(super) fn run_check(
    root: &Path,
    only: Option<CheckOnly>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<Checked> {
    let (model, _, pending) = load_for_read(root, proposal)?;
    let mut diagnostics = if only == Some(CheckOnly::Code) {
        Vec::new()
    } else {
        let mut found = semantic::check(&model.manifest.id, &model.modules, &model.collections);
        found.extend(semantic::walkthroughs(&model));
        found
    };
    let (mut skips, mut units) = (None, None);
    if only != Some(CheckOnly::Semantic) {
        let report = code_check(&model, language_servers)?;
        diagnostics.extend(report.diagnostics);
        skips = Some(report.skips);
        units = Some(report.units);
    }
    diagnostics.sort();
    let warning = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    let information = diagnostics.len() - warning;
    Ok(Checked {
        model,
        diagnostics,
        skips,
        units,
        pending,
        warning,
        information,
    })
}

impl Checked {
    /// The JSON output of `model check`.
    pub(super) fn output(self) -> Result<Value> {
        let mut output = json!({
            "schema": MODEL_SCHEMA,
            "kind": "check",
            "success": true,
            "vault": self.model.dir,
            "revision": self.model.revision,
            "diagnostics": self.diagnostics,
            "counts": {"warning": self.warning, "information": self.information},
            "passed": self.warning == 0,
        });
        if let Some(skips) = self.skips {
            output["skips"] = json!(skips);
        }
        if let Some(units) = self.units {
            output["units"] = json!(units);
        }
        extend(&mut output, json!({ "proposal": self.model.proposal }))?;
        if let Some(pending) = self.pending {
            output["pending_operation"] = pending;
        }
        Ok(output)
    }
}

/// The model's checkout and its scan, made only when a binding or anchor names code there.
fn scan_checkout(model: &Model) -> Result<(PathBuf, Result<Document, NoScan>)> {
    let checkout = checkpoint::checkout(&model.dir)?;
    let doc = if code::needs_scan(&model.modules, &checkout) {
        super::scan_cli::live(&checkout, None).map_err(|error| NoScan::Failed(format!("{error:#}")))
    } else {
        Err(NoScan::NotNeeded)
    };
    Ok((checkout, doc))
}

/// The error of a failed scan, for the output of a measure that went without it.
fn scan_error(doc: &Result<Document, NoScan>) -> Option<&str> {
    match doc {
        Err(NoScan::Failed(error)) => Some(error),
        _ => None,
    }
}

/// The infrastructure of the checkout. A read that fails warns and leaves it empty with the
/// error, so the modules bound to units go unjudged as after a failed scan.
fn read_infra(checkout: &Path) -> Infra {
    infra::read(checkout).unwrap_or_else(|error| {
        let error = format!("{error:#}");
        eprintln!("warning: reading the checkout's infrastructure failed: {error}");
        Infra::failed(error)
    })
}

/// The code check against the model's checkout and its infrastructure, scanning it once when a
/// binding or anchor names code there. A failed scan or infrastructure read becomes a
/// `SCAN_FAILED` warning.
fn code_check(model: &Model, language_servers: bool) -> Result<CodeReport> {
    let (checkout, doc) = scan_checkout(model)?;
    let checkout = checkout.as_path();
    let infra = read_infra(checkout);
    let lsp = language_server_facts(model, checkout, &doc, &infra, language_servers)?;
    Ok(code::check(
        &model.manifest.id,
        &model.modules,
        &model.config,
        checkout,
        doc.as_ref().map_err(Clone::clone),
        &infra,
        &lsp,
    ))
}

/// What the configured language servers find when `enabled` and the scan ran; otherwise
/// nothing, with a `language-servers-not-enabled` skip when servers are configured. An invalid
/// config refuses only when `enabled`.
fn language_server_facts(
    model: &Model,
    checkout: &Path,
    doc: &Result<Document, NoScan>,
    infra: &Infra,
    enabled: bool,
) -> Result<Facts> {
    let (servers, skips) = lsp::configured(checkout, enabled)?;
    let mut facts = match doc {
        Ok(doc) => {
            let (id, modules) = (&model.manifest.id, &model.modules);
            lsp::gather(checkout, &servers, id, modules, doc, infra)
        }
        Err(_) => Facts::default(),
    };
    facts.skips.extend(skips);
    Ok(facts)
}

/// `model inspect`: each bound module's interface, or one module's.
fn inspect_model(
    root: &Path,
    markdown: bool,
    subject: Option<&str>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<()> {
    if !markdown {
        return write(
            None,
            &inspect_output(root, subject, proposal, language_servers)?,
        );
    }
    let Inspected { model, report, .. } = inspected(root, subject, proposal, language_servers)?;
    print_proposal(&model);
    print_inspect(&model, &report);
    Ok(())
}

/// The JSON output of `model inspect`.
pub(super) fn inspect_output(
    root: &Path,
    subject: Option<&str>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<Value> {
    let Inspected {
        model,
        report,
        scan_error,
    } = inspected(root, subject, proposal, language_servers)?;
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "inspect",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
    });
    extend(&mut output, &report)?;
    extend(&mut output, json!({ "proposal": model.proposal }))?;
    if let Some(error) = scan_error {
        output["scan_error"] = json!(error);
    }
    Ok(output)
}

/// What `model inspect` measured.
struct Inspected {
    model: Model,
    report: InspectReport,
    /// The error of a failed scan.
    scan_error: Option<String>,
}

fn inspected(
    root: &Path,
    subject: Option<&str>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<Inspected> {
    let (model, _, _) = load_for_read(root, proposal)?;
    let id = &model.manifest.id;
    let subject = subject
        .map(|s| vault::find(id, &model.modules, s).map(|m| model.qualify(&m.id)))
        .transpose()?;
    let (checkout, doc) = scan_checkout(&model)?;
    let scan = doc.as_ref().map_err(Clone::clone);
    let infra = read_infra(&checkout);
    let lsp = language_server_facts(&model, &checkout, &doc, &infra, language_servers)?;
    let (modules, subject) = (&model.modules, subject.as_deref());
    let report = inspect::inspect(id, modules, &checkout, scan, &infra, subject, &lsp);
    let scan_error = (scan_error(&doc).or(infra.error.as_deref())).map(str::to_owned);
    Ok(Inspected {
        model,
        report,
        scan_error,
    })
}

/// `n` and `noun`, plural unless `n` is 1.
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Each relationship's qualified ID with how it reads: `From -> To [kind]`.
fn relationship_names(model: &Model) -> BTreeMap<String, String> {
    let names = names(model);
    let mut found = BTreeMap::new();
    for module in &model.modules {
        let from = &module.name;
        for r in &module.outgoing {
            let to = name_of(&names, &r.to);
            found.insert(model.qualify(&r.id), format!("{from} -> {to} [{}]", r.kind));
        }
    }
    found
}

/// The relationships `ids` as they read, or `undrawn` for none.
fn drawn_as(relationships: &BTreeMap<String, String>, ids: &[String]) -> String {
    if ids.is_empty() {
        return "undrawn".to_owned();
    }
    let read = ids
        .iter()
        .map(|id| relationships.get(id).unwrap_or(id).as_str());
    format!("drawn as {}", read.collect::<Vec<_>>().join(", "))
}

/// How a module's symbols read after its file and entry point counts.
fn symbols_summary(m: &inspect::Interface) -> String {
    match (&m.symbols, m.symbols_skipped) {
        (Some(s), _) => {
            let served = s.source == inspect::Source::LanguageServer;
            let by = if served { " by language server" } else { "" };
            format!(", {} exported, {} used outside{by}", s.exported, s.used)
        }
        (None, Some(reason)) => format!(", symbols {}", reason.as_str()),
        (None, None) => String::new(),
    }
}

fn print_inspect(model: &Model, report: &InspectReport) {
    let names = names(model);
    let relationships = relationship_names(model);
    for m in &report.modules {
        let symbols = symbols_summary(m);
        let entries = count(m.entry_points.len(), "entry point");
        println!("{}: {}, {entries}{symbols}", m.name, count(m.files, "file"));
        for entry in &m.entry_points {
            println!("  entry point {entry}");
        }
        match m.symbols.as_ref().map(|s| &s.unused) {
            Some(Some(unused)) => {
                (unused.iter()).for_each(|u| println!("  unused outside: {} {}", u.file, u.name))
            }
            Some(None) => println!("  unused outside: withheld, symbol coverage is incomplete"),
            None => {}
        }
        for deep in &m.deep_imports {
            println!("  deep import: {} -> {}", deep.from, deep.to);
        }
        for (arrow, list) in [("->", &m.outgoing), ("<-", &m.incoming)] {
            for n in list {
                let edges = count(n.edges, "edge");
                let drawn = drawn_as(&relationships, &n.relationships);
                println!("  {arrow} {}: {edges}, {drawn}", name_of(&names, &n.module));
            }
        }
        for (arrow, list) in [("->", &m.hops_out), ("<-", &m.hops_in)] {
            for h in list {
                let other = name_of(&names, &h.module);
                let at = format!("{}:{} {} = {}", h.file, h.line, h.variable, h.address);
                println!("  {arrow} {other}: {} at {at}", h.evidence.as_str());
            }
        }
    }
    if !report.skips.is_empty() || !report.language_servers.is_empty() {
        println!("Skipped:");
    }
    let modules = (report.skips.iter()).map(|s| (name_of(&names, &s.module), s.reason));
    let servers = (report.language_servers.iter()).map(|s| (s.subject.as_str(), s.reason));
    for (subject, reason) in modules.chain(servers) {
        println!("- {subject} {}", reason.as_str());
    }
}

/// `model locality`: co-change locality of the current state's modules over Git history.
fn model_locality(root: &Path, markdown: bool, since: Option<&str>, limit: usize) -> Result<()> {
    let (model, _, _) = load_for_read(root, None)?;
    let history = locality::read(&checkpoint::checkout(&model.dir)?, since, limit)?;
    let report = locality::measure(&model.manifest.id, &model.modules, &history.changes);
    if markdown {
        print_locality(&model, &history, &report);
        return Ok(());
    }
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "locality",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
        "history": history,
    });
    extend(&mut output, &report)?;
    write(None, &output)
}

fn print_locality(model: &Model, history: &History, report: &LocalityReport) {
    let names = names(model);
    println!(
        "Read {} of {} (at most {}; {} of more than {} files left out)",
        count(history.commits, "commit"),
        history.range,
        history.limit,
        count(history.sweeps, "sweep"),
        history.max_files
    );
    if history.shallow {
        println!("The checkout is shallow: older history is missing");
    }
    for m in &report.modules {
        let partners: Vec<String> = (m.partners.iter().take(3))
            .map(|p| format!("{} x{}", name_of(&names, &p.module), p.commits))
            .collect();
        let with = if partners.is_empty() {
            String::new()
        } else {
            format!("; changes with {}", partners.join(", "))
        };
        println!(
            "- {}: {}, {} local ({:.0}%){with}",
            m.name,
            count(m.commits, "commit"),
            m.local,
            m.local_share * 100.0
        );
    }
    if !report.hidden_couplings.is_empty() {
        println!("Hidden couplings:");
    }
    for c in &report.hidden_couplings {
        let [a, b] = &c.modules;
        let (a, b) = (name_of(&names, a), name_of(&names, b));
        println!(
            "- {a} and {b}: {} commits together, no relationship",
            c.commits
        );
    }
}

/// `model footprint`: the modules a branch's changes touch and its model changes. A base model
/// that does not load leaves `model` null with `model_error`.
fn model_footprint(
    root: &Path,
    markdown: bool,
    base: Option<&str>,
    head: Option<&str>,
) -> Result<()> {
    let checkout = checkpoint::checkout(&vault::path(root)?)?;
    let range = footprint::range(&checkout, base, head)?;
    let model = match &range.head {
        None => load_for_read(root, None)?.0,
        Some(head) => checkpoint::load_committed(&checkout, head)?.ok_or_else(|| {
            Refusal::new(Code::VaultNotFound, format!("commit {head} holds no model"))
        })?,
    };
    let (before, model_error) = match checkpoint::load_committed(&checkout, &range.base.commit) {
        Ok(before) => (before, None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let files = footprint::changed(&checkout, &range.base.commit, range.head.as_deref())?;
    let (mut doc, mut scan_error) = footprint_scan(&model, &range, &files)?;
    // The infrastructure is read from the checkout, so it serves only a head that is checked out,
    // and only changed files cross anything. A failed read skips the crossings as a scan does.
    let infra = if range.checked_out && !files.is_empty() {
        read_infra(&checkout)
    } else {
        Infra::default()
    };
    if let Some(error) = &infra.error {
        doc = Err(CrossingsSkipped::ScanFailed);
        scan_error.get_or_insert_with(|| error.clone());
    }
    let head_state = (&model.modules[..], &model.collections[..]);
    let base_state = (before.as_ref()).map(|m| (&m.modules[..], &m.collections[..]));
    let (id, scan) = (&model.manifest.id, doc.as_ref().map_err(|s| *s));
    let report = footprint::measure(id, head_state, base_state, &files, scan, &infra);
    if markdown {
        print_footprint(&model, &range, &report, model_error.as_deref());
        return Ok(());
    }
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "footprint",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
        "base": range.base,
        "head": range.head,
    });
    extend(&mut output, &report)?;
    // Absent parts stay as null, so the output keeps one shape.
    output["crossings"] = json!(report.crossings);
    output["crossings_skipped"] = json!(report.crossings_skipped);
    output["model"] = json!(report.model);
    output["model_error"] = json!(model_error);
    output["scan_error"] = json!(scan_error);
    write(None, &output)
}

/// The scan footprint crossings read, or why there is none, with the error of a failed scan.
/// The scan reads the checkout, so it serves only a head that is checked out.
fn footprint_scan(
    model: &Model,
    range: &Range,
    files: &[String],
) -> Result<(Result<Document, CrossingsSkipped>, Option<String>)> {
    if !range.checked_out {
        return Ok((Err(CrossingsSkipped::HeadNotCheckedOut), None));
    }
    if files.is_empty() {
        return Ok((Err(CrossingsSkipped::NotNeeded), None));
    }
    Ok(match scan_checkout(model)?.1 {
        Ok(doc) => (Ok(doc), None),
        Err(NoScan::NotNeeded) => (Err(CrossingsSkipped::NotNeeded), None),
        Err(NoScan::Failed(error)) => (Err(CrossingsSkipped::ScanFailed), Some(error)),
    })
}

fn print_footprint(model: &Model, range: &Range, report: &FootprintReport, error: Option<&str>) {
    let names = names(model);
    let relationships = relationship_names(model);
    let head = range.head.as_deref().unwrap_or("the working tree");
    let reference = range.base.reference.as_deref().unwrap_or("HEAD");
    println!(
        "Footprint of {head} since {} ({reference})",
        range.base.commit
    );
    for m in &report.modules {
        let parts = match m.in_parts {
            0 => String::new(),
            n => format!(", {n} in parts"),
        };
        println!("- {}: {}{parts}", m.name, count(m.files.len(), "file"));
    }
    if !report.outside.is_empty() {
        println!("Outside every binding: {}", report.outside.join(", "));
    }
    for c in report.crossings.iter().flatten() {
        let (from, to) = (name_of(&names, &c.from), name_of(&names, &c.to));
        let drawn = drawn_as(&relationships, &c.relationships);
        let hops = match c.hops {
            0 => String::new(),
            n => format!(", {}", count(n, "hop")),
        };
        println!(
            "Crosses {from} -> {to}: {}{hops}, {drawn}",
            count(c.edges, "edge")
        );
    }
    if let Some(skipped) = report.crossings_skipped {
        println!("Crossings not checked: {}", skipped.as_str());
    }
    for (section, changes) in report.model.iter().flat_map(|m| m.sections()) {
        let (added, removed) = (changes.added.len(), changes.removed.len());
        let changed = changes.changed.len();
        if added + removed + changed > 0 {
            println!("Model {section}: {added} added, {removed} removed, {changed} changed");
        }
    }
    if let Some(error) = error {
        println!("Model changes not compared: {error}");
    }
}

/// What the code check left unjudged.
struct Skipped<'a> {
    skips: &'a [Skip],
    units: &'a [UnitStatus],
}

fn print_diagnostics(
    diagnostics: &[Diagnostic],
    skipped: Skipped<'_>,
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
    let mut counts = BTreeMap::new();
    for skip in skipped.skips {
        *counts.entry((skip.site, skip.reason)).or_insert(0) += 1;
    }
    let unbound = skipped.units.iter().filter(|u| u.owner.is_none()).count();
    if !counts.is_empty() || unbound > 0 {
        println!("Skipped:");
    }
    for ((site, reason), count) in counts {
        println!("- {} {}: {count}", site.as_str(), reason.as_str());
    }
    if unbound > 0 {
        println!("- unit {}: {unbound}", SkipReason::InfraUnbound.as_str());
    }
    let verdict = if warning == 0 { "passed" } else { "failed" };
    println!("Check {verdict}: warnings: {warning}, information: {information}");
}

/// Warns on stderr when what an interrupted write covers no longer loads after finishing it: a
/// newer build may judge a record an older one planned otherwise. The writes themselves are
/// complete. Each write's own scope is loaded: the current state, its proposal, or both.
fn warn_unloadable(dir: &Path, finished: &[(&Scope, &Receipt)]) {
    for (scope, receipt) in finished {
        let current = (scope.current()).then(|| vault::load_dir(dir.to_path_buf()));
        let proposal = (scope.proposal()).map(|id| vault::load_proposal(dir.to_path_buf(), id));
        for error in current.into_iter().chain(proposal).filter_map(Result::err) {
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

/// The model for `model read` and `model check`, the current state or the proposal `proposal`,
/// its machine-local state folder (`None` with a warning on stderr) and the edit pending there.
/// A `MODEL_INVALID` refusal also names the pending edit.
fn load_for_read(
    root: &Path,
    proposal: Option<&str>,
) -> Result<(Model, Option<PathBuf>, Option<Value>)> {
    let dir = vault::locate(root)?;
    let state = boundrail::arch::state_dir(&dir)
        .inspect_err(|error| eprintln!("warning: claims and pending edits omitted: {error:#}"))
        .ok();
    let pending = state.as_deref().and_then(pending_operation);
    let model = vault::load_target(dir, proposal).map_err(|mut error| {
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

/// The model `model read` prints, with its machine-local state folder, the edit pending there
/// and the checkpoint it came from.
struct Loaded {
    model: Model,
    state: Option<PathBuf>,
    pending: Option<Value>,
    checkpoint: Option<Checkpoint>,
}

/// [`load_for_read`], or with `checkpoint` the model that checkpoint holds, which has no claims
/// or pending edit and needs no vault in the checkout.
fn load_reading(root: &Path, checkpoint: Option<&str>, proposal: Option<&str>) -> Result<Loaded> {
    let Some(which) = checkpoint else {
        let (model, state, pending) = load_for_read(root, proposal)?;
        return Ok(Loaded {
            model,
            state,
            pending,
            checkpoint: None,
        });
    };
    let (checkpoint, model) = checkpoint::load(root, which)?;
    Ok(Loaded {
        model,
        state: None,
        pending: None,
        checkpoint: Some(checkpoint),
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

fn print_checkpoint(checkpoint: &Checkpoint) {
    println!(
        "Checkpoint {} by {} at {}: {}",
        checkpoint.id, checkpoint.writer, checkpoint.time, checkpoint.doing
    );
    println!(
        "  commit {} revision {}",
        checkpoint.commit, checkpoint.revision
    );
}

/// Prints a journaled write's receipt inside the output envelope, with the current state's
/// revision after it when given.
fn print_written(
    markdown: bool,
    kind: &str,
    dir: &Path,
    receipt: &Receipt,
    current: Option<&boundrail::arch::revision::Revision>,
) -> Result<()> {
    if markdown {
        print_receipt(&label(kind), receipt);
        if let Some(current) = current {
            println!("current revision {}", current.digest);
        }
        return Ok(());
    }
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": kind,
        "success": true,
        "vault": dir,
    });
    extend(&mut output, receipt)?;
    extend(&mut output, json!({ "revision": current }))?;
    write(None, &output)
}

/// Adds the fields of `fields`, which serializes to an object, to the output envelope `output`,
/// leaving out null ones.
fn extend(output: &mut Value, fields: impl serde::Serialize) -> Result<()> {
    if let (Some(envelope), Value::Object(fields)) =
        (output.as_object_mut(), serde_json::to_value(fields)?)
    {
        envelope.extend(fields.into_iter().filter(|(_, value)| !value.is_null()));
    }
    Ok(())
}

/// A write's kind as a Markdown line starts it: `edit` as `Edit`.
fn label(kind: &str) -> String {
    let mut label = kind.to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn print_receipt(label: &str, receipt: &Receipt) {
    println!(
        "{label} {} by {}: {}",
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

/// `model proposals`: every proposal folder with its ID, name, status and predecessor, or why
/// its `proposal.json` does not read. `model read --proposal` shows a proposal's revision.
fn list_proposals(root: &Path, markdown: bool) -> Result<()> {
    if !markdown {
        return write(None, &proposals_output(root)?);
    }
    let entries = proposal_entries(&vault::locate(root)?)?;
    if entries.is_empty() {
        println!("No proposals");
    }
    for entry in &entries {
        let id = entry["id"].as_str().unwrap_or_default();
        match entry["error"].as_str() {
            Some(error) => println!("- {id}: {error}"),
            None => println!(
                "- {} {id}, {}, drawn from {}",
                entry["name"].as_str().unwrap_or_default(),
                entry["status"].as_str().unwrap_or_default(),
                entry["predecessor"]["state"]
                    .as_str()
                    .unwrap_or("no current state"),
            ),
        }
    }
    Ok(())
}

/// The JSON output of `model proposals`.
pub(super) fn proposals_output(root: &Path) -> Result<Value> {
    let dir = vault::locate(root)?;
    let entries = proposal_entries(&dir)?;
    Ok(json!({
        "schema": MODEL_SCHEMA,
        "kind": "proposals",
        "success": true,
        "vault": dir,
        "proposals": entries,
    }))
}

fn proposal_entries(dir: &Path) -> Result<Vec<Value>> {
    let list = proposal::list(dir)?;
    Ok(list
        .iter()
        .map(|listed| match &listed.proposal {
            Ok(p) => json!({"id": listed.id, "name": p.name, "status": p.status, "predecessor": p.predecessor}),
            Err(error) => json!({"id": listed.id, "error": error}),
        })
        .collect())
}

/// The JSON output of `model checkpoints`.
pub(super) fn checkpoints_output(root: &Path, limit: Option<usize>) -> Result<Value> {
    let dir = vault::path(root)?;
    let list = checkpoint::list(root, limit)?;
    Ok(json!({
        "schema": MODEL_SCHEMA,
        "kind": "checkpoints",
        "success": true,
        "vault": dir,
        "checkpoints": list,
    }))
}

/// `model compare`: the proposal `subject` against the current state or the proposal `against`.
fn compare_proposal(root: &Path, markdown: bool, subject: &str, against: &str) -> Result<()> {
    if !markdown {
        return write(None, &compare_output(root, subject, against)?);
    }
    let c = compared(root, subject, against)?;
    print_comparison(&c.proposal, &c.against, &c.comparison);
    Ok(())
}

/// The JSON output of `model compare`.
pub(super) fn compare_output(root: &Path, subject: &str, against: &str) -> Result<Value> {
    let c = compared(root, subject, against)?;
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "compare",
        "success": true,
        "vault": c.vault,
        "proposal": c.proposal,
        "against": c.against,
    });
    extend(&mut output, &c.comparison)?;
    Ok(output)
}

/// What `model compare` found.
struct Compared {
    vault: PathBuf,
    /// How the output names the proposal and the state it is compared with.
    proposal: Value,
    against: Value,
    comparison: Comparison,
}

fn compared(root: &Path, subject: &str, against: &str) -> Result<Compared> {
    let dir = vault::locate(root)?;
    let after = vault::load_proposal(dir.clone(), subject)?;
    let before = match against {
        CURRENT => vault::load_dir(dir.clone())?,
        other => vault::load_proposal(dir.clone(), other)?,
    };
    let comparison = compare::compare(
        &after.manifest.id,
        (&before.modules, &before.collections),
        (&after.modules, &after.collections),
    );
    let side = |model: &Model| match &model.proposal {
        Some(drawn) => {
            json!({"id": model.qualify(&drawn.id), "name": drawn.name, "revision": model.revision.digest})
        }
        None => json!({"id": CURRENT, "revision": model.revision.digest}),
    };
    Ok(Compared {
        proposal: side(&after),
        against: side(&before),
        vault: dir,
        comparison,
    })
}

fn print_comparison(proposal: &Value, against: &Value, comparison: &Comparison) {
    let named = |side: &Value| match side["name"].as_str() {
        Some(name) => format!("{name} ({})", side["id"].as_str().unwrap_or_default()),
        None => "the current state".to_owned(),
    };
    println!("Proposal {} against {}", named(proposal), named(against));
    if comparison.is_empty() {
        println!("No differences");
    }
    for (noun, changes) in comparison.sections() {
        for (verb, entries) in [
            ("added", &changes.added),
            ("removed", &changes.removed),
            ("changed", &changes.changed),
        ] {
            for entry in entries {
                let name = entry
                    .name
                    .as_deref()
                    .map(|n| format!("{n} "))
                    .unwrap_or_default();
                let fields = if entry.fields.is_empty() {
                    String::new()
                } else {
                    format!(": {}", entry.fields.join(", "))
                };
                let noun = noun.strip_suffix('s').unwrap_or(noun);
                println!("- {verb} {noun} {name}{}{fields}", entry.id);
            }
        }
    }
}

/// The proposal line of a markdown reading of a proposal.
fn print_proposal(model: &Model) {
    if let Some(drawn) = &model.proposal {
        let status = serde_json::to_value(drawn.status).unwrap_or_default();
        println!(
            "Proposal {} ({}), {}, revision {}",
            drawn.name,
            model.qualify(&drawn.id),
            status.as_str().unwrap_or_default(),
            model.revision.digest
        );
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
    write(None, &refusal_output(refusal))
}

/// The JSON output of a refused command.
pub(super) fn refusal_output(refusal: &Refusal) -> Value {
    let mut error = json!({"code": refusal.code, "message": refusal.message});
    if let Some(details) = &refusal.details {
        error["details"] = details.clone();
    }
    json!({
        "schema": MODEL_SCHEMA,
        "kind": "refusal",
        "success": false,
        "error": error,
    })
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

fn print_walkthroughs(walkthroughs: &[WalkthroughEntry<'_>]) {
    if walkthroughs.is_empty() {
        return;
    }
    println!("Walkthroughs:");
    for walkthrough in walkthroughs {
        let beats = count(walkthrough.beats, "beat");
        println!("- {} {}: {beats}", walkthrough.name, walkthrough.id);
    }
}

/// The walkthrough as a script: each beat numbered with its title, its narration indented below,
/// and one line saying what it shows.
fn print_walkthrough(model: &Model, walkthrough: &Walkthrough) {
    let id = model.qualify(&walkthrough.id);
    println!("Walkthrough {} {id}", walkthrough.name);
    if let Some(description) = &walkthrough.description {
        println!("{description}");
    }
    let names = names(model);
    let flows: BTreeMap<String, &str> = (model.collections.iter())
        .flat_map(|c| &c.flows)
        .map(|f| (model.qualify(&f.id), f.name.as_str()))
        .collect();
    for (n, beat) in (1..).zip(&walkthrough.beats) {
        println!("{n}. {}", beat.title);
        for line in beat.narration.lines() {
            println!("   {line}");
        }
        println!("   Shows: {}", describe(&names, &flows, &beat.reading));
    }
}

/// One line saying what a reading shows, naming modules and flows by `names` and `flows`.
fn describe(
    names: &BTreeMap<String, &str>,
    flows: &BTreeMap<String, &str>,
    reading: &BeatReading,
) -> String {
    let mut text = reading.view.as_str().to_owned();
    if let Some(flow) = &reading.flow {
        text += &format!(" of flow {}", flows.get(flow).copied().unwrap_or(flow));
    }
    text += &format!(" in {}", reading.state.text());
    if let Some(against) = &reading.against {
        text += &format!(", compared with {}", against.text());
    }
    if let Some(scope) = &reading.scope {
        text += &format!(", parts of {}", name_of(names, scope));
    }
    if let Some(depth) = reading.depth {
        text += &format!(", depth {depth}");
    }
    if !reading.focus.is_empty() {
        let focus: Vec<&str> = reading.focus.iter().map(|m| name_of(names, m)).collect();
        text += &format!(", focus on {}", focus.join(", "));
    }
    text
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
