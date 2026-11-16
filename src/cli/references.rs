//! `model read <module> --references`, `--reference N` and `--code`: where a module's references
//! and binding lead. Other repositories are only read, never written or fetched.

use super::*;
use boundrail::arch::reference::{self, CodeTarget, Followed};
use boundrail::arch::types::{Module, Reference};

#[derive(serde::Serialize)]
struct Entry<'a> {
    index: usize,
    reference: &'a Reference,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved: Option<Resolved>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<Skipped>,
}

#[derive(serde::Serialize)]
struct Resolved {
    checkout: PathBuf,
    vault: PathBuf,
    model: Manifested,
    state: ReadingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoint: Option<Checkpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<Subject>,
}

#[derive(serde::Serialize)]
struct Manifested {
    id: String,
    name: String,
}

#[derive(serde::Serialize)]
struct Subject {
    id: String,
    name: String,
    #[serde(rename = "type")]
    what: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}

#[derive(serde::Serialize)]
struct Skipped {
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<Code>,
    message: String,
}

fn checkout_root(root: &Path) -> Result<PathBuf> {
    checkpoint::checkout(&vault::path(root)?)
}

fn subject(model: &Model, id: &str) -> Result<Subject> {
    let found = |what, local: &str, name: &str| Subject {
        id: model.qualify(local),
        name: name.to_owned(),
        what,
        kind: None,
    };
    Ok(match model.find_grouping(id) {
        Some(Found::Collection(c)) => found("collection", &c.id, &c.name),
        Some(Found::Flow(_, f)) => found("flow", &f.id, &f.name),
        Some(Found::Walkthrough(w)) => found("walkthrough", &w.id, &w.name),
        Some(Found::Picture(p)) => found("picture", &p.id, &p.name),
        Some(Found::View(v)) => found("view", &v.id, &v.name),
        None => {
            let module = vault::find(&model.manifest.id, &model.modules, id)?;
            Subject {
                kind: Some(module.kind.clone()),
                ..found("module", &module.id, &module.name)
            }
        }
    })
}

fn entries<'a>(root: &Path, module: &'a Module) -> Result<Vec<Entry<'a>>> {
    if module.references.is_empty() {
        return Ok(Vec::new());
    }
    let checkout = checkout_root(root)?;
    let resolve = |reference: &Reference| -> Result<Resolved> {
        let Followed {
            checkout,
            model,
            state,
            checkpoint,
        } = reference::follow(&checkout, reference)?;
        let subject = (reference.subject.as_deref())
            .map(|s| subject(&model, s))
            .transpose()?;
        Ok(Resolved {
            checkout,
            vault: model.dir,
            model: Manifested {
                id: model.manifest.id,
                name: model.manifest.name,
            },
            state,
            checkpoint,
            subject,
        })
    };
    let entries = (module.references.iter().enumerate())
        .map(|(index, reference)| {
            let (resolved, skipped) = match resolve(reference) {
                Ok(resolved) => (Some(resolved), None),
                Err(error) => {
                    let code = error.downcast_ref::<Refusal>().map(|r| r.code);
                    let message = format!("{error:#}");
                    (None, Some(Skipped { code, message }))
                }
            };
            Entry {
                index,
                reference,
                resolved,
                skipped,
            }
        })
        .collect();
    Ok(entries)
}

fn list_output(root: &Path, loaded: Loaded, subject: &str) -> Result<Value> {
    let Loaded {
        model, checkpoint, ..
    } = loaded;
    let module = vault::find(&model.manifest.id, &model.modules, subject)?;
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "read-references",
        "success": true,
        "vault": model.dir,
        "module": model.qualify(&module.id),
        "references": entries(root, module)?,
        "revision": model.revision,
    });
    extend(&mut output, json!({ "proposal": model.proposal }))?;
    if let Some(checkpoint) = checkpoint {
        output["checkpoint"] = json!(checkpoint);
    }
    Ok(output)
}

/// [`list_output`] for the browser view, from the current state, a proposal or a checkpoint.
pub(crate) fn references_output(
    root: &Path,
    subject: &str,
    checkpoint: Option<&str>,
    proposal: Option<&str>,
) -> Result<Value> {
    list_output(root, load_reading(root, checkpoint, proposal)?, subject)
}

