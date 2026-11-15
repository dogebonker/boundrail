//! `model setup`: makes a repository ready for agents to author its model.
use super::*;

/// The model skills, as shipped under `skills/`, by folder name.
const SKILLS: [(&str, &str); 3] = [
    (
        "boundrail-model",
        include_str!("../../skills/boundrail-model/SKILL.md"),
    ),
    (
        "boundrail-plan",
        include_str!("../../skills/boundrail-plan/SKILL.md"),
    ),
    (
        "boundrail-pr",
        include_str!("../../skills/boundrail-pr/SKILL.md"),
    ),
];

/// Project files at the checkout root that show a language, and the allowlisted server for it.
const SERVERS: [(&[&str], &str); 5] = [
    (
        &["package.json", "tsconfig.json"],
        "typescript-language-server",
    ),
    (
        &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        "pyright-langserver",
    ),
    (&["Cargo.toml"], "rust-analyzer"),
    (&["go.mod"], "gopls"),
    (&["pom.xml", "build.gradle", "build.gradle.kts"], "jdtls"),
];

const NEXT_STEPS: [&str; 5] = [
    "boundrail model read",
    "boundrail model schema model-edit",
    "boundrail model edit --doing \"<why>\" --input <request.json>",
    "boundrail model check",
    "boundrail model view",
];

/// Each agent's project skill folder.
fn skill_dir(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => ".claude/skills",
        Agent::Codex => ".agents/skills",
        Agent::Cursor => ".cursor/skills",
        Agent::Opencode => ".opencode/skills",
    }
}

/// What setup did with each file, by repository path.
#[derive(Default, serde::Serialize)]
struct Placed {
    written: Vec<String>,
    unchanged: Vec<String>,
    /// Files that differ from what setup writes, left alone without --force.
    kept: Vec<String>,
}

impl Placed {
    /// Writes `text` to `relative` below `checkout` when the file is missing, or differs and
    /// `force` is set. Refuses a symbolic link on the way.
    fn place(&mut self, checkout: &Path, relative: &str, text: &str, force: bool) -> Result<()> {
        let path = boundrail::hook_agents::safe_path(checkout, relative)?;
        match fs::read(&path) {
            Ok(found) if found == text.as_bytes() => {
                self.unchanged.push(relative.to_owned());
                return Ok(());
            }
            Ok(_) if !force => {
                self.kept.push(relative.to_owned());
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("reading {relative}")),
        }
        let parent = path
            .parent()
            .with_context(|| format!("no folder holds {relative}"))?;
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        boundrail::history::write_atomic(&path, text.as_bytes())?;
        self.written.push(relative.to_owned());
        Ok(())
    }
}

/// The allowlisted servers of the languages whose project files sit at the checkout root.
fn detected_servers(checkout: &Path) -> Vec<&'static str> {
    SERVERS
        .iter()
        .filter(|(files, _)| files.iter().any(|f| checkout.join(f).is_file()))
        .map(|(_, server)| *server)
        .collect()
}

/// What `model setup` did.
#[derive(serde::Serialize)]
struct Report {
    vault: PathBuf,
    created: bool,
    #[serde(flatten)]
    placed: Placed,
    /// The servers written to the language-server list; `None` without --language-servers.
    language_servers: Option<Vec<&'static str>>,
    /// What setup left alone, and why.
    notes: Vec<String>,
    next_steps: Vec<String>,
}

pub(super) fn run(
    root: &Path,
    markdown: bool,
    name: Option<&str>,
    agents: &[Agent],
    language_servers: bool,
    force: bool,
) -> Result<()> {
    let (vault, created) = create_vault(root, name)?;
    // The vault is `<checkout>/.boundrail/model`.
    let checkout = (vault.ancestors().nth(2)).context("the vault lies below its checkout")?;
    let (placed, servers) = place_files(checkout, agents, language_servers, force)?;
    let mut notes = Vec::new();
    if name.is_some() && !created {
        notes.push("The vault exists, so --name was not used.".to_owned());
    }
    if !placed.kept.is_empty() {
        notes
            .push("Kept files differ from what setup writes; pass --force to replace them.".into());
    }
    if servers.as_ref().is_some_and(Vec::is_empty) {
        notes.push(
            "No language server: no project file at the checkout root names a language.".into(),
        );
    }
    let mut next_steps: Vec<String> = NEXT_STEPS.iter().map(|s| (*s).to_owned()).collect();
    if agents.is_empty() {
        next_steps.insert(0, "boundrail model setup --agent claude".to_owned());
    }
    let report = Report {
        vault,
        created,
        placed,
        language_servers: servers,
        notes,
        next_steps,
    };
    if markdown {
        print_report(&report);
        return Ok(());
    }
    let mut output = json!({"schema": MODEL_SCHEMA, "kind": "setup", "success": true});
    extend(&mut output, &report)?;
    write(None, &output)
}

/// Installs the skills for `agents` and, with `language_servers`, the server list of the
/// languages detected, which it returns.
fn place_files(
    checkout: &Path,
    agents: &[Agent],
    language_servers: bool,
    force: bool,
) -> Result<(Placed, Option<Vec<&'static str>>)> {
    let mut placed = Placed::default();
    for &agent in agents {
        for (skill, text) in SKILLS {
            let relative = format!("{}/{skill}/SKILL.md", skill_dir(agent));
            placed.place(checkout, &relative, text, force)?;
        }
    }
    let servers = language_servers.then(|| detected_servers(checkout));
    if let Some(servers) = servers.as_ref().filter(|s| !s.is_empty()) {
        let entries: Vec<Value> = servers.iter().map(|s| json!({"server": s})).collect();
        let text = format!("{}\n", serde_json::to_string_pretty(&entries)?);
        placed.place(checkout, lsp::CONFIG, &text, force)?;
    }
    Ok((placed, servers))
}

/// Creates the vault when absent, named `name` or after the checkout folder. Returns the vault
/// folder and whether setup created it.
fn create_vault(root: &Path, name: Option<&str>) -> Result<(PathBuf, bool)> {
    let vault = vault::path(root)?;
    let folder = (vault.ancestors().nth(2))
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("Model");
    match vault::init(root, name.unwrap_or(folder), false) {
        Ok(_) => Ok((vault, true)),
        Err(error)
            if error
                .downcast_ref::<Refusal>()
                .is_some_and(|r| r.code == Code::VaultExists) =>
        {
            Ok((vault, false))
        }
        Err(error) => Err(error),
    }
}

fn print_report(report: &Report) {
    let state = if report.created { "Created" } else { "Found" };
    println!("{state} the model vault at {}", report.vault.display());
    let placed = &report.placed;
    for (label, files) in [
        ("Wrote", &placed.written),
        ("Unchanged", &placed.unchanged),
        ("Kept", &placed.kept),
    ] {
        for file in files {
            println!("{label} {file}");
        }
    }
    for note in &report.notes {
        println!("{note}");
    }
    println!("Next steps:");
    for step in &report.next_steps {
        println!("- {step}");
    }
}
