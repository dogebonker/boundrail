//! `model language-server`: lists, adds and removes language servers in the repository's list and
//! the user's config.
use super::*;

#[derive(Subcommand)]
pub(super) enum LanguageServerCmd {
    /// Print the repository's list, the user's config and the servers a run with language
    /// servers on would start, per language
    List,
    /// Name a language's server. The repository's list only names an allowlisted server, run with
    /// fixed arguments: --command picks one of the language's, else its default. A command or
    /// arguments go in the user's config with --user
    Add {
        /// c (C, C++, Objective-C), csharp, go, java, python, rust, swift or typescript
        language: String,
        /// The program: with --user a name on PATH or an absolute path; without, an allowlisted
        /// server of the language
        #[arg(long)]
        command: Option<String>,
        /// One argument of the command (repeatable); only with --user
        #[arg(long = "arg", requires = "command", allow_hyphen_values = true)]
        args: Vec<String>,
        /// Write the user's config, outside the checkout, instead of the repository's list
        #[arg(long)]
        user: bool,
    },
    /// Remove a language's allowlisted servers from the repository's list, deleting it when
    /// empty, or with --user from the user's config
    Remove {
        /// c, csharp, go, java, python, rust, swift or typescript
        language: String,
        /// Edit the user's config instead of the repository's list
        #[arg(long)]
        user: bool,
    },
}

pub(super) fn run(root: &Path, markdown: bool, command: LanguageServerCmd) -> Result<()> {
    let checkout = &checkpoint::checkout(&vault::path(root)?)?;
    let edited = match command {
        LanguageServerCmd::List => None,
        LanguageServerCmd::Add {
            language,
            command,
            args,
            user,
        } => Some(lsp::add(
            checkout,
            &language,
            command.as_deref(),
            &args,
            user,
        )?),
        LanguageServerCmd::Remove { language, user } => {
            Some(lsp::remove(checkout, &language, user)?)
        }
    };
    let listing = lsp::listing(checkout);
    if markdown {
        if let Some(edited) = &edited {
            let change = match edited.change {
                lsp::Change::Written => "Wrote",
                lsp::Change::Deleted => "Deleted",
                lsp::Change::Unchanged => "Unchanged",
            };
            println!("{change} {}", edited.file.display());
        }
        print_listing(&listing);
        return Ok(());
    }
    let mut output = json!({"schema": MODEL_SCHEMA, "kind": "language-servers", "success": true});
    if let Some(edited) = edited {
        output["edited"] = json!(edited);
    }
    extend(&mut output, &listing)?;
    write(None, &output)
}

fn print_listing(listing: &lsp::Listing) {
    let shown = |file: &Option<PathBuf>| {
        file.as_ref().map_or_else(
            || "(no user config folder)".to_owned(),
            |f| f.display().to_string(),
        )
    };
    let repository = &listing.repository;
    println!("Repository list {}:", shown(&repository.file));
    for entry in &repository.content {
        println!("- {}", entry.server);
    }
    let user = &listing.user;
    let trusted = &user.content.trusted;
    let on = if trusted.is_empty() {
        "off unless --language-servers".to_owned()
    } else {
        let folders: Vec<String> = trusted.iter().map(|f| f.display().to_string()).collect();
        format!("on without --language-servers below {}", folders.join(", "))
    };
    println!("User config {} (language servers {on}):", shown(&user.file));
    for server in &user.content.servers {
        let line =
            iter::once(server.command.as_str()).chain(server.args.iter().map(String::as_str));
        println!("- {}: {}", server.name, line.collect::<Vec<_>>().join(" "));
    }
    for problem in [&repository.problem, &user.problem, &listing.problem]
        .into_iter()
        .flatten()
    {
        println!("Problem: {problem}");
    }
    println!("Effective:");
    for e in &listing.effective {
        let line = iter::once(e.command.as_str()).chain(e.args.iter().map(String::as_str));
        let by = match e.defined_by {
            lsp::DefinedBy::Allowlist => "allowlist",
            lsp::DefinedBy::User => "user",
        };
        println!(
            "- {}: {} ({by}): {}",
            e.language,
            e.server,
            line.collect::<Vec<_>>().join(" ")
        );
    }
}