pub(super) fn list(root: &Path, loaded: Loaded, subject: &str, markdown: bool) -> Result<()> {
    if !markdown {
        return write(None, &list_output(root, loaded, subject)?);
    }
    let model = &loaded.model;
    let module = vault::find(&model.manifest.id, &model.modules, subject)?;
    if let Some(checkpoint) = &loaded.checkpoint {
        print_checkpoint(checkpoint);
    }
    print_proposal(model);
    let id = model.qualify(&module.id);
    if module.references.is_empty() {
        println!("{} {id} has no references.", module.name);
        return Ok(());
    }
    println!("References of {} {id}:", module.name);
    for entry in entries(root, module)? {
        match (&entry.resolved, &entry.skipped) {
            (Some(r), _) => {
                let subject = (r.subject.as_ref()).map_or_else(String::new, |s| {
                    let kind = s.kind.as_deref().unwrap_or(s.what);
                    format!(": {} [{kind}] {}", s.name, s.id)
                });
                println!(
                    "  {}. {} ({}){subject}",
                    entry.index,
                    r.model.name,
                    r.state.text()
                );
                let at = escape_controls(&r.checkout.to_string_lossy());
                println!("     at {at}");
            }
            (None, Some(s)) => {
                let code = s.code.map(|c| format!("{}: ", c.as_str()));
                let message = escape_controls(&s.message);
                println!(
                    "  {}. skipped: {}{message}",
                    entry.index,
                    code.unwrap_or_default()
                );
            }
            (None, None) => {}
        }
    }
    Ok(())
}

fn followed(
    root: &Path,
    model: &Model,
    subject: &str,
    index: usize,
    markdown: bool,
) -> Result<Option<Value>> {
    let module = vault::find(&model.manifest.id, &model.modules, subject)?;
    let id = model.qualify(&module.id);
    let reference = module.references.get(index).ok_or_else(|| {
        let message = format!(
            "{} {id} has {} reference(s); --reference counts from 0",
            module.name,
            module.references.len()
        );
        Refusal::new(Code::RequestInvalid, message)
    })?;
    let Followed {
        checkout,
        model: other,
        state,
        checkpoint,
    } = reference::follow(&checkout_root(root)?, reference)?;
    if markdown {
        println!(
            "Reference {index} of {}: {} ({}) at {}",
            module.name,
            other.manifest.name,
            state.text(),
            escape_controls(&checkout.to_string_lossy())
        );
    }
    let place = json!({ "index": index, "module": id, "checkout": checkout, "state": state });
    let loaded = Loaded {
        model: other,
        state: None,
        pending: None,
        checkpoint,
    };
    let output = match &reference.subject {
        Some(subject) => {
            let named = Named {
                subject: Some(subject),
                flow: None,
                walkthrough: None,
                picture: None,
                view: None,
            };
            read_named(loaded, &named, markdown)?
        }
        None if markdown => {
            print_whole(loaded, true)?;
            None
        }
        None => Some(whole_output(loaded)?),
    };
    Ok(output.map(|mut output| {
        output["reference"] = place;
        output
    }))
}

pub(super) fn follow(
    root: &Path,
    model: &Model,
    subject: &str,
    index: usize,
    markdown: bool,
) -> Result<()> {
    match followed(root, model, subject, index, markdown)? {
        Some(output) => write(None, &output),
        None => Ok(()),
    }
}

/// [`followed`] as JSON for the browser view.
pub(crate) fn reference_output(
    root: &Path,
    subject: &str,
    index: usize,
    checkpoint: Option<&str>,
    proposal: Option<&str>,
) -> Result<Value> {
    let loaded = load_reading(root, checkpoint, proposal)?;
    let output = followed(root, &loaded.model, subject, index, false)?;
    output.context("a JSON reading returns its output")
}

