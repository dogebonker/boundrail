use super::*;
use clap::ValueEnum;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::iter;

use boundrail::arch::bundle::{self, Exported};
use boundrail::arch::checkpoint::{self, Checkpoint, Published};
use boundrail::arch::claims::{self, Claim};
use boundrail::arch::code::{self, CodeReport, NoScan, Skip, SkipReason, UnitStatus};
use boundrail::arch::compare::{self, Comparison};
use boundrail::arch::diagnostic::{Diagnostic, Severity};
use boundrail::arch::edit::{self, Receipt};
use boundrail::arch::footprint::{
    self, AgainstProposal, CrossingsSkipped, FootprintReport, Proposed, Range,
};
use boundrail::arch::github;
use boundrail::arch::infra::{self, Infra};
use boundrail::arch::inspect::{self, InspectReport};
use boundrail::arch::locality::{self, History, LocalityReport};
use boundrail::arch::lsp::{self, Facts};
use boundrail::arch::picture;
use boundrail::arch::proposal::{self, CURRENT, Writing};
use boundrail::arch::render::{self, View};
use boundrail::arch::request::INPUT_MAX;
use boundrail::arch::semantic::{self, States};
use boundrail::arch::types::{
    self, BeatReading, Collection, Flow, Picture, ReadingState, ReadingView, SCHEMAS, Step,
    Walkthrough,
};
use boundrail::arch::vault::{
    self, CollectionView, Found, Model, ModuleView, PictureEntry, RelationshipView, Scope,
    WalkthroughEntry, escape_controls,
};
use boundrail::arch::{Code, Refusal};
use boundrail::cochange::MAX_FILES;
use serde_json::{Value, json};

#[path = "pictures.rs"]
mod pictures;
#[path = "setup.rs"]
mod setup;
pub(super) use pictures::pictures_output;
use setup::Agent;

const MODEL_SCHEMA: &str = "boundrail/model/v1";

#[derive(Args)]
pub(super) struct ModelArgs {
    /// Directory inside the Git checkout that holds the model
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    /// Output format; JSON when absent, except `model render`, which then prints the SVG.
    /// `annotations` (model check only) prints GitHub workflow commands, and `pr-comment`
    /// (model footprint only) a Markdown pull request comment
    #[arg(long, global = true, value_enum)]
    format: Option<ModelFormat>,
    #[command(subcommand)]
    command: Command,
}

