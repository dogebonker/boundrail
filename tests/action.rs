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
    assert_eq!(action["inputs"]["token"]["default"], "");
    let path = action["inputs"]["path"]["description"].as_str().unwrap();
    assert!(path.contains("never passes --language-servers"), "{path}");
    let inputs: Vec<&str> = (action["inputs"].as_mapping().unwrap().keys())
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(
        inputs,
        ["path", "fail-on", "token", "base-ref", "version", "binary"]
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