pub(super) fn code(
    root: &Path,
    loaded: Loaded,
    subject: &str,
    open: bool,
    markdown: bool,
) -> Result<()> {
    let Loaded {
        model, checkpoint, ..
    } = loaded;
    let module = vault::find(&model.manifest.id, &model.modules, subject)?;
    let id = model.qualify(&module.id);
    let binding = module.binding.as_ref().ok_or_else(|| {
        let message = format!("{} {id} has no binding", module.name);
        Refusal::new(Code::RequestInvalid, message)
    })?;
    let target = reference::code_target(&checkout_root(root)?, binding, checkpoint.as_ref());
    let opened = open.then(|| open_local(&target)).transpose()?;
    if markdown {
        let repo = (target.repo.as_deref()).map_or_else(String::new, |r| format!("{r}: "));
        let symbol =
            (target.symbol.as_deref()).map_or_else(String::new, |s| format!(", symbol {s}"));
        let bound = escape_controls(&format!("{repo}{}{symbol}", target.path));
        println!("{} {id} is bound to {bound}", module.name);
        match (&target.local_path, &target.unavailable) {
            (Some(path), _) => {
                println!("  local path {}", escape_controls(&path.to_string_lossy()));
            }
            (None, Some(why)) => println!("  not on this machine: {}", escape_controls(why)),
            (None, None) => {}
        }
        if let Some(commit) = &target.commit {
            println!("  retained at commit {commit}");
        }
        if let Some(c) = &target.correction {
            println!(
                "  the commit is from correction {} by {} (as its Writer trailer says, unverified)",
                escape_controls(&c.checkpoint),
                escape_controls(&c.writer)
            );
        }
        return Ok(());
    }
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "read-code",
        "success": true,
        "vault": model.dir,
        "module": id,
        "code_target": target,
        "revision": model.revision,
    });
    extend(&mut output, json!({ "proposal": model.proposal }))?;
    if let Some(checkpoint) = checkpoint {
        output["checkpoint"] = json!(checkpoint);
    }
    if let Some(opened) = opened {
        output["opened"] = json!(opened);
    }
    write(None, &output)
}

fn open_local(target: &CodeTarget) -> Result<Vec<String>> {
    let refuse =
        |message: String| -> anyhow::Error { Refusal::new(Code::RequestInvalid, message).into() };
    let Some(path) = &target.local_path else {
        let why = target.unavailable.as_deref().unwrap_or_default();
        return Err(refuse(format!(
            "{} is not on this machine, so it cannot be opened: {why}",
            target.path
        )));
    };
    let metadata = fs::metadata(path)
        .map_err(|e| refuse(format!("{} cannot be opened: {e}", path.display())))?;
    let folder = if metadata.is_dir() {
        path.as_path()
    } else {
        path.parent().unwrap_or(path)
    };
    let (program, args): (&str, Vec<std::ffi::OsString>) = if cfg!(target_os = "macos") {
        let flag = if metadata.is_dir() { "-R" } else { "-t" };
        ("open", vec![flag.into(), path.into()])
    } else if cfg!(windows) {
        (
            "explorer",
            vec![explorer_folder(folder).map_err(refuse)?.into()],
        )
    } else {
        ("xdg-open", vec![folder.into()])
    };
    let line: Vec<String> = iter::once(program.to_owned())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    eprintln!("boundrail: opening {}", escape_controls(&line.join(" ")));
    spawn_detached(program, &args).with_context(|| format!("starting {program}"))?;
    Ok(line)
}

fn explorer_folder(folder: &Path) -> std::result::Result<String, String> {
    let shown = folder.display();
    let text = (folder.to_str()).ok_or_else(|| format!("{shown} is not Unicode"))?;
    let plain = match text.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => rest.to_owned(),
        },
        None => text.to_owned(),
    };
    if plain.contains(',') {
        return Err(format!(
            "{shown} holds a comma, which Explorer reads as an option separator"
        ));
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_gets_a_plain_folder_without_commas() {
        let plain = |p: &str| explorer_folder(Path::new(p));
        assert_eq!(plain(r"\\?\C:\code\src").unwrap(), r"C:\code\src");
        assert_eq!(
            plain(r"\\?\UNC\host\share\src").unwrap(),
            r"\\host\share\src"
        );
        assert_eq!(plain(r"C:\code").unwrap(), r"C:\code");
        assert!(plain(r"\\?\C:\a,b").is_err());
        assert!(plain("/home/a,b").is_err());
    }
}
