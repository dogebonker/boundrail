//! The GitHub Action's metadata parses, its one step runs model commands with flags and formats
//! the binary accepts, and posting the comment is best effort.
use std::fs;
use std::path::Path;
use std::process::Command;

use boundrail::collect::TempDir;
use serde_yaml_ng::Value;

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

fn action() -> Value {
    serde_yaml_ng::from_str(&read("action.yml")).unwrap()
}

fn steps(action: &Value) -> &[Value] {
    action["runs"]["steps"].as_sequence().unwrap()
}

#[test]
fn the_action_is_one_model_step() {
    let action = action();
    assert_eq!(action["runs"]["using"], "composite");
    assert_eq!(action["inputs"]["fail-on"]["default"], "warnings");
    assert_eq!(action["inputs"]["gate"]["default"], "none");
    assert_eq!(action["inputs"]["token"]["default"], "");
    let path = action["inputs"]["path"]["description"].as_str().unwrap();
    assert!(path.contains("never passes --language-servers"), "{path}");
    let inputs: Vec<&str> = (action["inputs"].as_mapping().unwrap().keys())
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(
        inputs,
        [
            "path", "fail-on", "gate", "token", "base-ref", "version", "binary"
        ]
    );
    assert!(action.get("outputs").is_none());
    let steps = steps(&action);
    assert_eq!(steps.len(), 1);
    assert!(steps[0].get("if").is_none());
}