/// `model check` exits with its own verdict code; every other command exits 0 or refuses.
#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Model(ModelCmd),
    /// Check the model's own shape, compare it with the code, and check that embedded pictures
    /// still match the model; exits 1 when any warning is found
    Check {
        /// Run one check only; the code check scans the checkout, the embeds check draws every
        /// picture
        #[arg(long, value_enum)]
        only: Option<CheckOnly>,
        /// Check this proposal instead of the current state
        #[arg(long)]
        proposal: Option<String>,
        /// Check the current state and then every draft proposal, reporting each; exits 1 when
        /// any of them has a warning or does not load
        #[arg(long, conflicts_with = "proposal")]
        every_draft: bool,
        /// Run the language servers .boundrail/language-servers.json names, for call references and symbols.
        /// They run code from the checkout (a workspace tsserver.js under node_modules,
        /// rust-analyzer build scripts and proc macros, jdtls Gradle and Maven imports): use it on
        /// trusted checkouts only, never on untrusted pull request heads in CI. The servers may
        /// leave build output and project metadata in the checkout (rust-analyzer target/, jdtls
        /// .project, .classpath and .settings), while Boundrail itself writes nothing there
        #[arg(long)]
        language_servers: bool,
    },
    /// Report the modules a branch changes, the relationships its changed imports cross, and
    /// what it changes in the model; with --fail-outside, exits 1 when it changes a module the
    /// proposal does not name
    Footprint {
        /// The revision the branch starts from [default: the merge-base of HEAD with
        /// origin/HEAD, main or master, else HEAD]
        #[arg(long)]
        base: Option<String>,
        /// The branch's last commit [default: HEAD with the working tree]
        #[arg(long)]
        head: Option<String>,
        /// Set the changes beside what this proposal names: modules named and changed, changed
        /// but not named, named but not changed. Read from the checkout's vault
        #[arg(long)]
        proposal: Option<String>,
        /// Exit 1 when a changed module is outside what --proposal names and none of its
        /// changed parts holds its changes
        #[arg(long, requires = "proposal")]
        fail_outside: bool,
    },
    /// Draw the model as SVG: the architecture, or a flow as a sequence diagram or flowchart,
    /// optionally marked with what it changes against another state
    Render(RenderArgs),
    /// Serve a live, read-only view of the model in the browser on a local port
    View(super::view_cli::ViewArgs),
    /// List the picture recipes with whether each file still matches what the model draws:
    /// fresh, stale, missing or unrenderable
    Pictures {
        /// Draw every stale or missing picture to its output file, creating its folders; a
        /// picture left unrenderable shows in its status. Writes only the files the recipes
        /// declare, never the vault, and overwrites only files Boundrail drew; PNGs need resvg,
        /// rsvg-convert or magick on PATH outside the checkout
        #[arg(long)]
        write: bool,
        /// Only this picture: its ID or unique name
        #[arg(long)]
        only: Option<String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelFormat {
    Json,
    Markdown,
    Annotations,
    PrComment,
}

#[derive(Args)]
struct RenderArgs {
    #[command(flatten)]
    spec: RenderSpec,
    /// Write to this file instead of standard output; its folder must exist
    #[arg(long)]
    output: Option<PathBuf>,
    /// Rasterize the picture to this PNG file, with the first of resvg, rsvg-convert and magick
    /// on PATH outside the checkout; its folder must exist. Prints the SVG only with --output
    /// or --format json
    #[arg(long)]
    png: Option<PathBuf>,
    /// PNG pixels per picture unit, more than 0 and at most 8 [default: 1]
    #[arg(long, requires = "png")]
    scale: Option<f64>,
    /// Rasterize only this part of the picture: x,y,w,h in picture units from its top-left corner
    #[arg(long, requires = "png")]
    region: Option<String>,
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
    /// Draw what this picture's recipe reads: its ID or unique name; needs no other flag that
    /// says what to draw
    #[arg(long, conflicts_with = "walkthrough")]
    pub(super) picture: Option<String>,
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
    /// Draw this module and its direct neighbours as usual and dim the rest: its ID or unique
    /// name; repeat for several
    #[arg(long)]
    pub(super) focus: Vec<String>,
    /// Leave out the modules of this node kind, its key in config.yaml; their parts stay in the
    /// nearest container drawn. Repeat for several; architecture view only
    #[arg(long = "hide-kind")]
    pub(super) hide_kind: Vec<String>,
    /// Name the node and relationship kinds drawn below the picture; architecture view only
    #[arg(long)]
    pub(super) legend: bool,
    /// Colors: light, dark, or auto, which follows the viewer's color scheme [default: light]
    #[arg(long, value_enum)]
    pub(super) theme: Option<RenderTheme>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum RenderView {
    Architecture,
    Sequence,
    Flowchart,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum RenderTheme {
    Light,
    Dark,
    Auto,
}

impl From<RenderTheme> for render::Theme {
    fn from(theme: RenderTheme) -> Self {
        match theme {
            RenderTheme::Light => Self::Light,
            RenderTheme::Dark => Self::Dark,
            RenderTheme::Auto => Self::Auto,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum CheckOnly {
    Semantic,
    Code,
    Embeds,
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
    /// Set a repository up for the model: create the vault when absent, install the model
    /// skills for the agents named, and optionally write a starter language-server list.
    /// Running it again changes nothing; files you edited stay unless --force replaces them
    Setup {
        /// The model's name when the vault is created [default: the checkout folder's name]
        #[arg(long)]
        name: Option<String>,
        /// Install the boundrail-model, boundrail-plan and boundrail-pr skills into this agent's
        /// project skill folder (repeatable)
        #[arg(long, value_enum)]
        agent: Vec<Agent>,
        /// Write .boundrail/language-servers.json naming the allowlisted servers of the
        /// languages the checkout root's project files show
        #[arg(long)]
        language_servers: bool,
        /// Replace skill and language-server files that differ from what setup writes
        #[arg(long)]
        force: bool,
    },
    /// Print the model, one module with its parts and relationships, a collection, a flow, a
    /// walkthrough, or a picture recipe
    Read {
        /// Qualified ID, local ID, or a unique current or former module name; or a collection,
        /// flow, walkthrough or picture ID
        #[arg(conflicts_with_all = ["flow", "walkthrough", "picture"])]
        subject: Option<String>,
        /// Print one flow with its steps: its qualified or local ID, or its unique name
        #[arg(long, conflicts_with_all = ["walkthrough", "picture"])]
        flow: Option<String>,
        /// Print one walkthrough with its beats: its qualified or local ID, or its unique name;
        /// Markdown prints it as a script
        #[arg(long, conflicts_with = "picture")]
        walkthrough: Option<String>,
        /// Print one picture recipe: its qualified or local ID, or its unique name
        #[arg(long)]
        picture: Option<String>,
        /// Read the model a checkpoint holds: its ID or a unique commit prefix
        #[arg(long)]
        checkpoint: Option<String>,
        /// Read a proposal instead of the current state: its ID
        #[arg(long, conflicts_with = "checkpoint")]
        proposal: Option<String>,
    },
    /// List the vault file and request schemas, or print one as JSON Schema
    Schema { name: Option<String> },
    /// Apply a batch of module, relationship, collection, flow, step, walkthrough, beat and picture changes (schema `model-edit`)
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
    /// Report what a proposal adds, removes and changes against the current state, another
    /// proposal or a checkpoint, and how each module's drawn interface changes
    Compare {
        /// The proposal's ID
        proposal: String,
        /// Compare against the current state, or this proposal
        #[arg(long, default_value = CURRENT)]
        against: String,
        /// Compare against the model a checkpoint holds instead: its ID or a unique commit prefix
        #[arg(long, conflicts_with = "against")]
        against_checkpoint: Option<String>,
        /// Compare this alternative proposal against the same state too, and report the
        /// alternatives' changes and interface measures side by side; repeat for several
        #[arg(long)]
        beside: Vec<String>,
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
    /// Merge into a draft what the current state changed since the draft was drawn or last
    /// reconciled, and make the current state its predecessor; both sides changing one module,
    /// relationship or collection refuses with the conflicts
    Reconcile {
        /// The proposal's ID
        proposal: String,
        /// The current state's revision digest the merge is for
        #[arg(long)]
        expected_revision: String,
        /// The proposal's revision digest you read; another revision refuses
        #[arg(long)]
        expected_proposal_revision: String,
        #[command(flatten)]
        writing: WritingArgs,
        /// Your claim covering what the merge changes in the proposal
        #[arg(long)]
        claim: Option<String>,
    },
    /// Write the current state, a proposal with its base, or a checkpoint as one JSON bundle
    /// (schema `model-bundle`)
    Export {
        /// Export this proposal instead of the current state
        #[arg(long, conflicts_with = "checkpoint")]
        proposal: Option<String>,
        /// Export the model a checkpoint holds: its ID or a unique commit prefix
        #[arg(long)]
        checkpoint: Option<String>,
        /// Write to this file instead of standard output; its folder must exist, outside the
        /// model vault and the Git folder
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Import a bundle of this model as a new draft proposal; the current state does not change
    Import {
        /// The bundle file; `-` reads standard input
        file: PathBuf,
        /// The current state's revision digest the import is for
        #[arg(long)]
        expected_revision: String,
        /// The draft's name [default: the bundle's proposal or state name]
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        writing: WritingArgs,
    },
    /// Push (fast-forward only) or fetch the checkpoint history on a configured Git remote
    History {
        #[command(subcommand)]
        command: HistoryCmd,
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
    /// Report each module's interface as the model draws it (entry points, hidden parts, callers,
    /// outgoing, part-less entry parts that only forward, relationships on the container,
    /// measures; Markdown prints it for a subject only) and each bound module's interface from a
    /// scan: entry points, exported, used and unused symbols, imports past its entry points, and
    /// import edges per neighbouring module
    Inspect {
        /// One module: its ID or unique current or former name
        #[arg(conflicts_with = "group")]
        subject: Option<String>,
        /// Inspect this proposal instead of the current state
        #[arg(long)]
        proposal: Option<String>,
        /// Inspect the model a checkpoint holds: its ID or a unique commit prefix. The code is
        /// not scanned, as the checkout's code is not that state's code
        #[arg(long, conflicts_with_all = ["proposal", "language_servers"])]
        checkpoint: Option<String>,
        /// Report one group the config names, by its key, from the model alone: its members,
        /// relationships between them, relationships crossing its boundary, and neighbours
        #[arg(long, conflicts_with = "language_servers")]
        group: Option<String>,
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
}

#[derive(Subcommand)]
enum HistoryCmd {
    /// Push refs/boundrail/history to the same ref on the remote
    ///
    /// The history has one authoritative remote: push to and fetch from only that one.
    Push {
        /// A remote `git remote` lists
        #[arg(long, default_value = checkpoint::DEFAULT_REMOTE)]
        remote: String,
    },
    /// Fetch the remote's refs/boundrail/history into the local one, replaying local checkpoints
    /// the remote lacks
    ///
    /// The history has one authoritative remote: push to and fetch from only that one.
    Fetch {
        /// A remote `git remote` lists
        #[arg(long, default_value = checkpoint::DEFAULT_REMOTE)]
        remote: String,
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
    let markdown = matches!(args.format, Some(ModelFormat::Markdown));
    let result = refuse_format(args.format, &args.command).and_then(|()| match args.command {
        Command::Check {
            only,
            every_draft: true,
            language_servers,
            ..
        } => check_every_draft(&args.root, args.format, only, language_servers),
        Command::Check {
            only,
            proposal,
            language_servers,
            ..
        } => check(
            &args.root,
            args.format,
            only,
            proposal.as_deref(),
            language_servers,
        ),
        Command::Render(render) => {
            render_model(&args.root, args.format, &render).map(|()| ExitCode::SUCCESS)
        }
        Command::View(view) => super::view_cli::run(&args.root, &view).map(|()| ExitCode::SUCCESS),
        Command::Pictures { write, only } => {
            pictures::list(&args.root, markdown, only.as_deref(), write)
        }
        Command::Footprint {
            base,
            head,
            proposal,
            fail_outside,
        } => {
            let (base, head) = (base.as_deref(), head.as_deref());
            let report = model_footprint(&args.root, args.format, base, head, proposal.as_deref())?;
            Ok(footprint_verdict(report.as_ref(), fail_outside))
        }
        Command::Model(command) => {
            execute(&args.root, markdown, command).map(|()| ExitCode::SUCCESS)
        }
    });
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

/// Refuses `REQUEST_INVALID` a format the command does not print: `annotations` belongs to
/// `model check` and `pr-comment` to `model footprint`.
fn refuse_format(format: Option<ModelFormat>, command: &Command) -> Result<()> {
    let printed = match format {
        Some(ModelFormat::Annotations) => matches!(command, Command::Check { .. }),
        Some(ModelFormat::PrComment) => {
            matches!(command, Command::Footprint { .. })
        }
        _ => true,
    };
    if printed {
        return Ok(());
    }
    let message =
        "--format annotations is for model check, --format pr-comment for model footprint";
    Err(Refusal::new(Code::RequestInvalid, message).into())
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
            picture: None,
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
            print_pictures(&model.picture_entries());
            print_claims(state.as_deref().and_then(active_claims).as_deref());
            Ok(())
        }
        ModelCmd::Read {
            subject,
            flow,
            walkthrough,
            picture,
            checkpoint,
            proposal,
        } => {
            let Loaded {
                model,
                state,
                pending,
                checkpoint,
            } = load_reading(root, checkpoint.as_deref(), proposal.as_deref())?;
            let found = match (&flow, &walkthrough, &picture) {
                (Some(flow), _, _) => {
                    let (collection, flow) = model.flow(flow)?;
                    Some(Found::Flow(collection, flow))
                }
                (None, Some(walkthrough), _) => {
                    Some(Found::Walkthrough(model.walkthrough(walkthrough)?))
                }
                (None, None, Some(picture)) => Some(Found::Picture(model.picture(picture)?)),
                (None, None, None) => subject.as_deref().and_then(|s| model.find_grouping(s)),
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
        ModelCmd::Reconcile {
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
                proposal::reconcile(&dir, &writing, &proposal, &expected_revision, compared)?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            print_written(markdown, "reconcile", &dir, &receipt, None)
        }
        ModelCmd::Export {
            proposal,
            checkpoint,
            output,
        } => {
            let what = match (&proposal, &checkpoint) {
                (Some(proposal), _) => Exported::Proposal(proposal),
                (None, Some(checkpoint)) => Exported::Checkpoint(checkpoint),
                (None, None) => Exported::Current,
            };
            export_bundle(root, markdown, what, output.as_deref())
        }
        ModelCmd::Import {
            file,
            expected_revision,
            name,
            writing,
        } => {
            let dir = vault::locate(root)?;
            let input = read_request(Some(&file))?;
            let writing = writing.writing(None);
            let (receipt, finished) =
                bundle::import(&dir, &writing, &input, &expected_revision, name.as_deref())?;
            if let Some(scope) = &finished {
                warn_unloadable(&dir, &[(scope, &receipt)]);
            }
            print_written(markdown, "import", &dir, &receipt, None)
        }
        ModelCmd::History { command } => {
            let (pushing, remote) = match &command {
                HistoryCmd::Push { remote } => (true, remote),
                HistoryCmd::Fetch { remote } => (false, remote),
            };
            let published = if pushing {
                checkpoint::push(root, remote)?
            } else {
                checkpoint::fetch(root, remote)?
            };
            print_published(markdown, pushing, &published)
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
        ModelCmd::Compare {
            proposal,
            against,
            against_checkpoint,
            beside,
        } => {
            let asked = Compare {
                proposal: &proposal,
                against: &against,
                against_checkpoint: against_checkpoint.as_deref(),
                beside: &beside,
            };
            compare_proposal(root, markdown, &asked)
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
            checkpoint,
            group,
            language_servers,
        } => {
            let asked = Inspect {
                subject: subject.as_deref(),
                proposal: proposal.as_deref(),
                checkpoint: checkpoint.as_deref(),
                group: group.as_deref(),
                language_servers,
            };
            inspect_model(root, markdown, &asked)
        }
        ModelCmd::Locality { since, limit } => {
            model_locality(root, markdown, since.as_deref(), limit)
        }
        ModelCmd::Setup {
            name,
            agent,
            language_servers,
            force,
        } => setup::run(
            root,
            markdown,
            name.as_deref(),
            &agent,
            language_servers,
            force,
        ),
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
        "pictures": model.picture_entries(),
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
            Found::Picture(picture) => {
                if self.markdown {
                    print_picture(model, picture);
                }
                let entry = model.picture_entry(picture);
                (
                    entry.id.clone(),
                    json!({"kind": "read-picture", "picture": entry}),
                )
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

fn render_model(root: &Path, format: Option<ModelFormat>, args: &RenderArgs) -> Result<()> {
    let invalid =
        |message: String| anyhow::Error::from(Refusal::new(Code::RequestInvalid, message));
    if matches!(format, Some(ModelFormat::Markdown)) {
        let message = "model render prints SVG, or JSON with --format json";
        return Err(invalid(message.to_owned()));
    }
    let scale = args.scale.unwrap_or(1.0);
    if !(scale > 0.0 && scale <= MAX_SCALE) {
        return Err(invalid(format!(
            "--scale must be more than 0 and at most {MAX_SCALE}"
        )));
    }
    let region = (args.region.as_deref())
        .map(|text| {
            let numbers: Result<Vec<f64>, _> =
                text.split(',').map(|n| n.trim().parse::<f64>()).collect();
            match numbers.as_deref() {
                Ok(&[x, y, w, h]) if [x, y, w, h].iter().all(|n| n.is_finite()) => Ok([x, y, w, h]),
                _ => Err(invalid(format!("--region takes x,y,w,h, not `{text}`"))),
            }
        })
        .transpose()?;
    let output = args.output.as_deref();
    let png = args.png.as_deref();
    if let Some(png) = png {
        check_output(root, png, "--png")?;
        if output.is_some_and(|output| same_file(png, output)) {
            return Err(invalid(format!(
                "--png and --output both name {}; give each its own file",
                png.display()
            )));
        }
    }
    let (rendered, view) = render_svg(root, &args.spec, output)?;
    let render::Fallbacks { routes, labels } = rendered.fallbacks;
    if routes + labels > 0 {
        eprintln!(
            "note: render fallbacks: {} with no path around the boxes drawn with a plain bend, {} with no free spot placed at the middle of the route's middle segment",
            count(routes, "arrow"),
            count(labels, "label")
        );
    }
    let png = match png {
        Some(path) => {
            let framed = picture::reframe(&rendered.svg, scale, region).map_err(invalid)?;
            let checkout = checkpoint::checkout(&vault::path(root)?)?;
            let drawing = picture::Drawing::new(framed);
            let bytes = picture::rasterize(&drawing, &checkout).map_err(|failed| {
                let details = json!({"reason": failed.reason});
                Refusal::new(Code::RequestInvalid, failed.message).with_details(details)
            })?;
            Some((path, bytes))
        }
        None => None,
    };
    let text = if matches!(format, Some(ModelFormat::Json)) {
        let mut envelope = json!({
            "schema": MODEL_SCHEMA,
            "kind": "render",
            "success": true,
            "view": view,
            "revision": rendered.revision,
            "width": rendered.width,
            "height": rendered.height,
            "modules": rendered.modules,
            "relationships": rendered.relationships,
            "digest": boundrail::files::hash(rendered.svg.as_bytes()),
            "fallbacks": {
                "routes": rendered.fallbacks.routes,
                "labels": rendered.fallbacks.labels,
            },
            "svg": rendered.svg,
        });
        if let Some((path, _)) = &png {
            envelope["png"] = json!(path);
        }
        Some(serde_json::to_string_pretty(&envelope)? + "\n")
    } else if output.is_some() || png.is_none() {
        Some(rendered.svg)
    } else {
        None
    };
    if let (Some(path), Some(text)) = (output, &text) {
        write_file(path, text.as_bytes())?;
    }
    if let Some((path, bytes)) = &png {
        write_file(path, bytes)?;
    }
    if let (None, Some(text)) = (output, text) {
        print!("{text}");
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    let resolved = |path: &Path| {
        let parent = (path.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Some(fs::canonicalize(parent).ok()?.join(path.file_name()?))
    };
    a == b || resolved(a).is_some_and(|r| Some(r) == resolved(b))
}

const MAX_SCALE: f64 = 8.0;

pub(super) fn render_svg(
    root: &Path,
    spec: &RenderSpec,
    output: Option<&Path>,
) -> Result<(render::Rendered, &'static str)> {
    if spec.walkthrough.is_none() && spec.beat.is_none() && spec.picture.is_none() {
        return draw(root, spec, output);
    }
    let (current, _) = load_state(root, RenderState::Current)?;
    let reading = match &spec.picture {
        Some(picture) => {
            let why = "--picture draws what its recipe reads";
            if spec.walkthrough.is_some() || spec.beat.is_some() {
                let message = format!("{why}; leave out --walkthrough and --beat");
                return Err(Refusal::new(Code::RequestInvalid, message).into());
            }
            spec.refuse_drawing_flags(why)?;
            &current.picture(picture)?.reading
        }
        None => spec.beat(&current)?,
    };
    draw_reading(root, reading, output, &mut States::new(&current))
}

fn draw(
    root: &Path,
    spec: &RenderSpec,
    output: Option<&Path>,
) -> Result<(render::Rendered, &'static str)> {
    let view = spec.view()?;
    let (drawn, base) = spec.states()?;
    let (model, name) = load_state(root, drawn)?;
    let base = base.map(|b| load_state(root, b)).transpose()?;
    let base = (base.as_ref()).map(|(model, name)| (model, name.as_deref()));
    let focus = render::neighbourhood(&model, &spec.focus)?;
    let options = render::Options {
        focus: &focus,
        hide: &spec.hide_kind,
        legend: spec.legend,
        theme: spec.theme.map_or(render::Theme::Light, Into::into),
    };
    paint(
        root,
        &view,
        &options,
        (&model, name.as_deref()),
        base,
        output,
    )
}

fn draw_reading(
    root: &Path,
    reading: &BeatReading,
    output: Option<&Path>,
    states: &mut States<'_>,
) -> Result<(render::Rendered, &'static str)> {
    let spec = reading_spec(reading);
    let view = spec.view()?;
    let (drawn, base) = spec.states()?;
    let (drawn, base) = (drawn.reading(), base.map(RenderState::reading));
    states.load(&drawn);
    if let Some(base) = &base {
        states.load(base);
    }
    let drawn = states.get(&drawn).map_err(again)?;
    let base = (base.as_ref())
        .map(|b| states.get(b))
        .transpose()
        .map_err(again)?;
    let options = render::Options {
        focus: &reading.focus,
        ..render::Options::default()
    };
    paint(root, &view, &options, drawn, base, output)
}

/// A load error kept in a cache, as a new error: a refusal stays one.
fn again(error: &anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<Refusal>() {
        Some(refusal) => {
            let copy = Refusal::new(refusal.code, refusal.message.clone());
            match &refusal.details {
                Some(details) => copy.with_details(details.clone()).into(),
                None => copy.into(),
            }
        }
        None => anyhow::anyhow!("{error:#}"),
    }
}

fn paint<'m>(
    root: &Path,
    view: &View<'_>,
    options: &render::Options<'_>,
    drawn: (&'m Model, Option<&str>),
    base: Option<(&'m Model, Option<&str>)>,
    output: Option<&Path>,
) -> Result<(render::Rendered, &'static str)> {
    // A proposal's local and qualified IDs name the same state.
    let id = |model: &Model| model.proposal.as_ref().map(|p| p.id.clone());
    if let Some((base, _)) = base
        && id(drawn.0).is_some()
        && id(drawn.0) == id(base)
    {
        let message = "--against names the state being drawn";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    if let Some(output) = output {
        check_output(root, output, "--output")?;
    }
    let side = |(model, name): (&'m Model, Option<&str>)| render::Side {
        model,
        name: name.map(str::to_owned),
    };
    let base = base.map(side);
    let rendered = render::render(&side(drawn), base.as_ref(), view, options)?;
    Ok((rendered, view.name()))
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

impl RenderState<'_> {
    /// The state as a reading names it.
    fn reading(self) -> ReadingState {
        match self {
            Self::Checkpoint(c) => ReadingState::Checkpoint(c.to_owned()),
            Self::Current => ReadingState::Current,
            Self::Proposal(p) => ReadingState::Proposal(p.to_owned()),
        }
    }
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
    /// What the beat `--beat` of `--walkthrough` reads in `model`, the current state. Refuses
    /// `REQUEST_INVALID` without both flags or with any other flag that says what to draw.
    fn beat<'m>(&self, model: &'m Model) -> Result<&'m BeatReading> {
        let invalid = |message: &str| Refusal::new(Code::RequestInvalid, message);
        let (walkthrough, beat) = match (&self.walkthrough, &self.beat) {
            (Some(walkthrough), Some(beat)) => (walkthrough, beat),
            (None, _) => return Err(invalid("--beat needs --walkthrough").into()),
            (_, None) => return Err(invalid("--walkthrough needs --beat").into()),
        };
        self.refuse_drawing_flags("--walkthrough draws what its beat reads")?;
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
        Ok(&found.reading)
    }

    /// Refuses `REQUEST_INVALID` with `why` when a flag says what to draw.
    fn refuse_drawing_flags(&self, why: &str) -> Result<()> {
        let drawing = [
            &self.proposal,
            &self.checkpoint,
            &self.flow,
            &self.scope,
            &self.against,
        ];
        let styled = !self.focus.is_empty()
            || !self.hide_kind.is_empty()
            || self.legend
            || self.theme.is_some();
        if drawing.iter().any(|flag| flag.is_some())
            || self.view.is_some()
            || self.depth.is_some()
            || styled
        {
            let message = format!("{why}; leave out the other flags");
            return Err(Refusal::new(Code::RequestInvalid, message).into());
        }
        Ok(())
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

/// `model export`: the bundle on stdout, or in `--output` with a short report on stdout.
fn export_bundle(
    root: &Path,
    markdown: bool,
    what: Exported<'_>,
    output: Option<&Path>,
) -> Result<()> {
    if markdown && output.is_none() {
        let message = "model export prints a JSON bundle; add --output for a Markdown report";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    if let Some(output) = output {
        check_output(root, output, "--output")?;
    }
    let bundle = bundle::export(root, what)?;
    let text = bundle::to_text(&bundle)?;
    let Some(output) = output else {
        print!("{text}");
        return Ok(());
    };
    write_file(output, text.as_bytes())?;
    if markdown {
        println!(
            "Exported {} files to {}",
            bundle.files.len(),
            output.display()
        );
        return Ok(());
    }
    write(
        None,
        &json!({
            "schema": MODEL_SCHEMA,
            "kind": "export",
            "success": true,
            "output": output,
            "source": bundle.source,
            "files": bundle.files.len(),
        }),
    )
}

/// Prints what `model history push` or `fetch` moved.
fn print_published(markdown: bool, pushing: bool, published: &Published) -> Result<()> {
    let kind = if pushing {
        "history-push"
    } else {
        "history-fetch"
    };
    if !markdown {
        let mut output = json!({ "schema": MODEL_SCHEMA, "kind": kind, "success": true });
        extend(&mut output, published)?;
        return write(None, &output);
    }
    let (verb, place) = if pushing {
        ("Pushed", format!("to {}", published.remote))
    } else {
        ("Fetched", format!("from {}", published.remote))
    };
    if !published.updated {
        println!(
            "{} is up to date with {}",
            published.reference, published.remote
        );
        return Ok(());
    }
    println!(
        "{verb} {} checkpoint(s) {place}: {} -> {}",
        published.checkpoints.len(),
        published.before.as_deref().unwrap_or("none"),
        published.after.as_deref().unwrap_or("none"),
    );
    if !published.checkpoints.is_empty() {
        println!("checkpoints: {}", published.checkpoints.join(", "));
    }
    if !published.replayed.is_empty() {
        println!("replayed: {}", published.replayed.join(", "));
    }
    Ok(())
}

fn check_output(root: &Path, output: &Path, flag: &str) -> Result<()> {
    let parent = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let resolved_folder = fs::canonicalize(parent).map_err(|error| {
        Refusal::new(
            Code::RequestInvalid,
            format!(
                "{flag} {}: its folder cannot be read: {error}",
                output.display()
            ),
        )
    })?;
    let vault = vault::path(root).and_then(|dir| Ok(fs::canonicalize(dir)?));
    let git = boundrail::git::git_dir(root).and_then(|dir| Ok(fs::canonicalize(dir)?));
    let common = boundrail::git::common_dir(root).and_then(|dir| Ok(fs::canonicalize(dir)?));
    let places = [
        (vault, "the model vault"),
        (git, "the Git folder"),
        (common, "the Git folder"),
    ];
    for (inside, place) in places {
        if inside.is_ok_and(|inside| resolved_folder.starts_with(inside)) {
            return Err(Refusal::new(
                Code::RequestInvalid,
                format!(
                    "{flag} {} is inside {place}; write it elsewhere",
                    output.display()
                ),
            )
            .into());
        }
    }
    Ok(())
}

/// `model check`: exit 0 without warnings, 1 with any. Like `model read`, it names a pending edit.
/// The skips are reported only when the code check ran, a picture could not be checked, or a
/// reading was skipped.
fn check(
    root: &Path,
    format: Option<ModelFormat>,
    only: Option<CheckOnly>,
    proposal: Option<&str>,
    language_servers: bool,
) -> Result<ExitCode> {
    let checked = run_check(root, only, proposal, language_servers, &SharedScan::new())?;
    let code = ExitCode::from(u8::from(checked.warning > 0));
    if format == Some(ModelFormat::Annotations) {
        print!("{}", github::annotations(&checked.output()?));
        return Ok(code);
    }
    if format == Some(ModelFormat::Markdown) {
        print_checked(&checked);
        return Ok(code);
    }
    write(None, &checked.output()?)?;
    Ok(code)
}

/// What `model check --format markdown` prints for one state.
fn print_checked(checked: &Checked) {
    let Checked {
        model,
        diagnostics,
        skips,
        units,
        warning,
        information,
        ..
    } = checked;
    print_proposal(model);
    let skipped = Skipped {
        skips: skips.as_deref().unwrap_or_default(),
        units: units.as_deref().unwrap_or_default(),
    };
    print_diagnostics(diagnostics, skipped, *warning, *information);
}

/// `model check --every-draft`: the current state, then every draft proposal as `--proposal`
/// checks it, in `states`. A proposal whose `proposal.json` does not read is checked too, since
/// it may be a draft, and a draft that refuses is reported in its place by its refusal. Exits 1
/// when any state has a warning or refuses. The states share one scan of the checkout.
fn check_every_draft(
    root: &Path,
    format: Option<ModelFormat>,
    only: Option<CheckOnly>,
    language_servers: bool,
) -> Result<ExitCode> {
    let refuse = |message: &str| Err(Refusal::new(Code::RequestInvalid, message).into());
    if format == Some(ModelFormat::Annotations) {
        return refuse("--format annotations reports one state; check without --every-draft");
    }
    if only == Some(CheckOnly::Embeds) {
        return refuse("pictures belong to the current state; check embeds without --every-draft");
    }
    let dir = vault::locate(root)?;
    let drafts: Vec<String> = (proposal::list(&dir)?.into_iter())
        .filter(|listed| {
            (listed.proposal.as_ref()).map_or(true, |p| p.status == types::ProposalStatus::Draft)
        })
        .map(|listed| listed.id)
        .collect();
    let scan = SharedScan::new();
    let current = run_check(root, only, None, language_servers, &scan)?;
    let mut passed = current.warning == 0;
    let mut states = vec![Ok(current)];
    for id in drafts {
        match run_check(root, only, Some(&id), language_servers, &scan) {
            Ok(checked) => {
                passed &= checked.warning == 0;
                states.push(Ok(checked));
            }
            Err(error) => {
                passed = false;
                let refusal = (error.downcast::<Refusal>())
                    .with_context(|| format!("checking draft {id}"))?;
                states.push(Err((id, refusal)));
            }
        }
    }
    let code = ExitCode::from(u8::from(!passed));
    if format == Some(ModelFormat::Markdown) {
        for state in &states {
            match state {
                Ok(checked) if checked.model.proposal.is_none() => {
                    println!("Current state, revision {}", checked.model.revision.digest);
                    print_checked(checked);
                }
                Ok(checked) => print_checked(checked),
                Err((id, refusal)) => {
                    println!(
                        "Proposal {id}: {}: {}",
                        refusal.code.as_str(),
                        refusal.message
                    );
                }
            }
        }
        return Ok(code);
    }
    let states = (states.into_iter())
        .map(|state| match state {
            Ok(checked) => checked.output(),
            Err((id, refusal)) => {
                let mut output = refusal_output(&refusal);
                output["proposal"] = json!({ "id": id });
                Ok(output)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "check-every-draft",
        "success": true,
        "vault": dir,
        "passed": passed,
        "states": states,
    });
    write(None, &output)?;
    Ok(code)
}

/// What `model check` found.
pub(super) struct Checked {
    model: Model,
    diagnostics: Vec<Diagnostic>,
    /// `None` when the code check did not run, every picture was checked and no reading skipped.
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
    scan: &SharedScan,
) -> Result<Checked> {
    if proposal.is_some() && only == Some(CheckOnly::Embeds) {
        let message = "pictures belong to the current state; check embeds without --proposal";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    let (model, _, pending) = load_for_read(root, proposal)?;
    let runs = |part: CheckOnly| only.is_none_or(|o| o == part);
    // Each proposal or checkpoint a beat or picture reads loads once for the whole check.
    let mut states = States::new(&model);
    let (mut skips, mut units) = (None, None);
    let mut diagnostics = if runs(CheckOnly::Semantic) {
        let mut found = semantic::check(&model.manifest.id, &model.modules, &model.collections);
        let (judged, skipped) = semantic::walkthroughs(&mut states);
        found.extend(judged);
        if !skipped.is_empty() {
            skips = Some(skipped);
        }
        found
    } else {
        Vec::new()
    };
    if runs(CheckOnly::Code) {
        let report = code_check(&model, language_servers, scan)?;
        diagnostics.extend(report.diagnostics);
        skips.get_or_insert_with(Vec::new).extend(report.skips);
        units = Some(report.units);
    }
    if runs(CheckOnly::Embeds) && !model.pictures.is_empty() {
        let (found, skipped) = pictures::check(root, &mut states)?;
        diagnostics.extend(found);
        if !skipped.is_empty() {
            skips.get_or_insert_with(Vec::new).extend(skipped);
        }
    }
    drop(states);
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
        live_scan(&checkout)
    } else {
        Err(NoScan::NotNeeded)
    };
    Ok((checkout, doc))
}

/// The scan of `checkout`, or its error.
fn live_scan(checkout: &Path) -> Result<Document, NoScan> {
    super::scan_cli::live(checkout).map_err(|error| NoScan::Failed(format!("{error:#}")))
}

/// The scan of the checkout, made at most once for the states one command checks: it reads the
/// checkout, whichever state asks.
pub(super) type SharedScan = std::cell::OnceCell<Result<Document, NoScan>>;

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

/// The code check against the model's checkout and its infrastructure, with `scan`, made when
/// a binding or anchor names code there. A failed scan or infrastructure read becomes a
/// `SCAN_FAILED` warning.
fn code_check(model: &Model, language_servers: bool, scan: &SharedScan) -> Result<CodeReport> {
    let checkout = checkpoint::checkout(&model.dir)?;
    let checkout = checkout.as_path();
    let doc = if code::needs_scan(&model.modules, checkout) {
        (scan.get_or_init(|| live_scan(checkout)).as_ref()).map_err(Clone::clone)
    } else {
        Err(NoScan::NotNeeded)
    };
    let infra = read_infra(checkout);
    let scanned = doc.as_ref().ok().copied();
    let lsp = language_server_facts(model, checkout, scanned, &infra, language_servers)?;
    Ok(code::check(
        &model.manifest.id,
        &model.modules,
        &model.config,
        checkout,
        doc,
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
    doc: Option<&Document>,
    infra: &Infra,
    enabled: bool,
) -> Result<Facts> {
    let (servers, skips) = lsp::configured(checkout, enabled)?;
    let mut facts = match doc {
        Some(doc) => {
            let (id, modules) = (&model.manifest.id, &model.modules);
            lsp::gather(checkout, &servers, id, modules, doc, infra)
        }
        None => Facts::default(),
    };
    facts.skips.extend(skips);
    Ok(facts)
}

/// What `model inspect` was asked: one module or every one, or one group, in the current state,
/// a proposal or a checkpoint.
#[derive(Default)]
pub(super) struct Inspect<'a> {
    pub(super) subject: Option<&'a str>,
    pub(super) proposal: Option<&'a str>,
    pub(super) checkpoint: Option<&'a str>,
    pub(super) group: Option<&'a str>,
    pub(super) language_servers: bool,
}

/// Why a checkpoint's inspection has no code section.
const CHECKPOINT_CODE_SKIPPED: &str = "the checkout's code is not the checkpoint's code";

/// `model inspect`: each module's interface, or one module's or group's.
fn inspect_model(root: &Path, markdown: bool, asked: &Inspect<'_>) -> Result<()> {
    if !markdown {
        return write(None, &inspect_output(root, asked)?);
    }
    let Inspected {
        model,
        checkpoint,
        measured,
    } = inspected(root, asked)?;
    if let Some(checkpoint) = &checkpoint {
        print_checkpoint(checkpoint);
    }
    print_proposal(&model);
    match measured {
        Measured::Group(group) => print_group(&model, &group),
        Measured::Modules { report, drawn, .. } => {
            match checkpoint {
                Some(_) => println!("Code not scanned: {CHECKPOINT_CODE_SKIPPED}"),
                None => print_inspect(&model, &report),
            }
            // Every module's drawn interface would bury the scan; JSON keeps them all.
            if asked.subject.is_some() {
                print_drawn(&model, &drawn);
            }
        }
    }
    Ok(())
}

/// The JSON output of `model inspect`.
pub(super) fn inspect_output(root: &Path, asked: &Inspect<'_>) -> Result<Value> {
    let Inspected {
        model,
        checkpoint,
        measured,
    } = inspected(root, asked)?;
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "inspect",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
    });
    let scan_error = match measured {
        Measured::Group(group) => {
            output["group"] = json!(group);
            None
        }
        Measured::Modules {
            report,
            drawn,
            scan_error,
        } => {
            extend(&mut output, &report)?;
            output["model_interfaces"] = json!(drawn);
            if checkpoint.is_some() {
                output["code_skipped"] = json!(CHECKPOINT_CODE_SKIPPED);
            }
            scan_error
        }
    };
    extend(&mut output, json!({ "proposal": model.proposal }))?;
    if let Some(error) = scan_error {
        output["scan_error"] = json!(error);
    }
    if let Some(checkpoint) = checkpoint {
        output["checkpoint"] = json!(checkpoint);
    }
    Ok(output)
}

/// What `model inspect` measured, in the state it read.
struct Inspected {
    model: Model,
    checkpoint: Option<Checkpoint>,
    measured: Measured,
}

enum Measured {
    Group(inspect::GroupReport),
    Modules {
        /// The interfaces the scan shows; empty for a checkpoint.
        report: InspectReport,
        /// The interfaces the model draws.
        drawn: Vec<inspect::Drawn>,
        /// The error of a failed scan.
        scan_error: Option<String>,
    },
}

fn inspected(root: &Path, asked: &Inspect<'_>) -> Result<Inspected> {
    let Loaded {
        model, checkpoint, ..
    } = load_reading(root, asked.checkpoint, asked.proposal)?;
    let id = &model.manifest.id;
    if let Some(key) = asked.group {
        let group = model.config.groups.get(key).ok_or_else(|| {
            let message = format!("no group has the key `{key}` in config.yaml");
            Refusal::new(Code::SubjectNotFound, message)
        })?;
        let group = inspect::group(id, &model.modules, key, &group.name);
        return Ok(Inspected {
            model,
            checkpoint,
            measured: Measured::Group(group),
        });
    }
    let subject = (asked.subject)
        .map(|s| vault::find(id, &model.modules, s).map(|m| model.qualify(&m.id)))
        .transpose()?;
    let subject = subject.as_deref();
    let drawn = inspect::drawn(id, &model.modules, subject);
    if checkpoint.is_some() {
        let measured = Measured::Modules {
            report: InspectReport::default(),
            drawn,
            scan_error: None,
        };
        return Ok(Inspected {
            model,
            checkpoint,
            measured,
        });
    }
    let (checkout, doc) = scan_checkout(&model)?;
    let scan = doc.as_ref().map_err(Clone::clone);
    let infra = read_infra(&checkout);
    let scanned = doc.as_ref().ok();
    let lsp = language_server_facts(&model, &checkout, scanned, &infra, asked.language_servers)?;
    let modules = &model.modules;
    let report = inspect::inspect(id, modules, &checkout, scan, &infra, subject, &lsp);
    let scan_error = (scan_error(&doc).map(str::to_owned))
        .or_else(|| report.scan_error.clone())
        .or_else(|| infra.error.clone());
    Ok(Inspected {
        model,
        checkpoint,
        measured: Measured::Modules {
            report,
            drawn,
            scan_error,
        },
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

/// The interfaces the model draws, one paragraph per module.
fn print_drawn(model: &Model, drawn: &[inspect::Drawn]) {
    let names = names(model);
    let relationships = relationship_names(model);
    for d in drawn {
        let m = &d.measures;
        println!(
            "{} as drawn: {}, {}, {}, {} outgoing, depth {}",
            d.name,
            count(m.entry_points, "entry point"),
            count(m.hidden_parts, "hidden part"),
            count(m.callers, "caller"),
            m.outgoing,
            m.depth
        );
        let lists = [
            ("entry point", &d.entry_points),
            ("caller", &d.callers),
            ("outgoing", &d.outgoing),
        ];
        for (label, list) in lists {
            for r in list {
                let drawn = drawn_as(&relationships, &r.relationships);
                println!("  {label} {}: {drawn}", name_of(&names, &r.module));
            }
        }
        for (label, list) in [
            ("hidden part", &d.hidden_parts),
            ("pass-through", &d.pass_through),
        ] {
            (list.iter()).for_each(|id| println!("  {label} {}", name_of(&names, id)));
        }
        for id in &d.on_container {
            let read = relationships.get(id).unwrap_or(id);
            println!("  on container: {read}");
        }
    }
}

/// A group's members, internal and boundary relationships, and neighbours.
fn print_group(model: &Model, group: &inspect::GroupReport) {
    let names = names(model);
    let relationships = relationship_names(model);
    let read = |id: &String| relationships.get(id).unwrap_or(id).clone();
    let named = |ids: &[String]| -> String {
        let list: Vec<&str> = ids.iter().map(|id| name_of(&names, id)).collect();
        if list.is_empty() {
            "none".to_owned()
        } else {
            list.join(", ")
        }
    };
    println!(
        "Group {} ({}): {}, {}, {}",
        group.name,
        group.group,
        count(group.members.len(), "member"),
        count(group.internal.len(), "internal relationship"),
        count(group.boundary.len(), "boundary relationship")
    );
    println!("  members: {}", named(&group.members));
    for id in &group.internal {
        println!("  internal: {}", read(id));
    }
    for c in &group.boundary {
        let arrow = match c.direction {
            inspect::Direction::In => "in",
            inspect::Direction::Out => "out",
        };
        println!("  {arrow}: {}", read(&c.relationship));
    }
    println!("  neighbours: {}", named(&group.neighbours));
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

/// Exit 1 under `--fail-outside` when the branch changes a module outside the proposal, naming
/// each on standard error; 0 otherwise.
fn footprint_verdict(against: Option<&AgainstProposal>, fail_outside: bool) -> ExitCode {
    let Some(against) = against.filter(|a| fail_outside && !a.outside.is_empty()) else {
        return ExitCode::SUCCESS;
    };
    let changed = against.changed_not_named.iter();
    for s in changed.filter(|s| against.outside.contains(&s.module)) {
        eprintln!(
            "boundrail: {} ({}) changed outside the proposal",
            escape_controls(&s.name),
            s.module
        );
    }
    ExitCode::from(1)
}

/// `model footprint`: the modules a branch's changes touch and its model changes, set beside
/// what `proposal` names when given; returns that comparison. A base model that does not load
/// leaves `model` null with `model_error`.
fn model_footprint(
    root: &Path,
    format: Option<ModelFormat>,
    base: Option<&str>,
    head: Option<&str>,
    proposal: Option<&str>,
) -> Result<Option<AgainstProposal>> {
    let checkout = checkpoint::checkout(&vault::path(root)?)?;
    let range = footprint::range(&checkout, base, head)?;
    // The proposal is read from the checkout's vault, whatever head the footprint reads.
    let drawn = proposal
        .map(|p| vault::load_proposal(vault::locate(root)?, p))
        .transpose()?;
    let predecessor = match &drawn {
        Some(drawn) => match vault::load_base(drawn)? {
            Some(base) => Some(base),
            None => {
                let current = vault::load_dir(drawn.dir.clone())?;
                Some((current.modules, current.collections))
            }
        },
        None => None,
    };
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
    let against = match (&drawn, &predecessor) {
        (Some(d), Some((m, c))) => {
            let p = (d.proposal.as_ref()).context("a loaded proposal has its manifest")?;
            let proposed = Proposed {
                id: d.qualify(&p.id),
                name: &p.name,
                predecessor: (&m[..], &c[..]),
                drawn: (&d.modules[..], &d.collections[..]),
            };
            let branch = base_state.zip(report.model.as_ref());
            Some(footprint::against(
                id, &proposed, &files, head_state, branch,
            ))
        }
        _ => None,
    };
    if format == Some(ModelFormat::Markdown) {
        print_footprint(
            &model,
            &range,
            &report,
            against.as_ref(),
            model_error.as_deref(),
        );
        return Ok(against);
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
    extend(&mut output, json!({ "proposal": against }))?;
    // Absent parts stay as null, so the output keeps one shape.
    output["crossings"] = json!(report.crossings);
    output["crossings_skipped"] = json!(report.crossings_skipped);
    output["model"] = json!(report.model);
    output["model_error"] = json!(model_error);
    output["scan_error"] = json!(scan_error);
    output["proposal"] = json!(against);
    if format == Some(ModelFormat::PrComment) {
        let mut names: BTreeMap<String, String> = (names(&model).into_iter())
            .map(|(id, name)| (id, name.to_owned()))
            .collect();
        names.extend(relationship_names(&model));
        print!("{}", github::pr_comment(&output, &names));
    } else {
        write(None, &output)?;
    }
    Ok(against)
}

/// The scan footprint crossings read, or why there is none, with the error of a failed scan.
/// The scan reads the checkout, so it serves only a head that is checked out. A changed file of
/// a language whose collector failed fails it too: its edges are missing.
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
        Ok(doc) => {
            let failed: Vec<String> = (doc.scan_failures.iter())
                .filter(|f| f.files.iter().any(|file| files.binary_search(file).is_ok()))
                .map(|f| format!("{}: {}", f.language.as_str(), f.error))
                .collect();
            if failed.is_empty() {
                (Ok(doc), None)
            } else {
                (Err(CrossingsSkipped::ScanFailed), Some(failed.join("; ")))
            }
        }
        Err(NoScan::NotNeeded) => (Err(CrossingsSkipped::NotNeeded), None),
        Err(NoScan::Failed(error)) => (Err(CrossingsSkipped::ScanFailed), Some(error)),
    })
}

fn print_footprint(
    model: &Model,
    range: &Range,
    report: &FootprintReport,
    against: Option<&AgainstProposal>,
    error: Option<&str>,
) {
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
    if report.large {
        println!("Large change: more than {} files", MAX_FILES);
    }
    if let Some(against) = against {
        println!("Against proposal {} ({}):", against.name, against.id);
        let changed = (against.named_and_changed.iter()).chain(&against.changed_not_named);
        let called: BTreeMap<&str, &str> = changed
            .map(|s| (s.module.as_str(), s.name.as_str()))
            .collect();
        let lists = [
            &against.named_and_changed,
            &against.changed_not_named,
            &against.named_not_changed,
        ];
        for ((_, heading), subjects) in footprint::AGAINST_LISTS.iter().zip(lists) {
            let listed: Vec<String> = (subjects.iter())
                .map(|s| match s.holds.as_slice() {
                    [] => s.name.clone(),
                    holds => {
                        let held: Vec<&str> = (holds.iter())
                            .map(|h| called.get(h.as_str()).copied().unwrap_or(h))
                            .collect();
                        format!("{} (held by {})", s.name, held.join(", "))
                    }
                })
                .collect();
            let listed = if listed.is_empty() {
                "none".to_owned()
            } else {
                listed.join(", ")
            };
            println!("- {heading}: {listed}");
        }
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

/// Prints `checkpoint`, whose ID, writer, doing and revision are history text a commit made
/// outside `model retain` may fill with control characters, so those are escaped.
fn print_checkpoint(checkpoint: &Checkpoint) {
    println!(
        "Checkpoint {} by {} at {}: {}",
        escape_controls(&checkpoint.id),
        escape_controls(&checkpoint.writer),
        checkpoint.time,
        escape_controls(&checkpoint.doing)
    );
    println!(
        "  commit {} revision {}",
        checkpoint.commit,
        escape_controls(&checkpoint.revision)
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
fn compare_proposal(root: &Path, markdown: bool, asked: &Compare<'_>) -> Result<()> {
    if !markdown {
        return write(None, &compare_output(root, asked)?);
    }
    let c = compared(root, asked)?;
    let first = &c.alternatives[0];
    print_comparison(&first.side, &c.side, &first.comparison);
    let mut named = names(&c.against);
    named.extend(names(&first.model));
    print_interface_changes(&named, &first.interface_changes);
    if c.alternatives.len() > 1 {
        print_alternatives(&c);
    }
    Ok(())
}

/// What `model compare` was asked: a proposal against the current state, another proposal or a
/// checkpoint, beside other proposals.
pub(super) struct Compare<'a> {
    pub(super) proposal: &'a str,
    pub(super) against: &'a str,
    pub(super) against_checkpoint: Option<&'a str>,
    pub(super) beside: &'a [String],
}

/// The JSON output of `model compare`.
pub(super) fn compare_output(root: &Path, asked: &Compare<'_>) -> Result<Value> {
    let c = compared(root, asked)?;
    let first = &c.alternatives[0];
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "compare",
        "success": true,
        "vault": c.vault,
        "proposal": first.side,
        "against": c.side,
    });
    extend(&mut output, &first.comparison)?;
    output["interface_changes"] = json!(first.interface_changes);
    if c.alternatives.len() > 1 {
        let alternatives: Vec<Value> = (c.alternatives.iter())
            .map(|a| {
                json!({
                    "proposal": a.side,
                    "subjects": a.comparison.counts(),
                    "interface_changes": a.interface_changes.len(),
                })
            })
            .collect();
        output["alternatives"] = json!(alternatives);
        output["interface_measures"] = json!(c.measures());
    }
    Ok(output)
}

/// What `model compare` found.
struct Compared {
    vault: PathBuf,
    /// The state compared against, how the output names it, and its drawn interfaces.
    against: Model,
    side: Value,
    drawn: Vec<inspect::Drawn>,
    /// The proposal, then each `--beside` proposal, compared against it.
    alternatives: Vec<Alternative>,
}

struct Alternative {
    model: Model,
    side: Value,
    comparison: Comparison,
    interface_changes: Vec<compare::InterfaceChange>,
    drawn: Vec<inspect::Drawn>,
}

impl Compared {
    fn measures(&self) -> Vec<compare::MeasuresRow> {
        let drawn: Vec<&[inspect::Drawn]> =
            self.alternatives.iter().map(|a| &a.drawn[..]).collect();
        compare::measures_beside(&self.drawn, &drawn)
    }
}

fn compared(root: &Path, asked: &Compare<'_>) -> Result<Compared> {
    let dir = vault::locate(root)?;
    let after = vault::load_proposal(dir.clone(), asked.proposal)?;
    let (before, checkpoint) = match (asked.against_checkpoint, asked.against) {
        (Some(which), _) => {
            let (checkpoint, model) = checkpoint::load(root, which)?;
            (model, Some(checkpoint))
        }
        (None, CURRENT) => (vault::load_dir(dir.clone())?, None),
        (None, other) => (vault::load_proposal(dir.clone(), other)?, None),
    };
    let mut models = vec![after];
    for beside in asked.beside {
        models.push(vault::load_proposal(dir.clone(), beside)?);
    }
    let model_id = models[0].manifest.id.clone();
    let side = |model: &Model| match &model.proposal {
        Some(drawn) => {
            json!({"id": model.qualify(&drawn.id), "name": drawn.name, "revision": model.revision.digest})
        }
        None => json!({"id": CURRENT, "revision": model.revision.digest}),
    };
    let against_side = match &checkpoint {
        Some(checkpoint) => json!({"checkpoint": checkpoint, "revision": before.revision.digest}),
        None => side(&before),
    };
    let drawn = inspect::drawn(&model_id, &before.modules, None);
    let alternatives = (models.into_iter())
        .map(|after| {
            let comparison = compare::compare(
                &model_id,
                (&before.modules, &before.collections),
                (&after.modules, &after.collections),
            );
            let now = inspect::drawn(&model_id, &after.modules, None);
            Alternative {
                side: side(&after),
                comparison,
                interface_changes: compare::interface_changes(&drawn, &now),
                drawn: now,
                model: after,
            }
        })
        .collect();
    Ok(Compared {
        vault: dir,
        against: before,
        side: against_side,
        drawn,
        alternatives,
    })
}

/// How a Markdown line names a compared state.
fn state_named(side: &Value) -> String {
    match (side["name"].as_str(), side["checkpoint"]["id"].as_str()) {
        (Some(name), _) => format!("{name} ({})", side["id"].as_str().unwrap_or_default()),
        (None, Some(id)) => format!("checkpoint {}", escape_controls(id)),
        (None, None) => "the current state".to_owned(),
    }
}

fn print_comparison(proposal: &Value, against: &Value, comparison: &Comparison) {
    println!(
        "Proposal {} against {}",
        state_named(proposal),
        state_named(against)
    );
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

/// One line per module whose drawn interface changed, naming what it gained and lost.
fn print_interface_changes(names: &BTreeMap<String, &str>, changes: &[compare::InterfaceChange]) {
    if changes.is_empty() {
        return;
    }
    println!("Interface changes:");
    for c in changes {
        let mut said: Vec<String> = (c.lists().into_iter())
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(label, ids)| {
                let named: Vec<&str> = ids.iter().map(|id| name_of(names, id)).collect();
                format!("{label} {}", named.join(", "))
            })
            .collect();
        let m = &c.interface_measures;
        if m.before != m.after {
            said.push(format!(
                "measures {} -> {}",
                measured(Some(m.before)),
                measured(Some(m.after))
            ));
        }
        println!("- {} ({}): {}", c.name, c.module, said.join("; "));
    }
}

/// Measures as `entry points/hidden parts/callers/outgoing/depth`, or `-` for a missing module.
fn measured(measures: Option<inspect::Measures>) -> String {
    measures.map_or("-".to_owned(), |m| {
        let inspect::Measures {
            entry_points,
            hidden_parts,
            callers,
            outgoing,
            depth,
        } = m;
        format!("{entry_points}/{hidden_parts}/{callers}/{outgoing}/{depth}")
    })
}

/// The alternatives' change counts, then the measures of every module they do not all share.
fn print_alternatives(c: &Compared) {
    println!("Alternatives against {}:", state_named(&c.side));
    println!("| Proposal | Added | Removed | Changed | Interface changes |");
    println!("|---|---|---|---|---|");
    for a in &c.alternatives {
        let n = a.comparison.counts();
        println!(
            "| {} | {} | {} | {} | {} |",
            state_named(&a.side),
            n.added,
            n.removed,
            n.changed,
            a.interface_changes.len()
        );
    }
    let rows: Vec<_> = (c.measures().into_iter())
        .filter(|r| r.alternatives.iter().any(|m| *m != r.against))
        .collect();
    if rows.is_empty() {
        return;
    }
    println!("Interface measures (entry points/hidden parts/callers/outgoing/depth):");
    let heads: Vec<String> = iter::once(&c.side)
        .chain(c.alternatives.iter().map(|a| &a.side))
        .map(state_named)
        .collect();
    println!("| Module | {} |", heads.join(" | "));
    println!("|---|{}", "---|".repeat(heads.len()));
    for row in rows {
        let cells: Vec<String> = iter::once(row.against)
            .chain(row.alternatives.iter().copied())
            .map(measured)
            .collect();
        println!("| {} ({}) | {} |", row.name, row.module, cells.join(" | "));
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

fn print_pictures(pictures: &[PictureEntry<'_>]) {
    if pictures.is_empty() {
        return;
    }
    println!("Pictures:");
    for picture in pictures {
        println!("- {} {}: {}", picture.name, picture.id, picture.output);
    }
}

/// The picture recipe: its file and one line saying what it shows.
fn print_picture(model: &Model, picture: &Picture) {
    println!("Picture {} {}", picture.name, model.qualify(&picture.id));
    if let Some(description) = &picture.description {
        println!("{description}");
    }
    println!("Writes: {}", picture.output);
    let shows = describe(&names(model), &flow_names(model), &picture.reading);
    println!("Shows: {shows}");
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
    let flows = flow_names(model);
    for (n, beat) in (1..).zip(&walkthrough.beats) {
        println!("{n}. {}", beat.title);
        for line in beat.narration.lines() {
            println!("   {line}");
        }
        println!("   Shows: {}", describe(&names, &flows, &beat.reading));
    }
}

/// Flow names by qualified ID.
fn flow_names(model: &Model) -> BTreeMap<String, &str> {
    (model.collections.iter())
        .flat_map(|c| &c.flows)
        .map(|f| (model.qualify(&f.id), f.name.as_str()))
        .collect()
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
