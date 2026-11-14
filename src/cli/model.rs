use super::*;
use boundrail::arch::types::{self, SCHEMAS};
use boundrail::arch::vault::{self, Model, ModuleView, RelationshipView};
use boundrail::arch::{Code, Refusal};
use serde_json::json;

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
    /// List the vault file schemas, or print one as JSON Schema
    Schema { name: Option<String> },
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
            let model = vault::load(&args.root)?;
            let reading = model.read();
            if markdown {
                print_reading(&model, &reading);
                return Ok(());
            }
            write(
                None,
                &json!({
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
                }),
            )
        }
        ModelCmd::Read {
            subject: Some(subject),
        } => {
            let model = vault::load(&args.root)?;
            let selection = model.select(&subject)?;
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
                return Ok(());
            }
            write(
                None,
                &json!({
                    "schema": MODEL_SCHEMA,
                    "kind": "read-module",
                    "success": true,
                    "vault": model.dir,
                    "module": selection.module,
                    "parts": selection.parts,
                    "incoming": selection.incoming,
                    "outgoing": selection.outgoing,
                    "revision": model.revision,
                }),
            )
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