/// Each `boundrail model` invocation of the model script, as its words from the subcommand on.
fn invocations(code: &[&str]) -> Vec<Vec<String>> {
    (code.iter())
        .filter(|line| line.contains("\"$boundrail\" model "))
        .map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let start = words.iter().position(|w| *w == "model").unwrap() + 1;
            (words[start..].iter())
                .take_while(|w| !w.starts_with('>') && **w != "||")
                .map(|w| (*w).to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn the_step_runs_real_commands_and_flags_and_never_language_servers() {
    let action = action();
    let step = &steps(&action)[0];
    let run = step["run"].as_str().unwrap();
    assert_eq!(run, "\"$GITHUB_ACTION_PATH/ci/model-action.sh\"");
    let script = read("ci/model-action.sh");
    let code: Vec<&str> = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    assert!(
        code.iter().all(|line| !line.contains("--language-servers")),
        "the action must never run language servers"
    );

    // Every INPUT_* the script reads, the step sets.
    let env = step["env"].as_mapping().unwrap();
    for word in script.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if word.len() > "INPUT_".len() && word.starts_with("INPUT_") {
            assert!(env.contains_key(word), "the step does not set {word}");
        }
    }

    let invocations = invocations(&code);
    let commands: Vec<&str> = invocations.iter().map(|w| w[0].as_str()).collect();
    assert_eq!(commands, ["check", "footprint", "footprint"]);
    assert!(
        invocations[0].iter().any(|w| w == "--no-language-servers"),
        "a user's config must not turn language servers on in the action"
    );
    let outside = TempDir::new("action-format").unwrap();
    for words in &invocations {
        let help = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model", &words[0], "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        for word in words.iter().skip(1).filter(|w| w.starts_with("--")) {
            assert!(help.contains(&format!("{word} ")), "{}: {word}", words[0]);
        }
        // The command accepts its format and goes on to look for a checkout, which a folder
        // outside Git does not have.
        let at = words.iter().position(|w| w == "--format").unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model", &words[0], "--format", &words[at + 1]])
            .current_dir(outside.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(2), "{stdout}");
        assert!(stdout.contains("CHECKOUT_NOT_FOUND"), "{words:?}: {stdout}");
    }

    // The flags the footprint takes from a proposal marker exist too.
    let against: Vec<&str> = (code.iter())
        .filter(|line| line.contains("against=(") || line.contains("against+=("))
        .flat_map(|line| line.split(['(', ')', ' ']))
        .filter(|w| w.starts_with("--"))
        .collect();
    assert_eq!(against, ["--proposal", "--fail-outside"]);
    let help = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model", "footprint", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for flag in against {
        assert!(help.contains(&format!("{flag} ")), "footprint: {flag}");
    }
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=ci", "-c", "user.email=ci@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A pull request run whose token cannot write: the comment is not posted, the step still
/// passes with fail-on never, and only the posting step sees the token.
#[cfg(unix)]
#[test]
fn a_token_that_cannot_write_posts_no_comment_and_fails_nothing() {
    let dir = TempDir::new("action-post").unwrap();
    let (repo, bin) = (dir.path().join("repo"), dir.path().join("bin"));
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let seen = dir.path().join("seen");
    executable(
        &bin.join("boundrail"),
        &format!(
            "#!/usr/bin/env bash\necho \"boundrail ${{INPUT_TOKEN:-none}}\" >> '{}'\n\
             case \"$*\" in\n  --version) echo boundrail 0 ;;\n  \
             *'model check'*) printf '::warning file=a.ts,title=X::msg\\nnoise\\n'; exit 1 ;;\n  \
             *'model footprint'*) printf '<!-- boundrail:footprint -->\\nhi\\n' ;;\nesac\n",
            seen.display()
        ),
    );
    executable(
        &bin.join("gh"),
        &format!(
            "#!/usr/bin/env bash\necho \"gh $GH_TOKEN $*\" >> '{}'\n\
             case \"$*\" in\n  'api user'*) exit 1 ;;\n  \
             *'--method POST'*) echo 'HTTP 403: Resource not accessible by integration' >&2; exit 1 ;;\n\
             esac\n",
            seen.display()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/model-action.sh");
    let out = Command::new("bash")
        .arg(script)
        .env("PATH", path)
        .env("RUNNER_TEMP", dir.path().join("runner"))
        .env("GITHUB_STEP_SUMMARY", dir.path().join("summary.md"))
        .env("INPUT_BINARY", bin.join("boundrail"))
        .env("INPUT_PATH", &repo)
        .env("INPUT_BASE_REF", base.trim())
        .env("INPUT_FAIL_ON", "never")
        .env("INPUT_TOKEN", "secret")
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("PR_NUMBER", "7")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("::notice::footprint comment not posted: the token cannot write"),
        "{stdout}"
    );
    let seen = fs::read_to_string(seen).unwrap();
    assert!(
        seen.lines()
            .all(|l| !l.starts_with("boundrail") || l == "boundrail none")
    );
    assert!(seen.contains("gh secret api --method POST repos/owner/repo/issues/7/comments"));
    // The existing comment is looked up by the bot's login as well as the marker.
    assert!(seen.contains("github-actions[bot]"), "{seen}");
    let summary = fs::read_to_string(dir.path().join("summary.md")).unwrap();
    assert!(summary.contains("**Model check:** 1 warnings"), "{summary}");
}

/// fail-on defaults to warnings, an empty fail-on fails on nothing, and any other value is an
/// error.
#[cfg(unix)]
#[test]
fn fail_on_defaults_to_warnings_and_an_empty_fail_on_is_never() {
    let dir = TempDir::new("action-fail-on").unwrap();
    let binary = dir.path().join("boundrail");
    executable(
        &binary,
        "#!/usr/bin/env bash\ncase \"$*\" in\n  --version) echo boundrail 0 ;;\n  \
         *'model check'*) printf '::warning file=a.ts,title=X::msg\\n'; exit 1 ;;\nesac\n",
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/model-action.sh");
    for (fail_on, status) in [(None, 1), (Some(""), 0), (Some("new-forbidden"), 2)] {
        let mut command = Command::new("bash");
        command
            .arg(&script)
            .env("RUNNER_TEMP", dir.path().join("runner"))
            .env("GITHUB_STEP_SUMMARY", dir.path().join("summary.md"))
            .env("INPUT_BINARY", &binary)
            .env("INPUT_PATH", dir.path())
            .env_remove("INPUT_FAIL_ON")
            .env_remove("INPUT_BASE_REF");
        if let Some(fail_on) = fail_on {
            command.env("INPUT_FAIL_ON", fail_on);
        }
        let out = command.output().unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(status), "{fail_on:?}: {stdout}");
    }
}

/// A footprint cut at the comment limit goes whole into the summary, inside a tilde fence longer
/// than any tilde run the repository put in it.
#[cfg(unix)]
#[test]
fn the_summary_fence_outgrows_tilde_runs_in_the_footprint() {
    let dir = TempDir::new("action-fence").unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let binary = dir.path().join("boundrail");
    executable(
        &binary,
        "#!/usr/bin/env bash\ncase \"$*\" in\n  --version) echo boundrail 0 ;;\n  \
         *pr-comment*) printf 'cut\\n_Truncated at 60,000 characters._\\n' ;;\n  \
         *'model footprint'*) printf 'before\\n~~~~~~~~\\nafter\\n' ;;\nesac\n",
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/model-action.sh");
    let out = Command::new("bash")
        .arg(script)
        .env("RUNNER_TEMP", dir.path().join("runner"))
        .env("GITHUB_STEP_SUMMARY", dir.path().join("summary.md"))
        .env("INPUT_BINARY", &binary)
        .env("INPUT_PATH", &repo)
        .env("INPUT_BASE_REF", base.trim())
        .env("INPUT_FAIL_ON", "never")
        .env("GITHUB_EVENT_NAME", "push")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let summary = fs::read_to_string(dir.path().join("summary.md")).unwrap();
    let fence = "~".repeat(9);
    assert!(
        summary.contains(&format!("\n{fence}\nbefore\n~~~~~~~~\nafter\n{fence}\n")),
        "{summary}"
    );
}

/// A proposal marker in the pull request body runs the footprint against that proposal, and
/// gate: outside adds --fail-outside, whose exit 1 fails the step. The body is untrusted: a marker
/// whose ID is not 8 letters or digits, or a second marker, stops the step before the footprint
/// runs and nothing in it is evaluated. Without a token, gate: outside cannot read the body.
#[cfg(unix)]
#[test]
fn a_proposal_marker_sets_the_footprint_against_its_proposal_and_the_gate_fails_outside() {
    let dir = TempDir::new("action-marker").unwrap();
    let (repo, bin) = (dir.path().join("repo"), dir.path().join("bin"));
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let (seen, body) = (dir.path().join("seen"), dir.path().join("body"));
    executable(
        &bin.join("boundrail"),
        &format!(
            "#!/usr/bin/env bash\necho \"$*\" >> '{}'\n\
             case \"$*\" in\n  --version) echo boundrail 0 ;;\n  \
             *'model check'*) ;;\n  \
             *--fail-outside*) printf '<!-- boundrail:footprint -->\\n'; exit 1 ;;\n  \
             *'model footprint'*) printf '<!-- boundrail:footprint -->\\n' ;;\nesac\n",
            seen.display()
        ),
    );
    executable(
        &bin.join("gh"),
        &format!(
            "#!/usr/bin/env bash\ncase \"$*\" in\n  'api user'*) exit 1 ;;\n  \
             *pulls/7*) cat '{}' ;;\nesac\n",
            body.display()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/model-action.sh");
    let run = |text: &str, gate: Option<&str>, token: &str| {
        fs::write(&body, text).unwrap();
        let _ = fs::remove_file(&seen);
        let mut command = Command::new("bash");
        command
            .arg(&script)
            .current_dir(dir.path())
            .env("PATH", &path)
            .env("RUNNER_TEMP", dir.path().join("runner"))
            .env("GITHUB_STEP_SUMMARY", dir.path().join("summary.md"))
            .env("INPUT_BINARY", bin.join("boundrail"))
            .env("INPUT_PATH", &repo)
            .env("INPUT_BASE_REF", base.trim())
            .env("INPUT_FAIL_ON", "warnings")
            .env("INPUT_TOKEN", token)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_REPOSITORY", "owner/repo")
            .env("PR_NUMBER", "7")
            .env_remove("INPUT_GATE");
        if let Some(gate) = gate {
            command.env("INPUT_GATE", gate);
        }
        let out = command.output().unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let footprints: Vec<String> = fs::read_to_string(&seen)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("model footprint"))
            .map(str::to_owned)
            .collect();
        (out.status.code(), stdout, footprints)
    };
    let range = format!("--root {} --base {}", repo.display(), base.trim());
    let marked = "Summary\r\n\r\n<!-- boundrail:proposal Ab3dE5gH -->\r\n";

    let (status, stdout, footprints) = run(marked, None, "secret");
    assert_eq!(status, Some(0), "{stdout}");
    assert!(
        stdout.contains("footprint against proposal Ab3dE5gH"),
        "{stdout}"
    );
    assert_eq!(
        footprints,
        [format!(
            "model footprint {range} --proposal Ab3dE5gH --format pr-comment"
        )]
    );

    let (status, stdout, footprints) = run(marked, Some("outside"), "secret");
    assert_eq!(status, Some(1), "{stdout}");
    assert_eq!(
        footprints,
        [format!(
            "model footprint {range} --proposal Ab3dE5gH --fail-outside --format pr-comment"
        )]
    );
    let summary = fs::read_to_string(dir.path().join("summary.md")).unwrap();
    assert!(
        summary.contains("<!-- boundrail:footprint -->"),
        "{summary}"
    );

    // Under outside, a pull request without a marker fails; under none it runs as before.
    let (status, stdout, footprints) = run("No marker.\n", Some("outside"), "secret");
    assert_eq!(status, Some(2), "{stdout}");
    assert!(
        stdout.contains(
            "::error::boundrail: gate outside needs the pull request body to hold a marker"
        ),
        "{stdout}"
    );
    assert_eq!(footprints, Vec::<String>::new());
    let (status, stdout, footprints) = run("No marker.\n", None, "secret");
    assert_eq!(status, Some(0), "{stdout}");
    assert_eq!(
        footprints,
        [format!("model footprint {range} --format pr-comment")]
    );

    // A marker line inside a fenced code block is a quotation, not a marker.
    let quoted = "Example:\n```\n<!-- boundrail:proposal Ab3dE5gH -->\n```\n";
    let (status, stdout, _) = run(quoted, Some("outside"), "secret");
    assert_eq!(status, Some(2), "{stdout}");
    let (status, stdout, footprints) = run(quoted, None, "secret");
    assert_eq!(status, Some(0), "{stdout}");
    assert_eq!(
        footprints,
        [format!("model footprint {range} --format pr-comment")]
    );

    let pwned = dir.path().join("pwned");
    for text in [
        "<!-- boundrail:proposal $(touch pwned) -->\n",
        "<!-- boundrail:proposal `touch pwned` -->\n",
        "<!-- boundrail:proposal Ab3dE5gH; touch pwned -->\n",
        "<!-- boundrail:proposal Ab3dE5gH9 -->\n",
        "<!-- boundrail:proposal ../x/../y -->\n",
        "<!-- boundrail:proposal <proposal> -->\n",
        "<!-- boundrail:proposal Ab3dE5gH -->\n<!-- boundrail:proposal Zz9yY8xX -->\n",
    ] {
        let (status, stdout, footprints) = run(text, None, "secret");
        assert_eq!(status, Some(2), "{text}: {stdout}");
        assert!(
            stdout.contains("::error::boundrail: the pull request body needs one marker line"),
            "{stdout}"
        );
        assert!(!stdout.contains("touch"), "{stdout}");
        assert_eq!(footprints, Vec::<String>::new(), "{text}");
        assert!(!pwned.exists(), "{text}");
    }

    let (status, stdout, _) = run(marked, Some("outside"), "");
    assert_eq!(status, Some(2), "{stdout}");
    assert!(
        stdout.contains("could not read the pull request body"),
        "{stdout}"
    );
    let (status, stdout, footprints) = run(marked, None, "");
    assert_eq!(status, Some(0), "{stdout}");
    assert!(
        stdout.contains("::notice::pull request body not read"),
        "{stdout}"
    );
    assert_eq!(
        footprints,
        [format!("model footprint {range} --format pr-comment")]
    );

    // Under outside, an event that is not pull_request fails: the gate did not run.
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(dir.path())
        .env("PATH", &path)
        .env("RUNNER_TEMP", dir.path().join("runner"))
        .env("GITHUB_STEP_SUMMARY", dir.path().join("summary.md"))
        .env("INPUT_BINARY", bin.join("boundrail"))
        .env("INPUT_PATH", &repo)
        .env("INPUT_GATE", "outside")
        .env("GITHUB_EVENT_NAME", "pull_request_target")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stdout}");
    assert!(
        stdout.contains("gate outside runs on pull_request events only"),
        "{stdout}"
    );

    let (status, stdout, _) = run(marked, Some("everything"), "secret");
    assert_eq!(status, Some(2), "{stdout}");
    assert!(stdout.contains("gate must be none or outside"), "{stdout}");
}
