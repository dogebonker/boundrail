use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use boundrail::collect::TempDir;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A temporary directory, a Git checkout unless made with `outside`.
struct Checkout(TempDir);
impl Checkout {
    fn new() -> Self {
        let checkout = Self::outside();
        let out = Command::new("git")
            .args(["init", "-q"])
            .current_dir(checkout.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        checkout
    }
    fn outside() -> Self {
        Self(TempDir::new("arch-model").unwrap())
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn vault(&self) -> PathBuf {
        self.path().join(".boundrail/model")
    }
    /// Exit code and stdout parsed as JSON.
    fn run(&self, args: &[&str]) -> (i32, Value) {
        let (code, stdout) = self.run_raw(args);
        let value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
        (code, value)
    }
    fn run_raw(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .arg("model")
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
        )
    }
    /// `run` with the fake language server on PATH and `user` as the user's config folder: exit
    /// code, stdout as JSON, and stderr.
    fn run_served(&self, user: &Path, args: &[&str]) -> (i32, Value, String) {
        self.run_served_after(&[fake_server_dir()], user, args)
    }
    /// `run_served` with `dirs` ahead of the rest of PATH.
    fn run_served_after(&self, dirs: &[&Path], user: &Path, args: &[&str]) -> (i32, Value, String) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = dirs.iter().map(|d| d.to_path_buf());
        let path = std::env::join_paths(dirs.chain(std::env::split_paths(&path))).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model"])
            .args(args)
            .current_dir(self.path())
            .env("PATH", path)
            .env("XDG_CONFIG_HOME", user)
            .env("BOUNDRAIL_FAKE_LSP_SCRIPT", user.join(SCRIPT))
            .output()
            .unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
        let stderr = String::from_utf8(out.stderr).unwrap();
        (out.status.code().unwrap(), value, stderr)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (code, value) = self.run(args);
        assert_eq!(code, 0, "{value:#}");
        assert_eq!(value["success"], true);
        assert_eq!(value["schema"], "boundrail/model/v1");
        value
    }
    fn refused(&self, args: &[&str], expected: &str) -> Value {
        let (code, value) = self.run(args);
        assert_eq!(code, 2, "{value:#}");
        assert_eq!(value["success"], false);
        assert_eq!(value["schema"], "boundrail/model/v1");
        assert_eq!(value["kind"], "refusal");
        assert_eq!(value["error"]["code"], expected, "{value:#}");
        value["error"].clone()
    }
    /// `model init` and the new model's ID.
    fn init(&self) -> String {
        let out = self.ok(&["init", "--name", "Shop"]);
        out["manifest"]["id"].as_str().unwrap().to_owned()
    }
    fn write(&self, path: &str, text: &str) {
        let file = self.vault().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    fn module(&self, value: Value) {
        let id = value["id"].as_str().unwrap().to_owned();
        self.write(
            &format!("modules/{id}.module.json"),
            &serde_json::to_string_pretty(&value).unwrap(),
        );
    }
    fn read_module(&self, id: &str) -> Value {
        let path = self.vault().join(format!("modules/{id}.module.json"));
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }
    fn digest(&self) -> Value {
        self.ok(&["read"])["revision"]["digest"].clone()
    }
    /// An edit request against the current model and revision with a fresh operation ID.
    fn request(&self, changes: Value) -> Value {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let read = self.ok(&["read"]);
        json!({
            "target": {"model": read["manifest"]["id"], "state": read["manifest"]["current"]["id"]},
            "expected_revision": read["revision"]["digest"],
            "operation_id": format!("op-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            "writer": "tester",
            "changes": changes,
        })
    }
    /// A model command with `input` on standard input: exit code and stdout parsed as JSON.
    fn pipe(&self, args: &[&str], input: &Value) -> (i32, Value) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model"])
            .args(args)
            .current_dir(self.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.to_string().as_bytes()).unwrap();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
        (out.status.code().unwrap(), value)
    }
    /// `model claim` by `writer` on `subjects`: the exit code and the output.
    fn try_claim(&self, writer: &str, subjects: Value) -> (i32, Value) {
        let read = self.ok(&["read"]);
        let request = json!({
            "target": {"model": read["manifest"]["id"], "state": read["manifest"]["current"]["id"]},
            "writer": writer,
            "reason": format!("{writer} works here"),
            "subjects": subjects,
        });
        self.pipe(&["claim", "--doing", "reserve my modules"], &request)
    }
    /// The claim `model claim` records for `writer` on `subjects`.
    fn claim(&self, writer: &str, subjects: Value) -> Value {
        let (code, out) = self.try_claim(writer, subjects);
        assert_eq!(code, 0, "{out:#}");
        assert_eq!(out["kind"], "claim");
        out["claim"].clone()
    }
    fn claim_refused(&self, writer: &str, subjects: Value, expected: &str) -> Value {
        let (code, out) = self.try_claim(writer, subjects);
        assert_eq!(code, 2, "{out:#}");
        assert_eq!(out["error"]["code"], expected, "{out:#}");
        out["error"].clone()
    }
    fn release(&self, claim: &Value, writer: &str) -> (i32, Value) {
        self.pipe(
            &["release"],
            &json!({"claim": claim["id"], "writer": writer}),
        )
    }
    fn claims(&self) -> Value {
        self.ok(&["claims"])["claims"].clone()
    }
    /// An edit request by `writer`, naming `claim` when given.
    fn request_by(&self, writer: &str, claim: Option<&Value>, changes: Value) -> Value {
        let mut request = self.request(changes);
        request["writer"] = writer.into();
        if let Some(claim) = claim {
            request["claim"] = claim["id"].clone();
        }
        request
    }
    /// `model edit` with `request` on standard input.
    fn edit_with(&self, request: &Value, doing: &str, env: &[(&str, &str)]) -> (i32, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model", "edit", "--doing", doing])
            .envs(env.iter().copied())
            .current_dir(self.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.to_string().as_bytes()).unwrap();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
        )
    }
    fn edit(&self, request: &Value) -> (i32, Value) {
        let (code, stdout) = self.edit_with(request, "testing edits", &[]);
        let value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
        (code, value)
    }
    fn edited(&self, changes: Value) -> Value {
        let (code, value) = self.edit(&self.request(changes));
        assert_eq!(code, 0, "{value:#}");
        assert_eq!(value["kind"], "edit");
        assert_eq!(value["success"], true);
        value
    }
    /// Refuses the request with `expected`, leaving the revision and the lock untouched.
    fn edit_refused(&self, request: &Value, expected: &str) -> Value {
        let before = self.digest();
        let (code, value) = self.edit(request);
        assert_eq!(code, 2, "{value:#}");
        assert_eq!(value["kind"], "refusal");
        assert_eq!(value["error"]["code"], expected, "{value:#}");
        assert_eq!(self.digest(), before, "a refused edit wrote the vault");
        value["error"].clone()
    }
}

fn module(id: &str, name: &str, kind: &str, order: u32) -> Value {
    json!({"schema_version": 1, "id": id, "name": name, "kind": kind, "order": order})
}

/// Platform (system) holds Web (order 1) and Billing (order 0); Bank (external) is a second
/// root. Billing calls Bank; Web calls Billing over HTTP.
fn shop(checkout: &Checkout) -> String {
    let model = checkout.init();
    let q = |id: &str| format!("{model}#{id}");
    checkout.module(module("Platfrm1", "Platform", "system", 0));
    let mut web = module("WebApp01", "Web", "app", 1);
    web["parent"] = q("Platfrm1").into();
    web["former_names"] = json!(["Storefront"]);
    web["outgoing"] = json!([{"id": "RelWeb01", "to": q("Billing1"), "kind": "http", "order": 0, "label": "charges"}]);
    checkout.module(web);
    let mut billing = module("Billing1", "Billing", "service", 0);
    billing["parent"] = q("Platfrm1").into();
    billing["responsibility"] = "Charges customers".into();
    billing["outgoing"] =
        json!([{"id": "RelBil01", "to": q("BankApi1"), "kind": "call", "order": 0}]);
    checkout.module(billing);
    checkout.module(module("BankApi1", "Bank", "external", 1));
    model
}

#[test]
fn init_creates_the_vault_with_a_stable_revision() {
    let checkout = Checkout::new();
    fs::create_dir_all(checkout.path().join("src/deep")).unwrap();
    let out = checkout.ok(&["--root", "src/deep", "init", "--name", "Shop"]);
    let vault = checkout.vault();
    assert!(vault.join("model.json").is_file());
    assert!(vault.join("config.yaml").is_file());
    assert!(vault.join("modules").is_dir());
    assert_eq!(
        Path::new(out["vault"].as_str().unwrap()),
        vault.canonicalize().unwrap()
    );

    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(vault.join("model.json")).unwrap()).unwrap();
    assert_eq!(manifest, out["manifest"]);
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["name"], "Shop");
    assert_eq!(manifest["current"]["name"], "Current");
    assert_eq!(manifest["retired_ids"], json!([]));
    for id in [&manifest["id"], &manifest["current"]["id"]] {
        assert!(boundrail::arch::ids::is_local(id.as_str().unwrap()), "{id}");
    }
    assert!(
        fs::read_to_string(vault.join("model.json"))
            .unwrap()
            .ends_with("}\n")
    );

    let config: Value =
        serde_yaml_ng::from_str(&fs::read_to_string(vault.join("config.yaml")).unwrap()).unwrap();
    assert_eq!(config["node-kinds"].as_object().unwrap().len(), 16);
    assert_eq!(config["node-kinds"]["datastore"]["name"], "Datastore");
    let evidence = |kind: &str| config["relationship-kinds"][kind]["evidence"].clone();
    assert_eq!(evidence("call"), json!(["call"]));
    assert_eq!(evidence("http"), json!(["call", "configured-path"]));
    assert_eq!(
        evidence("dependency"),
        json!(["call", "configured-path", "import", "build-dependency"])
    );
    assert_eq!(evidence("other"), json!([]));
    assert_eq!(config["groups"], json!({}));

    let revision = &out["revision"];
    let paths: Vec<_> = revision["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["config.yaml", "model.json"]);
    let lines: String = revision["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let path = f["path"].as_str().unwrap();
            let sha = sha256(&fs::read(vault.join(path)).unwrap());
            assert_eq!(f["sha256"], sha);
            format!("{path}\0{sha}\n")
        })
        .collect();
    assert_eq!(revision["digest"], sha256(lines.as_bytes()));
    assert_eq!(checkout.ok(&["read"])["revision"], *revision);
    assert_eq!(checkout.ok(&["read"])["revision"], *revision);
}

#[test]
fn init_refuses_an_existing_vault_and_read_needs_one() {
    let checkout = Checkout::new();
    checkout.refused(&["read"], "VAULT_NOT_FOUND");
    let model = checkout.init();
    let before = fs::read(checkout.vault().join("model.json")).unwrap();
    checkout.refused(&["init", "--name", "Other"], "VAULT_EXISTS");
    assert_eq!(
        fs::read(checkout.vault().join("model.json")).unwrap(),
        before
    );
    assert_eq!(checkout.ok(&["read"])["manifest"]["id"], model);

    let outside = Checkout::outside();
    outside.refused(&["init", "--name", "Shop"], "CHECKOUT_NOT_FOUND");
    assert!(!outside.vault().exists());

    // An empty vault folder already claims the vault.
    let claimed = Checkout::new();
    fs::create_dir_all(claimed.vault()).unwrap();
    claimed.refused(&["init", "--name", "Shop"], "VAULT_EXISTS");
    assert_eq!(fs::read_dir(claimed.vault()).unwrap().count(), 0);
}

#[test]
fn init_refuses_a_blank_or_multiline_name_and_writes_nothing() {
    let checkout = Checkout::new();
    for name in ["", "   ", "Shop\nFloor"] {
        checkout.refused(&["init", "--name", name], "NAME_INVALID");
        assert!(!checkout.path().join(".boundrail").exists(), "{name:?}");
    }
    let out = checkout.ok(&["init", "--name", "  Shop  "]);
    assert_eq!(out["kind"], "init");
    assert_eq!(out["manifest"]["name"], "Shop");
}

#[test]
fn setup_creates_the_vault_and_skills_once_and_keeps_edited_files() {
    let checkout = Checkout::new();
    for file in ["package.json", "Cargo.toml"] {
        fs::write(checkout.path().join(file), "{}\n").unwrap();
    }
    let args = [
        "setup",
        "--agent",
        "claude",
        "--agent",
        "codex",
        "--language-servers",
    ];
    let skills = |dir: &str| {
        ["boundrail-model", "boundrail-plan", "boundrail-pr"]
            .map(|skill| format!("{dir}/{skill}/SKILL.md"))
    };
    let mut files: Vec<String> = skills(".claude/skills").into();
    files.extend(skills(".agents/skills"));
    files.push(".boundrail/language-servers.json".into());

    let first = checkout.ok(&args);
    assert_eq!(first["kind"], "setup");
    assert_eq!(first["created"], true);
    assert_eq!(first["written"], json!(files));
    assert_eq!(
        first["language_servers"],
        json!(["typescript-language-server", "rust-analyzer"])
    );
    assert_eq!(first["next_steps"][0], "boundrail model read");
    let shipped = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/boundrail-model/SKILL.md"),
    )
    .unwrap();
    let installed = checkout
        .path()
        .join(".claude/skills/boundrail-model/SKILL.md");
    assert_eq!(fs::read_to_string(&installed).unwrap(), shipped);
    let servers = fs::read_to_string(checkout.path().join(".boundrail/language-servers.json"));
    let servers: Value = serde_json::from_str(&servers.unwrap()).unwrap();
    assert_eq!(
        servers,
        json!([{"server": "typescript-language-server"}, {"server": "rust-analyzer"}])
    );
    let digest = checkout.digest();

    let second = checkout.ok(&args);
    assert_eq!(second["created"], false);
    assert_eq!(second["written"], json!([]));
    assert_eq!(second["unchanged"], json!(files));
    assert_eq!(second["notes"], json!([]));
    assert_eq!(checkout.digest(), digest);
    let renamed = checkout.ok(&["setup", "--name", "Other"]);
    assert_eq!(
        renamed["notes"],
        json!(["The vault exists, so --name was not used."])
    );
    assert_eq!(checkout.digest(), digest);

    fs::write(&installed, "my notes\n").unwrap();
    let kept = checkout.ok(&args);
    assert_eq!(kept["kept"], json!([files[0]]));
    assert_eq!(fs::read_to_string(&installed).unwrap(), "my notes\n");
    let mut forced = args.to_vec();
    forced.push("--force");
    let replaced = checkout.ok(&forced);
    assert_eq!(replaced["written"], json!([files[0]]));
    assert_eq!(fs::read_to_string(&installed).unwrap(), shipped);

    // Without --agent setup installs nothing and says how to.
    let bare = Checkout::new();
    let out = bare.ok(&["setup", "--name", "Shop"]);
    assert_eq!(out["written"], json!([]));
    assert_eq!(out["language_servers"], Value::Null);
    assert_eq!(out["next_steps"][0], "boundrail model setup --agent claude");
    assert_eq!(bare.ok(&["read"])["manifest"]["name"], "Shop");
}

#[test]
fn read_an_empty_model() {
    let checkout = Checkout::new();
    let model = checkout.init();
    let out = checkout.ok(&["read"]);
    assert_eq!(out["manifest"]["id"], model);
    assert_eq!(out["roots"], json!([]));
    assert_eq!(out["modules"], json!([]));
    assert_eq!(out["relationships"], json!([]));
    assert_eq!(
        out["config"]["relationship-kinds"]["event"]["dash"],
        "dashed"
    );
}

#[test]
fn read_returns_containment_and_relationships_in_order() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let out = checkout.ok(&["read"]);
    assert_eq!(out["roots"], json!([q("Platfrm1"), q("BankApi1")]));
    let ids: Vec<_> = out["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [q("Platfrm1"), q("Billing1"), q("WebApp01"), q("BankApi1")]
    );
    let platform = &out["modules"][0];
    assert_eq!(platform["children"], json!([q("Billing1"), q("WebApp01")]));
    assert!(platform.get("parent").is_none());
    let billing = &out["modules"][1];
    assert_eq!(billing["parent"], q("Platfrm1"));
    assert_eq!(billing["responsibility"], "Charges customers");
    assert_eq!(billing["children"], json!([]));
    assert_eq!(
        out["relationships"],
        json!([
            {"id": q("RelBil01"), "from": q("Billing1"), "to": q("BankApi1"), "kind": "call", "order": 0},
            {"id": q("RelWeb01"), "from": q("WebApp01"), "to": q("Billing1"), "kind": "http", "order": 0, "label": "charges"},
        ])
    );

    let (code, text) = checkout.run_raw(&["read", "--format", "markdown"]);
    assert_eq!(code, 0);
    assert!(text.contains("  - Billing [service]"), "{text}");
    assert!(text.contains("Web -> Billing [http] charges"), "{text}");
}

#[test]
fn every_invalid_model_is_refused_with_its_problems() {
    type Break = fn(&Checkout, &str);
    let cases: [(&str, Break, &str, &str, &str); 16] = [
        (
            "module JSON",
            |c, _| c.write("modules/Broken01.module.json", "{ not json"),
            "modules/Broken01.module.json",
            "Broken01",
            "does not parse",
        ),
        (
            "config YAML",
            |c, _| c.write("config.yaml", "node-kinds: [unclosed"),
            "config.yaml",
            "config.yaml",
            "does not parse",
        ),
        (
            "duplicate ID",
            |c, m| {
                let mut extra = module("Extra001", "Extra", "service", 0);
                extra["outgoing"] = json!([{"id": "Billing1", "to": format!("{m}#BankApi1"), "kind": "call", "order": 0}]);
                c.module(extra);
            },
            "modules/Extra001.module.json",
            "Billing1",
            "also used in",
        ),
        (
            "retired ID",
            |c, _| {
                let path = c.vault().join("model.json");
                let mut manifest: Value =
                    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                manifest["retired_ids"] = json!(["BankApi1"]);
                fs::write(path, manifest.to_string()).unwrap();
            },
            "modules/BankApi1.module.json",
            "BankApi1",
            "retired",
        ),
        (
            "missing parent",
            |c, m| {
                let mut orphan = module("Orphan01", "Orphan", "service", 0);
                orphan["parent"] = format!("{m}#Missing1").into();
                c.module(orphan);
            },
            "modules/Orphan01.module.json",
            "Orphan01",
            "parent",
        ),
        (
            "missing target",
            |c, m| {
                let mut dangling = module("Dangle01", "Dangling", "service", 0);
                dangling["outgoing"] = json!([{"id": "RelDan01", "to": format!("{m}#Missing1"), "kind": "call", "order": 0}]);
                c.module(dangling);
            },
            "modules/Dangle01.module.json",
            "RelDan01",
            "target",
        ),
        (
            "containment cycle",
            |c, m| {
                let mut a = module("CycleA01", "A", "module", 0);
                a["parent"] = format!("{m}#CycleB01").into();
                let mut b = module("CycleB01", "B", "module", 0);
                b["parent"] = format!("{m}#CycleA01").into();
                c.module(a);
                c.module(b);
            },
            "modules/CycleA01.module.json",
            "CycleA01",
            "cycle",
        ),
        (
            "node kind",
            |c, _| c.module(module("Lambda01", "Lambda", "lambda", 0)),
            "modules/Lambda01.module.json",
            "Lambda01",
            "node kind",
        ),
        (
            "relationship kind",
            |c, m| {
                let mut odd = module("Oddrel01", "Odd", "service", 0);
                odd["outgoing"] = json!([{"id": "RelOdd01", "to": format!("{m}#BankApi1"), "kind": "telepathy", "order": 0}]);
                c.module(odd);
            },
            "modules/Oddrel01.module.json",
            "RelOdd01",
            "relationship kind",
        ),
        (
            "responsibility length",
            |c, _| {
                let mut long = module("LongRes1", "Long", "service", 0);
                long["responsibility"] = "x".repeat(201).into();
                c.module(long);
            },
            "modules/LongRes1.module.json",
            "LongRes1",
            "responsibility has 201 characters",
        ),
        (
            "description length",
            |c, _| {
                let mut long = module("LongDsc1", "Long", "service", 0);
                long["description"] = "é".repeat(2001).into();
                c.module(long);
            },
            "modules/LongDsc1.module.json",
            "LongDsc1",
            "description has 2001 characters",
        ),
        (
            "label length",
            |c, m| {
                let mut long = module("LongLbl1", "Long", "service", 0);
                long["outgoing"] = json!([{"id": "RelLng01", "to": format!("{m}#BankApi1"), "kind": "call", "order": 0, "label": "x".repeat(61)}]);
                c.module(long);
            },
            "modules/LongLbl1.module.json",
            "RelLng01",
            "label has 61 characters",
        ),
        (
            "escape sequence in a name",
            |c, _| c.module(module("Escape01", "Red\u{1b}[31m", "service", 0)),
            "modules/Escape01.module.json",
            "Escape01",
            "name needs 1 to 120 characters",
        ),
        (
            "tab in a label",
            |c, m| {
                let mut tab = module("TabLbl01", "Tab", "service", 0);
                tab["outgoing"] = json!([{"id": "RelTab01", "to": format!("{m}#BankApi1"), "kind": "call", "order": 0, "label": "a\tb"}]);
                c.module(tab);
            },
            "modules/TabLbl01.module.json",
            "RelTab01",
            "label contains a control character",
        ),
        (
            "unknown step field",
            |c, m| {
                let step = json!({"kind": "message", "id": "Step0001", "from": format!("{m}#WebApp01"), "to": format!("{m}#Billing1"), "label": "pay", "colour": "red"});
                let collection = json!({"schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0, "flows": [{"id": "Flow0001", "name": "Pay", "order": 0, "steps": [step]}]});
                c.write(
                    "collections/Coll0001.collection.json",
                    &collection.to_string(),
                );
            },
            "collections/Coll0001.collection.json",
            "Coll0001",
            "does not parse: unknown field `colour`",
        ),
        (
            "step naming a missing module",
            |c, m| {
                let step = json!({"kind": "message", "id": "Step0001", "from": format!("{m}#WebApp01"), "to": format!("{m}#Missing1"), "label": "pay"});
                let collection = json!({"schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0, "flows": [{"id": "Flow0001", "name": "Pay", "order": 0, "steps": [step]}]});
                c.write(
                    "collections/Coll0001.collection.json",
                    &collection.to_string(),
                );
            },
            "collections/Coll0001.collection.json",
            "Step0001",
            "is not a module",
        ),
    ];
    for (case, breaks, file, subject, message) in cases {
        let checkout = Checkout::new();
        let model = shop(&checkout);
        breaks(&checkout, &model);
        let error = checkout.refused(&["read"], "MODEL_INVALID");
        let problems = error["details"]["problems"].as_array().unwrap();
        assert!(
            problems.iter().any(|p| p["file"] == file
                && p["subject"] == subject
                && p["message"].as_str().unwrap().contains(message)),
            "{case}: {problems:#?}"
        );
    }

    // Limits are inclusive, and every problem is listed at once.
    let checkout = Checkout::new();
    shop(&checkout);
    let mut fits = module("Fits0001", "Fits", "service", 0);
    fits["responsibility"] = "x".repeat(200).into();
    fits["description"] = "Line one\n\tLine two".into();
    checkout.module(fits);
    checkout.ok(&["read"]);
    checkout.module(module("Lambda01", "Lambda", "lambda", 0));
    checkout.write("modules/Broken01.module.json", "{");
    let error = checkout.refused(&["read"], "MODEL_INVALID");
    assert_eq!(error["details"]["problems"].as_array().unwrap().len(), 2);
}

#[test]
fn subjects_resolve_by_id_name_and_former_name() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    for subject in [q("Billing1"), "Billing1".into(), "Billing".into()] {
        let out = checkout.ok(&["read", &subject]);
        assert_eq!(out["module"]["id"], q("Billing1"), "{subject}");
        assert_eq!(out["incoming"][0]["id"], q("RelWeb01"));
        assert_eq!(out["incoming"].as_array().unwrap().len(), 1);
        assert_eq!(out["outgoing"][0]["to"], q("BankApi1"));
        assert_eq!(out["parts"], json!([]));
    }
    let web = checkout.ok(&["read", "Storefront"]);
    assert_eq!(web["module"]["id"], q("WebApp01"));
    let platform = checkout.ok(&["read", "Platform"]);
    let parts: Vec<_> = platform["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(parts, [q("Billing1"), q("WebApp01")]);

    checkout.refused(&["read", "Payments"], "SUBJECT_NOT_FOUND");
    // A current name that is another module's former name is ambiguous.
    checkout.module(module("Storefr2", "Storefront", "app", 2));
    let error = checkout.refused(&["read", "Storefront"], "SUBJECT_AMBIGUOUS");
    let matches: Vec<_> = error["details"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(matches, [q("Storefr2"), q("WebApp01")]);
    // An ID still selects one of them.
    assert_eq!(
        checkout.ok(&["read", "Storefr2"])["module"]["name"],
        "Storefront"
    );
}

#[test]
fn revision_follows_recognized_files_only() {
    let checkout = Checkout::new();
    shop(&checkout);
    let digest = || checkout.ok(&["read"])["revision"]["digest"].clone();
    let first = digest();
    checkout.write("notes.md", "scratch");
    checkout.write("modules/README.txt", "scratch");
    checkout.write("modules/draft.json", "{}");
    checkout.write(".lock", "{}");
    assert_eq!(digest(), first);
    let files = checkout.ok(&["read"])["revision"]["files"].clone();
    assert_eq!(files.as_array().unwrap().len(), 6);

    let mut bank = module("BankApi1", "Bank", "external", 1);
    bank["description"] = "Card network".into();
    checkout.module(bank);
    let second = digest();
    assert_ne!(second, first);
    checkout.module(module("BankApi1", "Bank", "external", 1));
    assert_eq!(digest(), first);
}

#[test]
fn schemas_describe_the_vault_files() {
    let checkout = Checkout::outside();
    let names = checkout.ok(&["schema"]);
    assert_eq!(
        names["schemas"],
        json!([
            "model-bundle",
            "model-claim",
            "model-collection",
            "model-edit",
            "model-manifest",
            "model-picture",
            "model-proposal",
            "model-release",
            "model-walkthrough",
            "module",
            "vault-config"
        ])
    );
    checkout.refused(&["schema", "nope"], "SCHEMA_NOT_FOUND");

    let model = Checkout::new();
    shop(&model);
    let vault = model.vault();
    let read_json = |path: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(vault.join(path)).unwrap()).unwrap()
    };
    let config: Value =
        serde_yaml_ng::from_str(&fs::read_to_string(vault.join("config.yaml")).unwrap()).unwrap();
    for (name, file) in [
        ("module", read_json("modules/WebApp01.module.json")),
        ("module", read_json("modules/Billing1.module.json")),
        ("model-manifest", read_json("model.json")),
        ("vault-config", config),
    ] {
        let (code, schema) = checkout.run(&["schema", name]);
        assert_eq!(code, 0);
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&file)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:#?}");
    }
    for (name, request) in [
        (
            "model-claim",
            json!({"target": {"model": "Model001", "state": "State001"}, "writer": "alice", "reason": "billing work", "subjects": ["Billing"]}),
        ),
        (
            "model-release",
            json!({"claim": "Claim001", "writer": "alice"}),
        ),
    ] {
        let (code, schema) = checkout.run(&["schema", name]);
        assert_eq!(code, 0);
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&request), "{name}");
        let mut bad = request.clone();
        bad["surprise"] = true.into();
        assert!(!validator.is_valid(&bad), "{name}");
    }
    let (_, schema) = checkout.run(&["schema", "module"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut bad = read_json("modules/Billing1.module.json");
    bad["responsibility"] = "x".repeat(201).into();
    assert!(!validator.is_valid(&bad));
    bad["responsibility"] = "ok".into();
    bad["surprise"] = true.into();
    assert!(!validator.is_valid(&bad));
}

#[cfg(unix)]
#[test]
fn symbolic_links_in_the_vault_are_refused_not_followed() {
    let checkout = Checkout::new();
    shop(&checkout);
    let elsewhere = checkout.path().join("elsewhere.yaml");
    fs::copy(checkout.vault().join("config.yaml"), &elsewhere).unwrap();
    fs::remove_file(checkout.vault().join("config.yaml")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, checkout.vault().join("config.yaml")).unwrap();
    let error = checkout.refused(&["read"], "MODEL_INVALID");
    assert_eq!(
        error["details"]["problems"][0],
        json!({"file": "config.yaml", "subject": "config.yaml", "message": "is a symbolic link"})
    );

    let linked = Checkout::new();
    shop(&linked);
    let target = linked.path().join("bank.json");
    fs::rename(linked.vault().join("modules/BankApi1.module.json"), &target).unwrap();
    std::os::unix::fs::symlink(&target, linked.vault().join("modules/BankApi1.module.json"))
        .unwrap();
    let error = linked.refused(&["read"], "MODEL_INVALID");
    let problems = error["details"]["problems"].as_array().unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p["file"] == "modules/BankApi1.module.json"
                && p["message"] == "is a symbolic link"),
        "{problems:#?}"
    );

    // A module file that is neither a regular file nor a folder is refused, not skipped.
    let special = Checkout::new();
    shop(&special);
    let fifo = special.vault().join("modules/Fifo0001.module.json");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());
    let error = special.refused(&["read"], "MODEL_INVALID");
    assert_eq!(
        error["details"]["problems"],
        json!([{"file": "modules/Fifo0001.module.json", "subject": "Fifo0001", "message": "is not a regular file"}])
    );

    // A linked folder is refused before anything is read through it.
    let folder = Checkout::new();
    shop(&folder);
    let elsewhere = folder.path().join("elsewhere");
    fs::rename(folder.vault().join("modules"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, folder.vault().join("modules")).unwrap();
    let error = folder.refused(&["read"], "MODEL_INVALID");
    assert_eq!(
        error["details"]["problems"],
        json!([{"file": "modules", "subject": "modules", "message": "is a symbolic link"}])
    );
    let request = json!({"target": {"model": "x", "state": "y"}, "expected_revision": "z",
        "operation_id": "op", "writer": "w", "changes": [{"type": "remove-module", "subject": "Bank"}]});
    let (code, value) = folder.edit(&request);
    assert_eq!(code, 2, "{value:#}");
    assert_eq!(value["error"]["code"], "MODEL_INVALID");
}

#[test]
fn edit_applies_every_change_type() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let before = checkout.digest();

    // A module added in the batch is referenced by name later in the same batch.
    let out = checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 2, "parent": "Billing", "description": "Books\nentries"}},
        {"type": "add-relationship", "source": "Ledger", "relationship": {"to": "Bank", "kind": "call", "order": 0, "label": "settles"}},
    ]));
    assert_eq!(out["schema"], "boundrail/model/v1");
    // Only a collection creates the collections folder.
    assert!(!checkout.vault().join("collections").exists());
    assert!(out["operation_id"].as_str().unwrap().starts_with("op-"));
    assert_eq!(out["writer"], "tester");
    assert_eq!(out["doing"], "testing edits");
    assert_eq!(out["before"]["digest"], before);
    assert_eq!(out["after"]["digest"], checkout.digest());
    let ledger = out["allocated"][0]["id"].as_str().unwrap().to_owned();
    let settles = out["allocated"][1]["id"].as_str().unwrap().to_owned();
    assert_eq!(out["allocated"][0]["name"], "Ledger");
    assert_eq!(
        out["allocated"][1]["name"],
        "relationship from Ledger to Bank"
    );
    let mut added = vec![ledger.clone(), settles.clone()];
    added.sort();
    assert_eq!(out["subjects"]["added"], json!(added));
    assert_eq!(out["subjects"]["changed"], json!([]));
    let local = ledger.split_once('#').unwrap().1.to_owned();
    let file = checkout.read_module(&local);
    assert_eq!(file["parent"], q("Billing1"));
    assert_eq!(file["description"], "Books\nentries");
    assert_eq!(file["outgoing"][0]["to"], q("BankApi1"));
    assert_eq!(file["outgoing"][0]["label"], "settles");

    let out = checkout.edited(json!([
        {"type": "edit-module", "subject": "Ledger", "set": {"kind": "module", "order": 3, "responsibility": "Keeps the books"}, "clear": ["description"]},
        {"type": "edit-relationship", "subject": q("RelBil01"), "set": {"label": "pays", "kind": "http", "emphasis": "hero"}},
        {"type": "remove-relationship", "subject": "RelWeb01"},
    ]));
    assert_eq!(
        out["subjects"]["changed"],
        json!(sorted(&[
            &ledger,
            &q("Billing1"),
            &q("RelBil01"),
            &q("WebApp01")
        ]))
    );
    assert_eq!(out["subjects"]["removed"], json!([q("RelWeb01")]));
    let file = checkout.read_module(&local);
    assert_eq!(file["kind"], "module");
    assert_eq!(file["order"], 3);
    assert_eq!(file["responsibility"], "Keeps the books");
    assert!(file.get("description").is_none());
    let billing = checkout.read_module("Billing1");
    assert_eq!(
        billing["outgoing"],
        json!([{"id": "RelBil01", "to": q("BankApi1"), "kind": "http", "order": 0, "label": "pays", "emphasis": "hero"}])
    );
    assert_eq!(checkout.read_module("WebApp01")["outgoing"], json!([]));
    let manifest = checkout.ok(&["read"])["manifest"].clone();
    assert_eq!(manifest["retired_ids"], json!(["RelWeb01"]));

    let out = checkout.edited(json!([{"type": "remove-module", "subject": ledger}]));
    assert_eq!(
        out["subjects"]["removed"],
        json!(sorted(&[&ledger, &settles]))
    );
    assert!(
        !checkout
            .vault()
            .join(format!("modules/{local}.module.json"))
            .exists()
    );
    let retired = checkout.ok(&["read"])["manifest"]["retired_ids"].clone();
    assert_eq!(retired.as_array().unwrap().len(), 3);
}

fn sorted(ids: &[&str]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    ids.sort();
    ids
}

#[test]
fn rename_keeps_the_id_and_records_former_names() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    checkout.edited(
        json!([{"type": "edit-module", "subject": "Billing", "set": {"name": "Payments"}}]),
    );
    let billing = checkout.read_module("Billing1");
    assert_eq!(billing["name"], "Payments");
    assert_eq!(billing["former_names"], json!(["Billing"]));
    assert_eq!(
        checkout.ok(&["read", "Billing"])["module"]["id"],
        format!("{model}#Billing1")
    );

    // Renaming back drops the name from the former names and records the one it leaves.
    checkout.edited(
        json!([{"type": "edit-module", "subject": "Payments", "set": {"name": "Billing"}}]),
    );
    let billing = checkout.read_module("Billing1");
    assert_eq!(billing["name"], "Billing");
    assert_eq!(billing["former_names"], json!(["Payments"]));
    checkout
        .edited(json!([{"type": "edit-module", "subject": "Web", "set": {"name": "Storefront"}}]));
    assert_eq!(
        checkout.read_module("WebApp01")["former_names"],
        json!(["Web"])
    );
}

#[test]
fn edit_moves_modules_and_refuses_containment_cycles() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    checkout.edited(json!([
        {"type": "edit-module", "subject": "Bank", "set": {"parent": "Platform"}},
        {"type": "edit-module", "subject": "Web", "clear": ["parent"]},
    ]));
    assert_eq!(checkout.read_module("BankApi1")["parent"], q("Platfrm1"));
    assert!(checkout.read_module("WebApp01").get("parent").is_none());
    assert_eq!(
        checkout.ok(&["read"])["roots"],
        json!([q("Platfrm1"), q("WebApp01")])
    );

    let request = checkout.request(json!([
        {"type": "edit-module", "subject": "Platform", "set": {"parent": "Billing"}},
    ]));
    let error = checkout.edit_refused(&request, "MODEL_INVALID");
    let problems = error["details"]["problems"].as_array().unwrap();
    assert!(
        problems.iter().any(|p| p["subject"] == "Platfrm1"
            && p["change"] == 0
            && p["message"].as_str().unwrap().contains("cycle")),
        "{problems:#?}"
    );
}

#[test]
fn remove_module_takes_its_relationships_unless_the_batch_moves_them() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let out = checkout.edited(json!([{"type": "remove-module", "subject": "Billing"}]));
    assert_eq!(
        out["subjects"]["removed"],
        json!(sorted(&[&q("Billing1"), &q("RelBil01"), &q("RelWeb01")]))
    );
    assert_eq!(out["subjects"]["changed"], json!([q("WebApp01")]));
    assert!(
        !checkout
            .vault()
            .join("modules/Billing1.module.json")
            .exists()
    );
    assert_eq!(checkout.read_module("WebApp01")["outgoing"], json!([]));
    assert_eq!(
        checkout.ok(&["read"])["manifest"]["retired_ids"],
        json!(["Billing1", "RelBil01", "RelWeb01"])
    );

    // Later changes in the batch can still re-target incoming and move outgoing relationships.
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let out = checkout.edited(json!([
        {"type": "remove-module", "subject": "Billing"},
        {"type": "edit-relationship", "subject": "RelWeb01", "set": {"to": "Bank"}},
        {"type": "edit-relationship", "subject": "RelBil01", "source": "Platform"},
    ]));
    assert_eq!(out["subjects"]["removed"], json!([q("Billing1")]));
    assert_eq!(
        checkout.read_module("WebApp01")["outgoing"][0]["to"],
        q("BankApi1")
    );
    assert_eq!(
        checkout.read_module("Platfrm1")["outgoing"][0]["id"],
        "RelBil01"
    );
}

#[test]
fn removing_a_parent_needs_its_children_removed_or_reparented() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let request = checkout.request(json!([{"type": "remove-module", "subject": "Platform"}]));
    let error = checkout.edit_refused(&request, "CHILDREN_ORPHANED");
    assert_eq!(
        error["details"]["children"],
        json!([q("Billing1"), q("WebApp01")])
    );

    checkout.edited(json!([
        {"type": "remove-module", "subject": "Platform"},
        {"type": "edit-module", "subject": "Billing", "set": {"parent": "Bank"}},
        {"type": "remove-module", "subject": "Web"},
    ]));
    assert_eq!(checkout.read_module("Billing1")["parent"], q("BankApi1"));
    assert_eq!(checkout.ok(&["read"])["roots"], json!([q("BankApi1")]));
}

#[test]
fn relationship_transfer_keeps_its_id() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let out = checkout.edited(json!([
        {"type": "edit-relationship", "subject": format!("{model}#RelBil01"), "source": "Web", "set": {"order": 1}},
    ]));
    assert_eq!(out["allocated"], json!([]));
    assert_eq!(checkout.read_module("Billing1")["outgoing"], json!([]));
    let web = checkout.read_module("WebApp01");
    let ids: Vec<_> = web["outgoing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["RelWeb01", "RelBil01"]);
    let read = checkout.ok(&["read", "Web"]);
    assert_eq!(read["outgoing"][1]["id"], format!("{model}#RelBil01"));
    assert_eq!(read["outgoing"][1]["from"], format!("{model}#WebApp01"));
}

#[test]
fn edit_refusals_write_nothing() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let base =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 5}}]));
    let with = |key: &str, value: Value| {
        let mut request = base.clone();
        request[key] = value;
        request
    };

    let error = checkout.edit_refused(
        &with("expected_revision", "0".repeat(64).into()),
        "STALE_REVISION",
    );
    assert_eq!(error["details"]["current"], checkout.digest());
    checkout.edit_refused(
        &with("target", json!({"model": model, "state": "Other001"})),
        "TARGET_MISMATCH",
    );
    checkout.edit_refused(
        &with(
            "target",
            json!({"model": "Other001", "state": base["target"]["state"]}),
        ),
        "TARGET_MISMATCH",
    );

    for doing in ["", "   ", "two\nlines", &"x".repeat(141)] {
        let (code, stdout) = checkout.edit_with(&base, doing, &[]);
        assert_eq!(code, 2, "{doing:?}");
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["error"]["code"], "DOING_INVALID", "{doing:?}");
    }

    let change = |change: Value| with("changes", json!([change]));
    let invalid = [
        with("operation_id", "has space".into()),
        with("operation_id", "x".repeat(65).into()),
        with("writer", "".into()),
        with("changes", json!([])),
        with("surprise", true.into()),
        change(json!({"type": "edit-module", "subject": "Bank", "bogus": 1})),
        change(json!({"type": "rename-module", "subject": "Bank"})),
        change(
            json!({"type": "add-module", "module": {"id": "Chosen01", "name": "X", "kind": "service", "order": 0}}),
        ),
        change(
            json!({"type": "edit-module", "subject": "Bank", "set": {"parent": "Platform"}, "clear": ["parent"]}),
        ),
        change(json!({"type": "edit-module", "subject": "Bank", "clear": ["name"]})),
        change(
            json!({"type": "edit-relationship", "subject": "RelBil01", "set": {"label": "x"}, "clear": ["label"]}),
        ),
        change(json!({"type": "edit-relationship", "subject": "RelBil01", "clear": ["kind"]})),
        with(
            "changes",
            json!([
                {"type": "edit-module", "subject": "Bank", "set": {"order": 5}},
                {"type": "edit-module", "subject": "BankApi1", "set": {"order": 6}},
            ]),
        ),
    ];
    for request in &invalid {
        checkout.edit_refused(request, "REQUEST_INVALID");
    }

    let error = checkout.edit_refused(
        &with(
            "changes",
            json!([
                {"type": "edit-module", "subject": "Bank", "set": {"order": 5}},
                {"type": "remove-relationship", "subject": "Missing1"},
            ]),
        ),
        "SUBJECT_NOT_FOUND",
    );
    assert_eq!(error["details"]["change"], 1);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("changes[1]: ")
    );
    checkout.edit_refused(
        &change(json!({"type": "remove-module", "subject": "Payments"})),
        "SUBJECT_NOT_FOUND",
    );
    checkout.module(module("Storefr2", "Storefront", "app", 2));
    let base = checkout.request(json!([]));
    let mut ambiguous = base.clone();
    ambiguous["changes"] = json!([{"type": "remove-module", "subject": "Storefront"}]);
    let error = checkout.edit_refused(&ambiguous, "SUBJECT_AMBIGUOUS");
    assert_eq!(error["details"]["change"], 0);
    assert_eq!(error["details"]["matches"].as_array().unwrap().len(), 2);

    let mut unknown_kind = base.clone();
    unknown_kind["changes"] = json!([
        {"type": "add-module", "module": {"name": "Fn", "kind": "lambda", "order": 0}},
    ]);
    let error = checkout.edit_refused(&unknown_kind, "MODEL_INVALID");
    let problem = &error["details"]["problems"][0];
    assert_eq!(problem["change"], 0);
    assert!(problem["message"].as_str().unwrap().contains("node kind"));

    let mut escape = base.clone();
    escape["changes"] = json!([
        {"type": "edit-module", "subject": "Bank", "set": {"name": "Bank\u{1b}[2J"}},
    ]);
    let error = checkout.edit_refused(&escape, "MODEL_INVALID");
    assert!(
        error["details"]["problems"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("name needs 1 to 120 characters")
    );
    let mut blank = base.clone();
    blank["changes"] = json!([{"type": "edit-module", "subject": "Bank", "set": {"name": ""}}]);
    checkout.edit_refused(&blank, "MODEL_INVALID");

    let mut oversized = base.clone();
    oversized["writer"] = "x".repeat(4 * 1024 * 1024).into();
    let error = checkout.edit_refused(&oversized, "REQUEST_INVALID");
    assert!(error["message"].as_str().unwrap().contains("larger than"));
    let mut many = base.clone();
    many["changes"] = (0..1001)
        .map(|_| json!({"type": "remove-relationship", "subject": "RelBil01"}))
        .collect();
    checkout.edit_refused(&many, "REQUEST_INVALID");
}

#[test]
fn operation_ids_replay_their_receipt_or_refuse_another_request() {
    let checkout = Checkout::new();
    shop(&checkout);
    let request = checkout.request(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 0}},
    ]));
    fs::write(checkout.path().join("request.json"), request.to_string()).unwrap();
    let args = [
        "edit",
        "--input",
        "request.json",
        "--doing",
        "add the ledger",
    ];
    let (code, first) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{first}");
    let receipt = checkout
        .path()
        .join(".git/boundrail/operations")
        .join(format!(
            "{}.json",
            request["operation_id"].as_str().unwrap()
        ));
    assert!(receipt.is_file());

    // Another edit moves the revision on; the first operation still replays its receipt.
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let moved = checkout.digest();
    let (code, again) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again, first);
    assert_eq!(checkout.digest(), moved);
    let modules = checkout.ok(&["read"])["modules"].as_array().unwrap().len();
    assert_eq!(modules, 5);
    let (code, text) = checkout.run_raw(&[
        "edit",
        "--input",
        "request.json",
        "--doing",
        "add the ledger",
        "--format",
        "markdown",
    ]);
    assert_eq!(code, 0);
    assert!(text.contains("by tester: add the ledger"), "{text}");
    assert!(text.contains("for Ledger"), "{text}");

    // A reworded --doing is the same request; the receipt keeps the original reason.
    let (code, stdout) = checkout.edit_with(&request, "another reason", &[]);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(stdout, first);
    let mut changed = request.clone();
    changed["writer"] = "someone else".into();
    let (code, stdout) = checkout.edit_with(&changed, "add the ledger", &[]);
    assert_eq!(code, 2);
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["error"]["code"], "OPERATION_REUSED");
    assert_eq!(checkout.digest(), moved);
}

#[test]
fn a_held_edit_lock_refuses() {
    let checkout = Checkout::new();
    shop(&checkout);
    let state = checkout.path().join(".git/boundrail");
    fs::create_dir_all(&state).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("model.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let request =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let error = checkout.edit_refused(&request, "VAULT_LOCKED");
    checkout.claim_refused("alice", json!(["Bank"]), "VAULT_LOCKED");
    assert!(
        error["details"]["lock"]
            .as_str()
            .unwrap()
            .ends_with("model.lock")
    );

    drop(lock);
    let (code, value) = checkout.edit(&request);
    assert_eq!(code, 0, "{value:#}");
    assert!(!checkout.vault().join("model.lock").exists());
}

#[test]
fn an_interrupted_edit_blocks_others_until_recovered() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let request = checkout.request(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 0}},
        {"type": "remove-module", "subject": "Web"},
    ]));
    let before = checkout.digest();
    // An error, not a refusal: nothing on stdout.
    let (code, stdout) = checkout.edit_with(&request, "add the ledger", &[STOP]);
    assert_eq!((code, stdout.as_str()), (2, ""));
    assert_eq!(checkout.digest(), before);
    let operation = request["operation_id"].as_str().unwrap();
    let journal = checkout.path().join(".git/boundrail/operations");
    let pending_path = journal.join(format!("pending/{operation}.json"));
    let record_path = journal.join(format!("{operation}.json"));
    let read_record =
        || -> Value { serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap() };
    let record: Value = serde_json::from_str(&fs::read_to_string(&pending_path).unwrap()).unwrap();
    assert!(!record_path.exists());
    assert_eq!(record["status"], "pending");
    assert_eq!(record["before"], before);
    let after = record["after"].clone();
    assert_eq!(record["output"]["after"]["digest"], after);
    let paths: Vec<_> = record["writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths[0], "model.json", "the manifest is written first");
    assert_eq!(*paths.last().unwrap(), "modules/WebApp01.module.json");

    let other =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let error = checkout.edit_refused(&other, "RECOVERY_REQUIRED");
    assert_eq!(error["details"]["operation_id"], operation);

    let recovered = checkout.ok(&["recover"]);
    assert_eq!(recovered["kind"], "recover");
    assert_eq!(recovered["completed"][0]["operation_id"], operation);
    assert_eq!(checkout.digest(), after);
    assert!(
        !checkout
            .vault()
            .join("modules/WebApp01.module.json")
            .exists()
    );
    let retired = checkout.ok(&["read"])["manifest"]["retired_ids"].clone();
    assert!(retired.as_array().unwrap().contains(&json!("WebApp01")));
    let record = read_record();
    assert_eq!(record["status"], "done");
    assert!(record.get("writes").is_none());
    assert!(!pending_path.exists());
    assert_eq!(checkout.ok(&["recover"])["completed"], json!([]));

    let (code, replayed) = checkout.edit(&request);
    assert_eq!(code, 0, "{replayed:#}");
    assert_eq!(replayed["after"]["digest"], after);
    assert_eq!(replayed["doing"], "add the ledger");
    assert!(
        replayed["subjects"]["removed"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("{model}#WebApp01")))
    );
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));

    // Replaying the interrupted operation itself also finishes it.
    let again = Checkout::new();
    shop(&again);
    let request =
        again.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
    let (code, stdout) = again.edit_with(&request, "reorder", &[STOP]);
    assert_eq!((code, stdout.as_str()), (2, ""));
    let (code, value) = again.edit(&request);
    assert_eq!(code, 0, "{value:#}");
    assert_eq!(again.read_module("BankApi1")["order"], 7);
    assert_eq!(value["after"]["digest"], again.digest());
}

#[test]
fn editing_a_relationship_keeps_its_place() {
    let checkout = Checkout::new();
    shop(&checkout);
    checkout.edited(json!([
        {"type": "add-relationship", "source": "Billing", "relationship": {"to": "Platform", "kind": "call", "order": 1}},
    ]));
    checkout.edited(json!([
        {"type": "edit-relationship", "subject": "RelBil01", "set": {"label": "pays"}},
    ]));
    let outgoing = checkout.read_module("Billing1")["outgoing"].clone();
    assert_eq!(outgoing.as_array().unwrap().len(), 2);
    assert_eq!(outgoing[0]["id"], "RelBil01");
    assert_eq!(outgoing[0]["label"], "pays");
}

#[test]
fn the_model_edit_schema_validates_a_request() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let request = checkout.request(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 0, "parent": "Billing", "groups": [], "binding": {"path": "src/ledger"}}},
        {"type": "edit-module", "subject": "Ledger", "set": {"name": "Books", "order": 1}, "clear": ["binding"]},
        {"type": "add-relationship", "source": "Books", "relationship": {"to": "Bank", "kind": "call", "order": 0, "emphasis": "muted", "anchor": {"path": "src/ledger.rs", "symbol": "settle"}}},
        {"type": "edit-relationship", "subject": format!("{model}#RelWeb01"), "source": "Platform", "set": {"label": "routes"}, "clear": ["description"]},
        {"type": "remove-relationship", "subject": "RelBil01"},
        {"type": "remove-module", "subject": "Bank"},
    ]));
    let (code, schema) = checkout.run(&["schema", "model-edit"]);
    assert_eq!(code, 0);
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&request)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    checkout.edited(request["changes"].clone());

    for (key, value) in [
        ("operation_id", json!("has space")),
        ("surprise", json!(true)),
        (
            "changes",
            json!([{"type": "edit-module", "subject": "Bank", "clear": ["name"]}]),
        ),
        (
            "changes",
            json!([{"type": "remove-module", "subject": "Bank", "bogus": 1}]),
        ),
    ] {
        let mut bad = request.clone();
        bad[key] = value;
        assert!(!validator.is_valid(&bad), "{key}");
    }
}

#[test]
fn claims_round_trip_and_show_in_the_reading() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    assert_eq!(checkout.claims(), json!([]));
    assert_eq!(checkout.ok(&["read"])["claims"], json!([]));

    // Subjects resolve like `model read`, are stored qualified, sorted and once each.
    let claim = checkout.claim(
        "alice",
        json!(["Billing", q("BankApi1"), "RelBil01", model, "Billing1"]),
    );
    assert!(boundrail::arch::ids::is_local(
        claim["id"].as_str().unwrap()
    ));
    assert_eq!(claim["writer"], "alice");
    assert_eq!(claim["reason"], "alice works here");
    assert_eq!(claim["doing"], "reserve my modules");
    assert!(claim["created"].as_u64().unwrap() > 0);
    assert!(claim.get("revoked").is_none());
    assert_eq!(
        claim["subjects"],
        json!(sorted(&[
            &model,
            &q("BankApi1"),
            &q("Billing1"),
            &q("RelBil01")
        ]))
    );
    assert_eq!(checkout.claims(), json!([claim]));
    assert_eq!(checkout.ok(&["read"])["claims"], json!([claim]));
    // Reading a module shows the claims that hold it, its parts or its relationships.
    assert_eq!(checkout.ok(&["read", "Bank"])["claims"], json!([claim]));
    assert_eq!(checkout.ok(&["read", "Platform"])["claims"], json!([claim]));
    assert_eq!(checkout.ok(&["read", "Web"])["claims"], json!([]));
    let stored: Value = serde_json::from_str(
        &fs::read_to_string(checkout.path().join(".git/boundrail/claims.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored, json!({"claims": [claim]}));
    let (code, text) = checkout.run_raw(&["claims", "--format", "markdown"]);
    assert_eq!(code, 0);
    assert!(text.contains("by alice: alice works here"), "{text}");

    checkout.claim_refused("alice", json!(["Payments"]), "SUBJECT_NOT_FOUND");
    checkout.claim_refused("alice", json!([]), "REQUEST_INVALID");
    let (code, out) = checkout.release(&json!({"id": "Missing1"}), "alice");
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("CLAIM_NOT_FOUND"))
    );

    let (code, out) = checkout.release(&claim, "alice");
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "release");
    assert_eq!(out["claim"], claim);
    assert_eq!(checkout.claims(), json!([]));
    assert_eq!(checkout.ok(&["read", "Bank"])["claims"], json!([]));

    // A corrupt claims file never fails a reading: the claims are left out with a warning.
    fs::write(
        checkout.path().join(".git/boundrail/claims.json"),
        "{ broken",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model", "read"])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let read: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(read.get("claims").is_none());
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning: claims omitted"));
}

#[test]
fn claims_wait_for_an_interrupted_edit() {
    let checkout = Checkout::new();
    shop(&checkout);
    let claim = checkout.claim("alice", json!(["Bank"]));
    let request = checkout.request_by(
        "alice",
        Some(&claim),
        json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]),
    );
    let (code, _) = checkout.edit_with(
        &request,
        "reorder",
        &[("BOUNDRAIL_TEST_STOP_AFTER_JOURNAL", "1")],
    );
    assert_eq!(code, 2);
    checkout.claim_refused("bob", json!(["Web"]), "RECOVERY_REQUIRED");
    let (code, out) = checkout.release(&claim, "alice");
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("RECOVERY_REQUIRED"))
    );
    checkout.ok(&["recover"]);
    checkout.claim("bob", json!(["Web"]));
}

#[test]
fn a_container_claim_holds_its_parts() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let platform = checkout.claim("alice", json!(["Platform"]));
    let (code, out) = checkout.edit(&checkout.request_by(
        "alice",
        Some(&platform),
        json!([{"type": "edit-module", "subject": "Billing", "set": {"description": "Takes money"}}]),
    ));
    assert_eq!(code, 0, "{out:#}");
    // Claims on either side of a containment clash.
    let error = checkout.claim_refused("bob", json!(["Billing"]), "CLAIM_CONFLICT");
    assert_eq!(
        error["details"]["conflicts"][0]["subjects"],
        json!([q("Billing1")])
    );
    checkout.claim_refused("bob", json!(["RelWeb01"]), "CLAIM_CONFLICT");
    let bank = checkout.claim("bob", json!(["Bank"]));
    checkout.claim_refused("alice", json!(["Bank"]), "CLAIM_CONFLICT");
    checkout.edit_refused(
        &checkout.request(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}])),
        "CLAIM_CONFLICT",
    );

    // A hand-edited ledger that gives bob a part of alice's container still blocks alice.
    let path = checkout.path().join(".git/boundrail/claims.json");
    let mut ledger: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let mut forged = bank.clone();
    forged["subjects"] = json!([q("WebApp01")]);
    ledger["claims"].as_array_mut().unwrap().push(forged);
    fs::write(&path, ledger.to_string()).unwrap();
    checkout.edit_refused(
        &checkout.request_by(
            "alice",
            Some(&platform),
            json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]),
        ),
        "CLAIM_CONFLICT",
    );
}

#[test]
fn writers_cannot_claim_or_release_each_others_subjects() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let first = checkout.claim("alice", json!(["Billing"]));
    let error = checkout.claim_refused("bob", json!(["Bank", "Billing"]), "CLAIM_CONFLICT");
    assert_eq!(
        error["details"]["conflicts"],
        json!([{"id": first["id"], "writer": "alice", "reason": "alice works here", "subjects": [q("Billing1")]}])
    );
    // The same writer may hold several claims, overlapping ones included.
    let second = checkout.claim("alice", json!(["Billing", "Web"]));
    checkout.claim("bob", json!(["Bank"]));
    assert_eq!(checkout.claims().as_array().unwrap().len(), 3);

    let (code, out) = checkout.release(&second, "bob");
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "CLAIM_NOT_HELD");
    assert_eq!(out["error"]["details"]["writer"], "alice");
    assert_eq!(checkout.claims().as_array().unwrap().len(), 3);
}

#[test]
fn a_revoked_claim_stops_blocking_and_refuses_its_holder() {
    let checkout = Checkout::new();
    shop(&checkout);
    let claim = checkout.claim("alice", json!(["Bank"]));
    let change = || json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]);
    checkout.edit_refused(
        &checkout.request_by("bob", None, change()),
        "CLAIM_CONFLICT",
    );

    let id = claim["id"].as_str().unwrap();
    let revoke = [
        "release",
        "--revoke",
        id,
        "--writer",
        "dan",
        "--reason",
        "alice left",
    ];
    let out = checkout.ok(&revoke);
    assert_eq!(out["kind"], "revoke");
    assert_eq!(out["claim"]["revoked"]["by"], "dan");
    assert_eq!(out["claim"]["revoked"]["reason"], "alice left");
    // Revoked claims stay listed until released, but the reading shows only active ones.
    assert_eq!(checkout.claims(), json!([out["claim"]]));
    assert_eq!(checkout.ok(&["read"])["claims"], json!([]));

    let error = checkout.edit_refused(
        &checkout.request_by("alice", Some(&claim), change()),
        "CLAIM_REVOKED",
    );
    assert_eq!(error["details"]["revoked"]["by"], "dan");
    let (code, out) = checkout.edit(&checkout.request_by("bob", None, change()));
    assert_eq!(code, 0, "{out:#}");

    // A revoked claim blocks nobody, so any writer may clear it.
    let (code, out) = checkout.release(&claim, "bob");
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(checkout.claims(), json!([]));
}

#[test]
fn an_edit_with_a_claim_needs_it_to_cover_every_subject() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let web = checkout.claim("alice", json!(["Web"]));
    let add = json!([
        {"type": "add-relationship", "source": "Web", "relationship": {"to": "Bank", "kind": "call", "order": 1}},
    ]);
    // A new relationship needs its source and target; its own ID did not exist to claim.
    let error = checkout.edit_refused(
        &checkout.request_by("alice", Some(&web), add.clone()),
        "CLAIM_INCOMPLETE",
    );
    assert_eq!(error["details"]["missing"], json!([q("BankApi1")]));
    checkout.edit_refused(
        &checkout.request_by("bob", Some(&web), add.clone()),
        "CLAIM_NOT_HELD",
    );
    let mut unknown = checkout.request_by("alice", None, add.clone());
    unknown["claim"] = "Missing1".into();
    checkout.edit_refused(&unknown, "CLAIM_NOT_FOUND");

    let ends = checkout.claim("alice", json!(["Web", "Bank"]));
    let request = checkout.request_by("alice", Some(&ends), add);
    let (code, out) = checkout.edit(&request);
    assert_eq!(code, 0, "{out:#}");

    // A done operation replays its receipt without checking claims again.
    let (code, _) = checkout.release(&ends, "alice");
    assert_eq!(code, 0);
    let (code, replayed) = checkout.edit(&request);
    assert_eq!(code, 0, "{replayed:#}");
    assert_eq!(replayed, out);

    // Moving a root module into another needs the module, the new parent and the root level.
    let bank = checkout.claim("alice", json!(["Bank"]));
    let error = checkout.edit_refused(
        &checkout.request_by(
            "alice",
            Some(&bank),
            json!([{"type": "edit-module", "subject": "Bank", "set": {"parent": "Platform"}}]),
        ),
        "CLAIM_INCOMPLETE",
    );
    assert_eq!(
        error["details"]["missing"],
        json!(sorted(&[&model, &q("Platfrm1")]))
    );

    // Modules and relationships added in the batch are covered by where they land.
    let platform = checkout.claim("alice", json!(["Platform"]));
    let (code, out) = checkout.edit(&checkout.request_by(
        "alice",
        Some(&platform),
        json!([
            {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 3, "parent": "Platform"}},
            {"type": "add-module", "module": {"name": "Journal", "kind": "module", "order": 0, "parent": "Ledger"}},
            {"type": "add-module", "module": {"name": "Audit", "kind": "service", "order": 4, "parent": "Platform"}},
            {"type": "add-relationship", "source": "Audit", "relationship": {"to": "Billing", "kind": "call", "order": 0}},
        ]),
    ));
    assert_eq!(code, 0, "{out:#}");

    // A batch that only retires an ID still rewrites the manifest, which the model ID holds.
    let temporary = json!([
        {"type": "add-module", "module": {"name": "Scratch", "kind": "other", "order": 9, "parent": "Platform"}},
        {"type": "remove-module", "subject": "Scratch"},
    ]);
    let error = checkout.edit_refused(
        &checkout.request_by("alice", Some(&platform), temporary.clone()),
        "CLAIM_INCOMPLETE",
    );
    assert_eq!(error["details"]["missing"], json!([model]));
    let root = checkout.claim("alice", json!([model]));
    let (code, out) = checkout.edit(&checkout.request_by("alice", Some(&root), temporary));
    assert_eq!(code, 0, "{out:#}");
}

#[test]
fn taking_a_claimed_modules_name_conflicts() {
    let checkout = Checkout::new();
    shop(&checkout);
    checkout.claim("bob", json!(["Bank"]));
    for change in [
        json!({"type": "add-module", "module": {"name": "Bank", "kind": "service", "order": 0, "parent": "Platform"}}),
        json!({"type": "edit-module", "subject": "Web", "set": {"name": "Bank"}}),
    ] {
        let error = checkout.edit_refused(&checkout.request(json!([change])), "CLAIM_CONFLICT");
        assert_eq!(error["details"]["conflicts"][0]["writer"], "bob");
    }
    // Another writer holding the root level does not stop a change inside a module.
    checkout.claim("bob", json!([checkout.ok(&["read"])["manifest"]["id"]]));
    checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Cart", "kind": "service", "order": 2, "parent": "Platform"}},
    ]));
}

#[test]
fn an_edit_without_a_claim_is_blocked_only_by_overlapping_claims() {
    let checkout = Checkout::new();
    shop(&checkout);
    checkout.claim("bob", json!(["Bank"]));
    // Relabelling Billing's call touches its target, Bank, which bob holds.
    let error = checkout.edit_refused(
        &checkout.request(json!([
            {"type": "edit-relationship", "subject": "RelBil01", "set": {"label": "pays"}},
        ])),
        "CLAIM_CONFLICT",
    );
    assert_eq!(error["details"]["conflicts"][0]["writer"], "bob");
    checkout.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
    // bob's own claims never block bob.
    let (code, out) = checkout.edit(&checkout.request_by(
        "bob",
        None,
        json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]),
    ));
    assert_eq!(code, 0, "{out:#}");
}

#[test]
fn a_relationship_transfer_needs_its_old_and_new_source() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let transfer = json!([{"type": "edit-relationship", "subject": "RelBil01", "source": "Web"}]);
    let partial = checkout.claim("alice", json!(["RelBil01", "Billing", "Bank"]));
    let error = checkout.edit_refused(
        &checkout.request_by("alice", Some(&partial), transfer.clone()),
        "CLAIM_INCOMPLETE",
    );
    assert_eq!(error["details"]["missing"], json!([q("WebApp01")]));
    let full = checkout.claim("alice", json!(["RelBil01", "Billing", "Bank", "Web"]));
    let (code, out) = checkout.edit(&checkout.request_by("alice", Some(&full), transfer));
    assert_eq!(code, 0, "{out:#}");

    // Removing a root module needs it, its relationships' ends and the root level. Its ID stays in
    // the claim, where it never matches again.
    let both = checkout.claim("alice", json!([model, "Bank", "Web", "RelBil01"]));
    let (code, out) = checkout.edit(&checkout.request_by(
        "alice",
        Some(&both),
        json!([{"type": "remove-module", "subject": "Bank"}]),
    ));
    assert_eq!(code, 0, "{out:#}");
    let listed = checkout.claims();
    assert!(listed.as_array().unwrap().iter().any(|c| {
        c["subjects"]
            .as_array()
            .unwrap()
            .contains(&json!(q("BankApi1")))
    }));
    checkout.claim_refused("bob", json!(["Bank"]), "SUBJECT_NOT_FOUND");
}

const STOP: (&str, &str) = ("BOUNDRAIL_TEST_STOP_AFTER_JOURNAL", "1");

/// Runs `request` as an edit that stops after writing its pending journal record, and returns that
/// record.
fn interrupt(checkout: &Checkout, request: &Value) -> Value {
    let (code, stdout) = checkout.edit_with(request, "interrupted", &[STOP]);
    assert_eq!((code, stdout.as_str()), (2, ""));
    let path = checkout.path().join(format!(
        ".git/boundrail/operations/pending/{}.json",
        request["operation_id"].as_str().unwrap()
    ));
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn recovery_refuses_a_vault_changed_since_the_plan() {
    // A hand change inside the edit's writes, then outside them.
    for path in [
        "modules/BankApi1.module.json",
        "modules/WebApp01.module.json",
    ] {
        let checkout = Checkout::new();
        shop(&checkout);
        let request = checkout
            .request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
        interrupt(&checkout, &request);
        let operation = request["operation_id"].as_str().unwrap();
        let file = checkout.vault().join(path);
        let hand = fs::read_to_string(&file).unwrap() + "\n";
        fs::write(&file, &hand).unwrap();

        let error = checkout.refused(&["recover"], "RECOVERY_CONFLICT");
        assert_eq!(
            error["details"],
            json!({"operation_id": operation, "paths": [path]})
        );
        let (code, out) = checkout.edit(&request);
        assert_eq!(code, 2, "{out:#}");
        assert_eq!(out["error"]["code"], "RECOVERY_CONFLICT");
        assert_eq!(fs::read_to_string(&file).unwrap(), hand);

        let out = checkout.ok(&["recover", "--abandon", operation]);
        assert_eq!(out["kind"], "abandon");
        assert_eq!(out["operation_id"], operation);
        assert_eq!(out["landed"], json!([]));
        assert_eq!(fs::read_to_string(&file).unwrap(), hand);
        assert_eq!(checkout.read_module("BankApi1")["order"], 1);

        checkout.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
        let (code, out) = checkout.edit(&request);
        assert_eq!(code, 2, "{out:#}");
        assert_eq!(out["error"]["code"], "OPERATION_ABANDONED");
        checkout.refused(&["recover", "--abandon", operation], "REQUEST_INVALID");
        checkout.refused(&["recover", "--abandon", "../x"], "REQUEST_INVALID");
    }
}

#[test]
fn recovery_rolls_a_partly_applied_edit_forward() {
    let checkout = Checkout::new();
    shop(&checkout);
    let request = checkout.request(json!([
        {"type": "edit-module", "subject": "Bank", "set": {"order": 7}},
        {"type": "remove-module", "subject": "Web"},
    ]));
    let record = interrupt(&checkout, &request);
    // The manifest and the deletion landed before the crash; the Bank file did not.
    for write in record["writes"].as_array().unwrap() {
        let file = checkout.vault().join(write["path"].as_str().unwrap());
        match write["content"].as_str() {
            Some(content) if write["path"] == "model.json" => fs::write(file, content).unwrap(),
            None => fs::remove_file(file).unwrap(),
            Some(_) => {}
        }
    }
    let recovered = checkout.ok(&["recover"]);
    assert_eq!(
        recovered["completed"][0]["operation_id"],
        request["operation_id"]
    );
    assert_eq!(checkout.digest(), record["after"]);
    assert_eq!(checkout.read_module("BankApi1")["order"], 7);

    // Abandoning reports what had already landed, and refuses once every write has.
    let request = checkout.request(json!([
        {"type": "edit-module", "subject": "Bank", "set": {"order": 8}},
        {"type": "edit-module", "subject": "Billing", "set": {"order": 8}},
    ]));
    let operation = request["operation_id"].as_str().unwrap();
    let record = interrupt(&checkout, &request);
    let land = |write: &Value| {
        fs::write(
            checkout.vault().join(write["path"].as_str().unwrap()),
            write["content"].as_str().unwrap(),
        )
        .unwrap()
    };
    land(&record["writes"][1]);
    land(&record["writes"][0]);
    checkout.refused(&["recover", "--abandon", operation], "REQUEST_INVALID");
    let recovered = checkout.ok(&["recover"]);
    assert_eq!(recovered["completed"][0]["operation_id"], operation);

    let request = checkout.request(json!([
        {"type": "edit-module", "subject": "Bank", "set": {"order": 9}},
        {"type": "edit-module", "subject": "Billing", "set": {"order": 9}},
    ]));
    let record = interrupt(&checkout, &request);
    land(&record["writes"][0]);
    let out = checkout.ok(&[
        "recover",
        "--abandon",
        request["operation_id"].as_str().unwrap(),
    ]);
    assert_eq!(out["landed"], json!([record["writes"][0]["path"]]));
    assert_eq!(checkout.ok(&["recover"])["completed"], json!([]));
}

#[test]
fn reads_point_to_an_interrupted_edit() {
    let checkout = Checkout::new();
    shop(&checkout);
    let read = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model"])
            .args(args)
            .current_dir(checkout.path())
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        (value, String::from_utf8(out.stderr).unwrap())
    };
    // Reading creates no state folder and warns about nothing.
    let (value, stderr) = read(&["read"]);
    assert!(value.get("pending_operation").is_none());
    assert_eq!(stderr, "");
    assert!(!checkout.path().join(".git/boundrail").exists());

    let request =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
    interrupt(&checkout, &request);
    let operation = &request["operation_id"];
    for args in [&["read"][..], &["read", "Bank"], &["check"]] {
        let (value, stderr) = read(args);
        assert_eq!(value["success"], true);
        assert_eq!(value["pending_operation"]["operation_id"], *operation);
        assert!(stderr.contains("boundrail model recover"), "{stderr}");
        assert!(stderr.contains("--abandon"), "{stderr}");
    }
    // While a writer holds the lock, the edit is still running rather than interrupted.
    let lock = fs::File::open(checkout.path().join(".git/boundrail/model.lock")).unwrap();
    lock.try_lock().unwrap();
    let (value, stderr) = read(&["read"]);
    assert_eq!(value["pending_operation"]["in_progress"], true);
    assert!(stderr.contains("in progress"), "{stderr}");
    assert!(!stderr.contains("recover"), "{stderr}");
    drop(lock);
    let (value, _) = read(&["read"]);
    assert_eq!(value["pending_operation"]["in_progress"], false);

    checkout.write("modules/BankApi1.module.json", "{ broken");
    let (value, _) = read(&["read"]);
    assert_eq!(value["error"]["code"], "MODEL_INVALID");
    assert_eq!(
        value["error"]["details"]["pending_operation"]["operation_id"],
        *operation
    );
}

#[test]
fn a_move_cannot_nest_one_writers_claim_inside_anothers() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let bank = checkout.claim("carol", json!(["Bank"]));
    let billing = checkout.claim("bob", json!(["Billing"]));
    let into = |parent: &str| json!([{"type": "edit-module", "subject": "Platform", "set": {"parent": parent}}]);
    let error = checkout.edit_refused(
        &checkout.request_by("carol", None, into("Bank")),
        "CLAIM_CONFLICT",
    );
    assert_eq!(
        error["details"]["conflicts"],
        json!([{
            "module": q("Platfrm1"),
            "inside": {"id": billing["id"], "writer": "bob", "subjects": [q("Billing1")]},
            "above": {"id": bank["id"], "writer": "carol", "subjects": [q("BankApi1")]},
        }])
    );

    // A writer without claims may move Platform under a module nobody holds.
    checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Hub", "kind": "system", "order": 2}},
        {"type": "edit-module", "subject": "Platform", "set": {"parent": "Hub"}},
    ]));

    // A writer may move its own claimed modules into each other.
    let (code, out) = checkout.release(&bank, "carol");
    assert_eq!(code, 0, "{out:#}");
    checkout.claim("bob", json!(["Bank"]));
    let (code, out) = checkout.edit(&checkout.request_by("bob", None, into("Bank")));
    assert_eq!(code, 0, "{out:#}");
}

#[test]
fn claim_and_release_retries_return_the_recorded_output() {
    let checkout = Checkout::new();
    shop(&checkout);
    let read = checkout.ok(&["read"]);
    let claim = json!({
        "target": {"model": read["manifest"]["id"], "state": read["manifest"]["current"]["id"]},
        "writer": "alice",
        "reason": "alice works here",
        "subjects": ["Bank"],
        "operation_id": "claim-1",
    });
    let args = ["claim", "--doing", "reserve the bank"];
    let (code, first) = checkout.pipe(&args, &claim);
    assert_eq!(code, 0, "{first:#}");
    let (code, again) = checkout.pipe(&args, &claim);
    assert_eq!(code, 0, "{again:#}");
    assert_eq!(again, first);
    assert_eq!(checkout.claims().as_array().unwrap().len(), 1);
    let mut other = claim.clone();
    other["subjects"] = json!(["Web"]);
    let (code, out) = checkout.pipe(&args, &other);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("OPERATION_REUSED"))
    );
    let mut bad = claim.clone();
    bad["operation_id"] = "a/b".into();
    let (code, out) = checkout.pipe(&args, &bad);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("REQUEST_INVALID"))
    );

    let release =
        json!({"claim": first["claim"]["id"], "writer": "alice", "operation_id": "release-1"});
    let (code, released) = checkout.pipe(&["release"], &release);
    assert_eq!(code, 0, "{released:#}");
    let (code, again) = checkout.pipe(&["release"], &release);
    assert_eq!(code, 0, "{again:#}");
    assert_eq!(again, released);
    assert_eq!(checkout.claims(), json!([]));
    let mut other = release.clone();
    other["writer"] = "bob".into();
    let (code, out) = checkout.pipe(&["release"], &other);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("OPERATION_REUSED"))
    );

    // A record left pending by a crash counts once the ledger shows its effect, and is redone
    // when the ledger does not.
    let path = checkout
        .path()
        .join(".git/boundrail/operations/release-1.json");
    let mut record: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        (&record["kind"], &record["status"]),
        (&json!("release"), &json!("done"))
    );
    record["status"] = "pending".into();
    fs::write(&path, record.to_string()).unwrap();
    let (code, again) = checkout.pipe(&["release"], &release);
    assert_eq!(code, 0, "{again:#}");
    assert_eq!(again, released);
    let path = checkout
        .path()
        .join(".git/boundrail/operations/claim-1.json");
    let mut record: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(record["kind"], "claim");
    record["status"] = "pending".into();
    fs::write(&path, record.to_string()).unwrap();
    let (code, redone) = checkout.pipe(&args, &claim);
    assert_eq!(code, 0, "{redone:#}");
    assert_ne!(redone["claim"]["id"], first["claim"]["id"]);
    assert_eq!(checkout.claims(), json!([redone["claim"]]));

    // Claims and edits share the operation IDs.
    let mut edit =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    edit["operation_id"] = "claim-1".into();
    checkout.edit_refused(&edit, "OPERATION_REUSED");
}

#[test]
fn an_edit_refuses_a_file_written_while_it_was_planned() {
    let checkout = Checkout::new();
    shop(&checkout);
    let request =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
    let path = "modules/WebApp01.module.json";
    let (code, stdout) = checkout.edit_with(
        &request,
        "reorder",
        &[("BOUNDRAIL_TEST_TOUCH_BEFORE_PLAN_CHECK", path)],
    );
    assert_eq!(code, 2, "{stdout}");
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["error"]["code"], "STALE_REVISION");
    assert_eq!(value["error"]["details"]["paths"], json!([path]));
    assert_eq!(value["error"]["details"]["current"], checkout.digest());
    assert_eq!(checkout.read_module("BankApi1")["order"], 1);
    let operations = checkout.path().join(".git/boundrail/operations");
    let operation = request["operation_id"].as_str().unwrap();
    assert!(!operations.join(format!("{operation}.json")).exists());
    assert!(
        !operations
            .join(format!("pending/{operation}.json"))
            .exists()
    );
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
}

fn bind(checkout: &Checkout, id: &str, path: &str) {
    let mut module = checkout.read_module(id);
    module["binding"] = json!({"path": path});
    checkout.module(module);
}

#[test]
fn check_passes_a_bound_model_and_fails_on_a_warning() {
    let checkout = Checkout::new();
    checkout.refused(&["check"], "VAULT_NOT_FOUND");
    let model = shop(&checkout);
    bind(&checkout, "WebApp01", "shop/web");
    bind(&checkout, "Billing1", "shop/billing");

    let (code, out) = checkout.run(&["check", "--only", "semantic"]);
    assert_eq!(code, 1, "{out:#}");
    assert!(!checkout.path().join(".git/boundrail").exists());
    assert_eq!(out["schema"], "boundrail/model/v1");
    assert_eq!(out["kind"], "check");
    assert_eq!(out["success"], true);
    assert_eq!(out["passed"], false);
    assert_eq!(out["counts"], json!({"warning": 1, "information": 0}));
    assert_eq!(out["revision"]["digest"], checkout.digest());
    assert_eq!(
        out["diagnostics"],
        json!([{
            "code": "CONTAINER_UNBOUND",
            "severity": "warning",
            "subjects": [format!("{model}#Platfrm1")],
            "message": "Platform has parts but no binding",
            "suggested_binding": "shop",
        }])
    );

    bind(&checkout, "Platfrm1", "shop");
    let out = checkout.ok(&["check", "--only", "semantic"]);
    assert_eq!(out["passed"], true);
    assert_eq!(out["diagnostics"], json!([]));
    assert_eq!(out["counts"], json!({"warning": 0, "information": 0}));
}

#[test]
fn check_information_never_fails_and_markdown_groups_by_severity() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    bind(&checkout, "Platfrm1", "shop");
    let mut bank = checkout.read_module("BankApi1");
    bank["outgoing"] =
        json!([{"id": "RelBnk01", "to": format!("{model}#Platfrm1"), "kind": "event", "order": 0}]);
    checkout.module(bank);

    let out = checkout.ok(&["check", "--only", "semantic"]);
    assert_eq!(out["passed"], true);
    assert_eq!(out["counts"], json!({"warning": 0, "information": 1}));
    assert_eq!(out["diagnostics"][0]["code"], "RELATIONSHIP_ON_CONTAINER");

    bind(&checkout, "WebApp01", "web");
    let (code, text) = checkout.run_raw(&["check", "--only", "semantic", "--format", "markdown"]);
    assert_eq!(code, 1, "{text}");
    assert_eq!(
        text,
        "Warnings:\n\
         - CONTAINER_PARTS_OUTSIDE Web is bound to web, outside Platform at shop\n\
         Information:\n\
         - RELATIONSHIP_ON_CONTAINER Bank -> Platform ends on a container; draw it between parts\n\
         Check failed: warnings: 1, information: 1\n"
    );
}

#[test]
fn binding_and_anchor_paths_stay_inside_the_repository() {
    let checkout = Checkout::new();
    shop(&checkout);
    bind(&checkout, "Platfrm1", ".");
    checkout.ok(&["read"]);
    let cases = [
        ("", "src/a.rs", "Billing1", "binding path"),
        ("/etc", "src/a.rs", "Billing1", "binding path"),
        ("src", "src/../../a.rs", "RelWeb01", "anchor path"),
    ];
    for (binding, anchor, subject, field) in cases {
        bind(&checkout, "Billing1", binding);
        let mut web = checkout.read_module("WebApp01");
        web["outgoing"][0]["anchor"] = json!({"path": anchor, "symbol": "charge"});
        checkout.module(web);
        let error = checkout.refused(&["read"], "MODEL_INVALID");
        let problems = error["details"]["problems"].as_array().unwrap();
        let message = format!("{field} is not a relative path inside the repository");
        assert!(
            problems
                .iter()
                .any(|p| p["subject"] == subject && p["message"] == message),
            "{binding} {anchor}: {problems:#?}"
        );
    }

    bind(&checkout, "Billing1", "src/billing");
    let mut web = checkout.read_module("WebApp01");
    web["outgoing"][0]["anchor"] = json!({"path": "src/web.rs", "symbol": "charge"});
    checkout.module(web);
    let request = checkout.request(json!([
        {"type": "edit-module", "subject": "Bank", "set": {"binding": {"path": "vendor/../../bank"}}}
    ]));
    let error = checkout.edit_refused(&request, "MODEL_INVALID");
    assert_eq!(error["details"]["problems"][0]["subject"], "BankApi1");
}

/// A TypeScript checkout: Web imports Billing and Billing imports `src/bank`.
fn typescript(checkout: &Checkout) {
    for (path, text) in [
        ("package.json", r#"{"name": "fixture"}"#),
        (
            "src/web/index.ts",
            "import { charge } from \"../billing\";\nexport const page = () => charge(1);\n",
        ),
        (
            "src/billing/index.ts",
            "import { pay } from \"../bank\";\nexport const charge = (n: number) => pay(n);\n",
        ),
        (
            "src/bank/index.ts",
            "export const pay = (n: number) => n;\n",
        ),
    ] {
        let file = checkout.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
}

/// Each diagnostic as `CODE subject, subject`.
fn found(out: &Value) -> Vec<String> {
    out["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let subjects: Vec<&str> = d["subjects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().split_once('#').unwrap().1)
                .collect();
            format!("{} {}", d["code"].as_str().unwrap(), subjects.join(", "))
        })
        .collect()
}

#[test]
fn check_compares_the_model_with_the_code() {
    let checkout = Checkout::new();
    typescript(&checkout);
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    bind(&checkout, "Platfrm1", "src");
    bind(&checkout, "WebApp01", "src/web");
    bind(&checkout, "Billing1", "src/billing");

    // Web -> Billing has its import; Billing -> Bank ends on an external module.
    let out = checkout.ok(&["check"]);
    assert_eq!(out["passed"], true, "{out:#}");
    assert_eq!(out["diagnostics"], json!([]));
    assert_eq!(
        out["skips"],
        json!([
            {"subject": q("BankApi1"), "site": "binding", "reason": "external"},
            {"subject": q("RelBil01"), "site": "relationship", "reason": "external"},
        ])
    );

    let mut ledger = module("Ledger01", "Ledger", "module", 2);
    ledger["parent"] = q("Platfrm1").into();
    ledger["binding"] = json!({"path": "src/bank"});
    checkout.module(ledger.clone());
    let mut ghost = module("Ghost001", "Ghost", "module", 3);
    ghost["parent"] = q("Platfrm1").into();
    ghost["binding"] = json!({"path": "src/ghost"});
    checkout.module(ghost);
    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(
        found(&out),
        [
            "BINDING_PATH_MISSING Ghost001",
            "RELATIONSHIP_UNDRAWN Billing1, Ledger01"
        ]
    );
    assert_eq!(
        out["diagnostics"][1]["message"],
        "Billing uses Ledger but no relationship is drawn: src/billing/index.ts:1 imports src/bank/index.ts"
    );

    // A line drawn against the import reports it once, as backwards.
    ledger["outgoing"] = json!([
        {"id": "RelLed01", "to": q("WebApp01"), "kind": "call", "order": 0},
        {"id": "RelLed02", "to": q("Billing1"), "kind": "call", "order": 1},
    ]);
    checkout.module(ledger);
    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(
        found(&out),
        [
            "BINDING_PATH_MISSING Ghost001",
            "RELATIONSHIP_UNSUPPORTED RelLed01",
            "RELATIONSHIP_BACKWARDS RelLed02",
        ]
    );

    let (code, text) = checkout.run_raw(&["check", "--format", "markdown"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.ends_with(
            "Skipped:\n- binding path-missing: 1\n- binding external: 1\n- relationship external: 1\n\
             Check failed: warnings: 3, information: 0\n"
        ),
        "{text}"
    );
}

#[test]
fn check_prints_github_annotations_at_the_file_the_evidence_names() {
    let checkout = Checkout::new();
    typescript(&checkout);
    let model = shop(&checkout);
    bind(&checkout, "Platfrm1", "src");
    bind(&checkout, "WebApp01", "src/web");
    bind(&checkout, "Billing1", "src/billing");
    let mut ledger = module("Ledger01", "Ledger", "module", 2);
    ledger["parent"] = format!("{model}#Platfrm1").into();
    ledger["binding"] = json!({"path": "src/bank"});
    checkout.module(ledger);
    let mut ghost = module("Ghost001", "Ghost", "module", 3);
    ghost["parent"] = format!("{model}#Platfrm1").into();
    ghost["binding"] = json!({"path": "src/ghost"});
    checkout.module(ghost);

    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(out["diagnostics"][0]["file"], "src/ghost");
    assert_eq!(out["diagnostics"][0].get("line"), None);
    assert_eq!(out["diagnostics"][1]["file"], "src/billing/index.ts");
    assert_eq!(out["diagnostics"][1]["line"], 1);
    let (code, text) = checkout.run_raw(&["check", "--format", "annotations"]);
    assert_eq!(code, 1, "{text}");
    assert_eq!(
        text,
        "::warning file=src/ghost,title=BINDING_PATH_MISSING::Ghost is bound to src/ghost, which \
         does not exist\n\
         ::warning file=src/billing/index.ts,line=1,title=RELATIONSHIP_UNDRAWN::Billing uses \
         Ledger but no relationship is drawn: src/billing/index.ts:1 imports src/bank/index.ts\n"
    );
    let error = checkout.refused(&["read", "--format", "annotations"], "REQUEST_INVALID");
    assert!(
        error["message"].as_str().unwrap().contains("model check"),
        "{error:#}"
    );
}

#[test]
fn check_runs_one_check_with_only() {
    let checkout = Checkout::new();
    typescript(&checkout);
    shop(&checkout);
    bind(&checkout, "WebApp01", "src/web");
    bind(&checkout, "Billing1", "src/gone");

    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["BINDING_PATH_MISSING Billing1"]);
    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(
        found(&out),
        [
            "CONTAINER_UNBOUND Platfrm1",
            "BINDING_PATH_MISSING Billing1"
        ]
    );
}

#[test]
fn check_without_a_scan_keeps_the_semantic_check_and_skips_the_code() {
    // Without source code the scan fails; the semantic check never scans.
    let checkout = Checkout::new();
    let model = shop(&checkout);
    fs::create_dir(checkout.path().join("docs")).unwrap();
    fs::write(checkout.path().join("docs/README.md"), "notes").unwrap();
    bind(&checkout, "Platfrm1", "docs");
    let out = checkout.ok(&["check", "--only", "semantic"]);
    assert_eq!(out["passed"], true);
    assert!(out.get("skips").is_none(), "{out:#}");

    // The failed scan alone fails the check.
    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["SCAN_FAILED Platfrm1"]);
    let message = out["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("the scan of the checkout failed: ") && !message.contains('\n'),
        "{message}"
    );

    bind(&checkout, "WebApp01", "web");
    let (code, out) = checkout.run(&["check"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(
        found(&out),
        [
            "CONTAINER_PARTS_OUTSIDE Platfrm1, WebApp01",
            "BINDING_PATH_MISSING WebApp01",
            "SCAN_FAILED Platfrm1"
        ]
    );
    let skip =
        json!({"subject": format!("{model}#Platfrm1"), "site": "binding", "reason": "scan-failed"});
    assert!(out["skips"].as_array().unwrap().contains(&skip), "{out:#}");
}

#[test]
fn check_judges_symbols_only_where_the_scan_indexes_them() {
    let checkout = Checkout::new();
    typescript(&checkout);
    let model = shop(&checkout);
    fs::write(
        checkout.path().join("src/bank/pay.js"),
        "function charge(n) { return n; }\nmodule.exports = { charge };\n",
    )
    .unwrap();
    let mut billing = checkout.read_module("Billing1");
    billing["binding"] = json!({"path": "src/bank/pay.js", "symbol": "charge"});
    checkout.module(billing);
    let mut web = checkout.read_module("WebApp01");
    web["binding"] = json!({"path": "src/web/index.ts", "symbol": "missing"});
    checkout.module(web);

    // CommonJS exports are not indexed, so their absence proves nothing; ES exports are.
    let out = checkout.run(&["check", "--only", "code"]).1;
    assert_eq!(found(&out), ["BINDING_SYMBOL_MISSING WebApp01"]);
    let skip = json!({"subject": format!("{model}#Billing1"), "site": "binding", "reason": "symbols-incomplete"});
    assert!(out["skips"].as_array().unwrap().contains(&skip), "{out:#}");
}

#[test]
fn recovery_finishes_a_landed_edit_whose_result_no_longer_validates() {
    let checkout = Checkout::new();
    shop(&checkout);
    let request =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 7}}]));
    let operation = request["operation_id"].as_str().unwrap();
    let mut record = interrupt(&checkout, &request);
    // As if an older build planned a module kind this build's vocabulary lacks.
    let path = "modules/BankApi1.module.json";
    let writes = record["writes"].as_array_mut().unwrap();
    let write = writes.iter_mut().find(|w| w["path"] == path).unwrap();
    let mut bank: Value = serde_json::from_str(write["content"].as_str().unwrap()).unwrap();
    bank["kind"] = "lambda".into();
    let content = serde_json::to_string_pretty(&bank).unwrap() + "\n";
    write["content"] = content.clone().into();
    let files = record["output"]["after"]["files"].as_array_mut().unwrap();
    files.iter_mut().find(|f| f["path"] == path).unwrap()["sha256"] =
        sha256(content.as_bytes()).into();
    let lines: String = files
        .iter()
        .map(|f| {
            format!(
                "{}\0{}\n",
                f["path"].as_str().unwrap(),
                f["sha256"].as_str().unwrap()
            )
        })
        .collect();
    let digest = sha256(lines.as_bytes());
    record["output"]["after"]["digest"] = digest.clone().into();
    record["after"] = digest.into();
    let journal = checkout.path().join(".git/boundrail/operations");
    let pending = journal.join(format!("pending/{operation}.json"));
    fs::write(&pending, record.to_string()).unwrap();
    for write in record["writes"].as_array().unwrap() {
        let file = checkout.vault().join(write["path"].as_str().unwrap());
        match write["content"].as_str() {
            Some(content) => fs::write(file, content).unwrap(),
            None => fs::remove_file(file).unwrap(),
        }
    }

    let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model", "recover"])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let recovered: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(recovered["completed"][0]["operation_id"], operation);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains(&format!(
            "warning: operation {operation} is complete, but the model does not load"
        )),
        "{stderr}"
    );
    assert!(!pending.exists());
    assert!(journal.join(format!("{operation}.json")).exists());
    checkout.refused(&["read"], "MODEL_INVALID");
}

/// The changes that add Orders, holding Checkout, to the shop: Web places an order with
/// Billing, which charges Bank when the card is valid and declines otherwise, then reserves
/// each item.
fn checkout_changes() -> Value {
    json!([
        {"type": "add-collection", "collection": {"name": "Orders", "order": 0, "description": "Buying"}},
        {"type": "add-flow", "collection": "Orders", "flow": {"name": "Checkout", "order": 0, "summary": "A customer pays", "steps": [
            {"kind": "message", "from": "Web", "to": "Billing", "label": "place order", "relationship": "RelWeb01"},
            {"kind": "decision", "label": "card valid?", "branches": [
                {"label": "yes", "steps": [
                    {"kind": "message", "from": "Billing", "to": "Bank", "label": "charge", "relationship": "RelBil01", "repeat": 3},
                ]},
                {"label": "no", "steps": [
                    {"kind": "message", "from": "Billing", "to": "Web", "label": "decline", "relationship": "RelWeb01", "reply": true},
                ]},
            ]},
            {"kind": "loop", "label": "each item", "steps": [
                {"kind": "message", "from": "Billing", "to": "Billing", "label": "reserve"},
            ]},
        ]}},
    ])
}

/// Adds Checkout and returns the qualified IDs allocated: Orders, Checkout, place order, card
/// valid?, yes, charge, no, decline, each item, reserve.
fn add_checkout(checkout: &Checkout) -> Vec<String> {
    let out = checkout.edited(checkout_changes());
    let allocated = out["allocated"].as_array().unwrap();
    allocated
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect()
}

fn local(id: &str) -> &str {
    id.split_once('#').unwrap().1
}

#[test]
fn a_collection_flow_and_steps_added_in_one_batch_read_back() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let request = checkout.request(checkout_changes());
    let (_, schema) = checkout.run(&["schema", "model-edit"]);
    let errors: Vec<String> = jsonschema::validator_for(&schema)
        .unwrap()
        .iter_errors(&request)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let (code, out) = checkout.edit(&request);
    assert_eq!(code, 0, "{out:#}");
    let ids: Vec<String> = out["allocated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect();
    let (orders, flow) = (&ids[0], &ids[1]);
    assert_eq!(out["subjects"]["added"], json!(sorted(&[orders, flow])));
    let names: Vec<&Value> = out["allocated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["name"])
        .collect();
    assert_eq!(
        names,
        [
            "Orders",
            "Checkout",
            "message place order",
            "decision card valid?",
            "branch yes",
            "message charge",
            "branch no",
            "message decline",
            "loop each item",
            "message reserve"
        ]
    );

    let read = checkout.ok(&["read", "--flow", flow]);
    assert_eq!(read["kind"], "read-flow");
    assert_eq!(read["collection"], *orders);
    assert_eq!(read["revision"]["digest"], checkout.digest());
    let steps = &read["flow"]["steps"];
    assert_eq!(read["flow"]["id"], *flow);
    assert_eq!(
        steps[0],
        json!({"kind": "message", "id": ids[2], "from": q("WebApp01"), "to": q("Billing1"),
               "label": "place order", "relationship": q("RelWeb01")})
    );
    assert_eq!(steps[1]["branches"][1]["id"], ids[6]);
    assert_eq!(steps[1]["branches"][1]["steps"][0]["reply"], true);
    assert_eq!(
        checkout.ok(&["read", "--flow", "Checkout"])["flow"],
        read["flow"]
    );
    assert_eq!(checkout.ok(&["read", local(flow)])["flow"], read["flow"]);
    let (code, text) = checkout.run_raw(&["read", "--flow", flow, "--format", "markdown"]);
    assert_eq!(code, 0);
    assert_eq!(
        text,
        format!(
            "Flow Checkout {flow} in Orders\n\
             A customer pays\n\
             1. Web -> Billing: place order\n\
             2. decision: card valid?\n  \
               - yes:\n    \
                 1. Billing -> Bank: charge x3\n  \
               - no:\n    \
                 1. Billing -> Web: decline (reply)\n\
             3. loop: each item\n  \
               1. Billing -> Billing: reserve\n"
        )
    );

    let read = checkout.ok(&["read", orders]);
    assert_eq!(read["kind"], "read-collection");
    let listed = json!({"id": orders, "name": "Orders", "order": 0, "description": "Buying",
        "flows": [{"id": flow, "name": "Checkout", "order": 0, "summary": "A customer pays"}]});
    assert_eq!(read["collection"], listed);
    assert_eq!(checkout.ok(&["read"])["collections"], json!([listed]));
    let (_, text) = checkout.run_raw(&["read", "--format", "markdown"]);
    assert!(
        text.ends_with(&format!(
            "Collections:\n- Orders {orders}\n  - Checkout {flow}: A customer pays\n"
        )),
        "{text}"
    );
    checkout.refused(&["read", "--flow", "Missing"], "SUBJECT_NOT_FOUND");

    let file = checkout
        .vault()
        .join(format!("collections/{}.collection.json", local(orders)));
    let file: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let (_, schema) = checkout.run(&["schema", "model-collection"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&file));
    let mut bad = file.clone();
    bad["flows"][0]["steps"][0]["colour"] = "red".into();
    assert!(!validator.is_valid(&bad));
    let files = checkout.ok(&["read"])["revision"]["files"].clone();
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == format!("collections/{}.collection.json", local(orders)))
    );
}

#[test]
fn a_step_moves_across_a_decisions_branches() {
    let checkout = Checkout::new();
    shop(&checkout);
    let ids = add_checkout(&checkout);
    let (yes, decline) = (&ids[4], &ids[7]);
    let out = checkout.edited(json!([
        {"type": "move-step", "subject": decline, "flow": "Checkout", "parent": yes, "position": 0},
    ]));
    assert_eq!(
        out["subjects"]["changed"],
        json!(sorted(&[&ids[0], &ids[1]]))
    );
    let read = checkout.ok(&["read", "--flow", &ids[1]]);
    let branches = &read["flow"]["steps"][1]["branches"];
    let moved: Vec<&Value> = branches[0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["id"])
        .collect();
    assert_eq!(moved, [decline, &ids[5]]);
    assert_eq!(branches[1]["steps"], json!([]));
}

#[test]
fn removing_a_module_a_step_references_refuses() {
    let checkout = Checkout::new();
    shop(&checkout);
    let ids = add_checkout(&checkout);
    let error = checkout.edit_refused(
        &checkout.request(json!([{"type": "remove-module", "subject": "Bank"}])),
        "REFERENCED_BY_FLOW",
    );
    assert_eq!(error["details"]["steps"], json!([ids[5]]));
    checkout.edited(json!([
        {"type": "remove-step", "subject": ids[5]},
        {"type": "remove-module", "subject": "Bank"},
    ]));
}

#[test]
fn a_flow_claim_blocks_another_writers_step_edit() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let ids = add_checkout(&checkout);
    let (orders, flow) = (&ids[0], &ids[1]);
    let claim = checkout.claim("alice", json!([flow]));
    assert_eq!(claim["subjects"], json!([flow]));
    let rename = json!([{"type": "edit-step", "subject": ids[2], "set": {"label": "order"}}]);
    let error = checkout.edit_refused(
        &checkout.request_by("bob", None, rename.clone()),
        "CLAIM_CONFLICT",
    );
    assert_eq!(error["details"]["conflicts"][0]["subjects"], json!([flow]));
    // Holding a flow conflicts with holding its collection.
    checkout.claim_refused("bob", json!([orders]), "CLAIM_CONFLICT");
    let (code, out) = checkout.edit(&checkout.request_by("alice", Some(&claim), rename));
    assert_eq!(code, 0, "{out:#}");
    let read = checkout.ok(&["read", "--flow", flow]);
    assert_eq!(read["claims"][0]["id"], claim["id"]);

    // Adding a collection needs the root level, which the model ID holds.
    let add = json!([{"type": "add-collection", "collection": {"name": "Returns", "order": 1}}]);
    let error = checkout.edit_refused(
        &checkout.request_by("alice", Some(&claim), add),
        "CLAIM_INCOMPLETE",
    );
    assert_eq!(error["details"]["missing"], json!([model]));
}

#[test]
fn check_reports_flows_without_a_summary_and_undrawn_messages() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    bind(&checkout, "Platfrm1", "shop");
    checkout.edited(json!([
        {"type": "add-collection", "collection": {"name": "Orders", "order": 0}},
        {"type": "add-flow", "collection": "Orders", "flow": {"name": "Refund", "order": 0, "steps": [
            {"kind": "message", "from": "Web", "to": "Bank", "label": "refund"},
        ]}},
    ]));
    let out = checkout.ok(&["check", "--only", "semantic"]);
    assert_eq!(out["passed"], true);
    assert_eq!(out["counts"], json!({"warning": 0, "information": 2}));
    let codes: Vec<&Value> = out["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| &d["code"])
        .collect();
    assert_eq!(codes, ["FLOW_SUMMARY_MISSING", "MESSAGE_UNDRAWN"]);
    assert_eq!(out["diagnostics"][0]["message"], "Refund has no summary");
    assert_eq!(
        out["diagnostics"][1]["subjects"][1],
        format!("{model}#WebApp01")
    );
}

#[test]
fn taking_a_claimed_collections_or_flows_name_conflicts() {
    let checkout = Checkout::new();
    shop(&checkout);
    let ids = add_checkout(&checkout);
    checkout.claim("alice", json!([ids[0]]));
    let error = checkout.edit_refused(
        &checkout.request_by(
            "bob",
            None,
            json!([{"type": "add-collection", "collection": {"name": "Orders", "order": 1}}]),
        ),
        "CLAIM_CONFLICT",
    );
    assert_eq!(
        error["details"]["conflicts"][0]["subjects"],
        json!([ids[0]])
    );
    let error = checkout.edit_refused(
        &checkout.request_by(
            "bob",
            None,
            json!([
                {"type": "add-collection", "collection": {"name": "Returns", "order": 1}},
                {"type": "add-flow", "collection": "Returns", "flow": {"name": "Checkout", "order": 0}},
            ]),
        ),
        "CLAIM_CONFLICT",
    );
    assert_eq!(
        error["details"]["conflicts"][0]["subjects"],
        json!([ids[1]])
    );
}

#[test]
fn a_renamed_flow_keeps_its_old_name() {
    let checkout = Checkout::new();
    shop(&checkout);
    let ids = add_checkout(&checkout);
    let (orders, flow) = (&ids[0], &ids[1]);
    checkout
        .edited(json!([{"type": "edit-flow", "subject": "Checkout", "set": {"name": "Purchase"}}]));
    let read = checkout.ok(&["read", "--flow", "Checkout"]);
    assert_eq!(read["flow"]["id"], *flow);
    assert_eq!(read["flow"]["name"], "Purchase");
    let file = checkout
        .vault()
        .join(format!("collections/{}.collection.json", local(orders)));
    let stored: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(stored["flows"][0]["former_names"], json!(["Checkout"]));
    assert_eq!(stored.get("former_names"), None);

    // A claim on the renamed flow protects its old name.
    let claim = checkout.claim("alice", json!([flow]));
    let take = json!([{"type": "add-flow", "collection": "Orders", "flow": {"name": "Checkout", "order": 1}}]);
    let error = checkout.edit_refused(
        &checkout.request_by("bob", None, take.clone()),
        "CLAIM_CONFLICT",
    );
    assert_eq!(error["details"]["conflicts"][0]["subjects"], json!([flow]));
    let (code, out) = checkout.release(&claim, "alice");
    assert_eq!(code, 0, "{out:#}");

    // Once a new flow takes the name, it names two flows.
    checkout.edited(take);
    let error = checkout.refused(&["read", "--flow", "Checkout"], "SUBJECT_AMBIGUOUS");
    assert_eq!(error["details"]["matches"].as_array().unwrap().len(), 2);
    assert_eq!(
        checkout.ok(&["read", "--flow", "Purchase"])["flow"]["id"],
        *flow
    );
}

/// A checkout with an initial commit and a local identity, so checkpoints can be committed.
fn committed() -> Checkout {
    let checkout = Checkout::new();
    for args in [
        &["config", "user.name", "tester"][..],
        &["config", "user.email", "tester@example.com"],
        &["config", "commit.gpgsign", "false"],
        &["commit", "-q", "--allow-empty", "-m", "initial"],
    ] {
        git(&checkout, args);
    }
    checkout
}

fn git(checkout: &Checkout, args: &[&str]) -> String {
    git_with(checkout, &[], args)
}

fn git_with(checkout: &Checkout, env: &[(&str, &Path)], args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// The checkout's HEAD, branches, index and working tree status.
fn checkout_state(checkout: &Checkout) -> Vec<String> {
    let index = fs::read(checkout.path().join(".git/index")).unwrap_or_default();
    vec![
        git(checkout, &["rev-parse", "HEAD"]),
        git(checkout, &["for-each-ref", "refs/heads"]),
        git(
            checkout,
            &["status", "--porcelain", "--untracked-files=all"],
        ),
        sha256(&index),
    ]
}

#[test]
fn checkpoints_keep_the_model_history_off_the_working_branch() {
    let checkout = committed();
    shop(&checkout);
    assert_eq!(checkout.ok(&["checkpoints"])["checkpoints"], json!([]));
    let before = checkout_state(&checkout);
    let head = git(&checkout, &["rev-parse", "HEAD"]);

    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    assert_eq!(checkout_state(&checkout), before);
    assert_eq!(first["doing"], "the shop as built");
    assert_eq!(first["writer"], "tester");
    assert_eq!(first["source"], head.trim());
    assert_eq!(first["revision"], checkout.digest());
    assert_eq!(first["parent"], Value::Null);
    assert_eq!(first["id"].as_str().unwrap().len(), 8);
    let tip = git(&checkout, &["rev-parse", "refs/boundrail/history"]);
    assert_eq!(first["commit"], tip.trim());
    let files = git(&checkout, &["ls-tree", "-r", "--name-only", tip.trim()]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        [
            "config.yaml",
            "model.json",
            "modules/BankApi1.module.json",
            "modules/Billing1.module.json",
            "modules/Platfrm1.module.json",
            "modules/WebApp01.module.json",
        ]
    );

    let error = checkout.refused(&["retain", "--doing", "again"], "NOTHING_TO_RETAIN");
    assert_eq!(error["details"]["checkpoint"], first["id"]);

    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let second =
        checkout.ok(&["retain", "--doing", "move the bank", "--writer", "none"])["checkpoint"]
            .clone();
    assert_eq!(second["parent"], first["id"]);
    assert_eq!(second["writer"], "none");
    assert_eq!(second["operation_id"], Value::Null);
    assert_eq!(checkout_state(&checkout), before);

    let list = checkout.ok(&["checkpoints"])["checkpoints"].clone();
    assert_eq!(list, json!([second, first]));
    let list = checkout.ok(&["checkpoints", "--limit", "1"])["checkpoints"].clone();
    assert_eq!(list, json!([second]));

    // The older checkpoint reads back the older model, whole or by subject.
    let id = first["id"].as_str().unwrap();
    let old = checkout.ok(&["read", "--checkpoint", &id.to_ascii_lowercase()]);
    assert_eq!(old["checkpoint"], first);
    assert_eq!(old["revision"]["digest"], first["revision"]);
    let bank = |read: &Value| {
        read["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "Bank")
            .unwrap()["order"]
            .clone()
    };
    assert_eq!(bank(&old), 1);
    assert_eq!(bank(&checkout.ok(&["read"])), 4);
    let prefix = &first["commit"].as_str().unwrap()[..10];
    let module = checkout.ok(&["read", "Bank", "--checkpoint", prefix]);
    assert_eq!(module["module"]["order"], 1);
    assert_eq!(module["checkpoint"]["id"], first["id"]);
    let (code, text) = checkout.run_raw(&["read", "--checkpoint", id, "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with(&format!("Checkpoint {id} by tester")),
        "{text}"
    );

    checkout.refused(
        &["read", "--checkpoint", "Unknown1"],
        "CHECKPOINT_NOT_FOUND",
    );
    checkout.refused(&["read", "--checkpoint", "abc"], "CHECKPOINT_NOT_FOUND");
    let (code, text) = checkout.run_raw(&[
        "read",
        "Nowhere",
        "--checkpoint",
        id,
        "--format",
        "markdown",
    ]);
    assert_eq!(code, 2, "{text}");
    assert!(text.starts_with("SUBJECT_NOT_FOUND"), "{text}");
    assert_eq!(checkout_state(&checkout), before);
}

#[test]
fn retain_is_idempotent_by_operation_id() {
    let checkout = committed();
    shop(&checkout);
    let args = [
        "retain",
        "--doing",
        "the shop as built",
        "--operation-id",
        "retain-1",
    ];
    let (code, first) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{first}");
    let (code, again) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again, first);

    // A record left pending by a crash after the ref moved counts as done.
    let record = checkout
        .path()
        .join(".git/boundrail/operations/retain-1.json");
    let pending = fs::read_to_string(&record)
        .unwrap()
        .replace(r#""status": "done""#, r#""status": "pending""#);
    assert!(pending.contains(r#""status": "pending""#), "{pending}");
    fs::write(&record, pending).unwrap();
    let (code, again) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again, first);

    // After the model changes, the same operation still returns its checkpoint.
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let (code, again) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again, first);
    let list = checkout.ok(&["checkpoints"])["checkpoints"].clone();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["operation_id"], "retain-1");

    checkout.refused(
        &[
            "retain",
            "--doing",
            "another reason",
            "--operation-id",
            "retain-1",
        ],
        "OPERATION_REUSED",
    );
    // An edit's operation ID cannot be reused for a retain either.
    let mut request =
        checkout.request(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 5}}]));
    request["operation_id"] = "retain-1".into();
    checkout.edit_refused(&request, "OPERATION_REUSED");
}

/// A linked worktree of `checkout`, detached at its HEAD, in a directory of its own.
fn worktree(checkout: &Checkout) -> Checkout {
    let linked = Checkout::outside();
    let path = linked.path().to_str().unwrap();
    git(checkout, &["worktree", "add", "-q", "--detach", path]);
    linked
}

#[test]
fn worktrees_share_the_history_and_its_operation_ids() {
    let checkout = committed();
    shop(&checkout);
    let args = [
        "retain",
        "--doing",
        "the shop as built",
        "--operation-id",
        "retain-1",
    ];
    let first = checkout.ok(&args)["checkpoint"].clone();

    // A worktree without a vault still reads the history.
    let linked = worktree(&checkout);
    assert!(!linked.vault().exists());
    let list = linked.ok(&["checkpoints"]);
    assert_eq!(list["checkpoints"], json!([first]));
    let id = first["id"].as_str().unwrap();
    let read = linked.ok(&["read", "--checkpoint", id]);
    assert_eq!(read["checkpoint"], first);
    assert_eq!(read["modules"].as_array().unwrap().len(), 4);
    assert!(
        read["vault"]
            .as_str()
            .unwrap()
            .ends_with(".boundrail/model")
    );
    assert!(!linked.vault().exists());

    // Its journal is its own, but the shared ref answers a retry of the same operation.
    linked.init();
    assert_eq!(linked.ok(&args)["checkpoint"], first);
    linked.refused(
        &[
            "retain",
            "--doing",
            "another reason",
            "--operation-id",
            "retain-1",
        ],
        "OPERATION_REUSED",
    );
    assert_eq!(checkout.ok(&["checkpoints"])["checkpoints"], json!([first]));
}

#[test]
fn a_locked_history_ref_refuses_and_leaves_it_unchanged() {
    let checkout = committed();
    shop(&checkout);
    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let lock = checkout.path().join(".git/refs/boundrail/history.lock");
    fs::write(&lock, "").unwrap();
    let error = checkout.refused(&["retain", "--doing", "move the bank"], "HISTORY_MOVED");
    assert_eq!(error["details"]["current"], first["commit"]);
    let tip = git(&checkout, &["rev-parse", "refs/boundrail/history"]);
    assert_eq!(tip.trim(), first["commit"]);

    fs::remove_file(&lock).unwrap();
    let second = checkout.ok(&["retain", "--doing", "move the bank"])["checkpoint"].clone();
    assert_eq!(second["parent"], first["id"]);
}

#[cfg(unix)]
#[test]
fn retain_builds_on_a_history_tip_that_is_not_a_checkpoint() {
    use std::os::unix::fs::PermissionsExt;
    let checkout = committed();
    shop(&checkout);
    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    // Another tool lands a commit without checkpoint trailers on the ref.
    let tree = git(&checkout, &["rev-parse", "refs/boundrail/history^{tree}"]);
    let foreign = git(
        &checkout,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            "refs/boundrail/history",
            "-m",
            "not a checkpoint",
        ],
    );
    let foreign = foreign.trim();
    git(
        &checkout,
        &["update-ref", "refs/boundrail/history", foreign],
    );

    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let second = checkout.ok(&["retain", "--doing", "move the bank"])["checkpoint"].clone();
    assert_eq!(second["parent"], first["id"]);
    let parent = git(&checkout, &["rev-parse", "refs/boundrail/history^"]);
    assert_eq!(parent.trim(), foreign);
    let list = checkout.ok(&["checkpoints"])["checkpoints"].clone();
    assert_eq!(list, json!([second, first]));
    // A limit counts checkpoints, not the commits between them.
    let list = checkout.ok(&["checkpoints", "--limit", "1"])["checkpoints"].clone();
    assert_eq!(list, json!([second]));
    assert_eq!(list[0]["parent"], first["id"]);
    let list = checkout.ok(&["checkpoints", "--limit", "2"])["checkpoints"].clone();
    assert_eq!(list, json!([second, first]));

    // A commit that lands while the checkpoint is written, here from inside its signer, refuses.
    let tools = TempDir::new("signer").unwrap();
    let signer = tools.path().join("sign");
    let script = format!(
        "#!/bin/sh\ncat >/dev/null\ngit update-ref refs/boundrail/history {foreign}\n\
         printf '\\n[GNUPG:] SIG_CREATED D 1 8 00 1 FAKE\\n' >&2\n\
         printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nfake\\n-----END PGP SIGNATURE-----\\n'\n"
    );
    fs::write(&signer, script).unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o755)).unwrap();
    git(&checkout, &["config", "commit.gpgsign", "true"]);
    git(&checkout, &["config", "gpg.format", "openpgp"]);
    git(
        &checkout,
        &["config", "gpg.program", signer.to_str().unwrap()],
    );
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 5}}]));
    let error = checkout.refused(&["retain", "--doing", "move it again"], "HISTORY_MOVED");
    assert_eq!(error["details"]["expected"], second["commit"]);
    assert_eq!(error["details"]["current"], foreign);
}

#[test]
fn retain_builds_on_a_history_ref_holding_an_annotated_tag() {
    let checkout = committed();
    shop(&checkout);
    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    let commit = first["commit"].as_str().unwrap();
    git(&checkout, &["tag", "-a", "-m", "tagged", "built", commit]);
    let tag = git(&checkout, &["rev-parse", "refs/tags/built"]);
    git(
        &checkout,
        &["update-ref", "refs/boundrail/history", tag.trim()],
    );

    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let second = checkout.ok(&["retain", "--doing", "move the bank"])["checkpoint"].clone();
    assert_eq!(second["parent"], first["id"]);
    let parent = git(&checkout, &["rev-parse", "refs/boundrail/history^"]);
    assert_eq!(parent.trim(), commit);
}

#[test]
fn retain_checks_the_expected_revision_when_given() {
    let checkout = committed();
    shop(&checkout);
    let reviewed = checkout.digest();
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let current = checkout.digest();
    let retain = ["retain", "--doing", "move the bank", "--expected-revision"];
    let stale = [&retain[..], &[reviewed.as_str().unwrap()]].concat();
    let error = checkout.refused(&stale, "STALE_REVISION");
    assert_eq!(error["details"]["current"], current);
    assert_eq!(checkout.ok(&["checkpoints"])["checkpoints"], json!([]));

    let fresh = [&retain[..], &[current.as_str().unwrap()]].concat();
    let retained = checkout.ok(&fresh)["checkpoint"].clone();
    assert_eq!(retained["revision"], current);
}

#[test]
fn retain_refuses_a_vault_file_too_large_to_read_back() {
    let checkout = committed();
    shop(&checkout);
    let id = add_checkout(&checkout)[0].clone();
    let path = format!("collections/{}.collection.json", local(&id));
    let file = checkout.vault().join(&path);
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(&" ".repeat(4 * 1024 * 1024));
    fs::write(&file, text).unwrap();
    let error = checkout.refused(&["retain", "--doing", "the shop as built"], "MODEL_INVALID");
    assert_eq!(error["details"]["path"], path);
    assert_eq!(checkout.ok(&["checkpoints"])["checkpoints"], json!([]));
}

/// A `Revision` trailer in the shape `model retain` writes, for forged checkpoints that must pass
/// the checks of a fetch.
const FORGED_REVISION: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// Puts a checkpoint on the history ref whose tree is the tree of `base` plus `extra` files as
/// (path, blob), and returns its ID.
fn forge(checkout: &Checkout, id: &str, base: &str, extra: &[(&str, &str)]) -> String {
    let temp = TempDir::new("forge").unwrap();
    let index = temp.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    git_with(checkout, &env, &["read-tree", base]);
    for (path, blob) in extra {
        let entry = format!("100644,{blob},{path}");
        git_with(
            checkout,
            &env,
            &["update-index", "--add", "--cacheinfo", &entry],
        );
    }
    let tree = git_with(checkout, &env, &["write-tree"]);
    let message =
        format!("forged\n\nCheckpoint: {id}\nRevision: {FORGED_REVISION}\nWriter: forger\n");
    let commit = git(
        checkout,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            "refs/boundrail/history",
            "-m",
            &message,
        ],
    );
    git(
        checkout,
        &["update-ref", "refs/boundrail/history", commit.trim()],
    );
    id.to_owned()
}

#[test]
fn a_checkpoint_tree_holding_anything_but_vault_files_is_refused() {
    let checkout = committed();
    shop(&checkout);
    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    let base = first["commit"].as_str().unwrap();
    let config = git(
        &checkout,
        &["rev-parse", "refs/boundrail/history:config.yaml"],
    );

    let id = forge(&checkout, "Forged01", base, &[("run.sh", config.trim())]);
    let error = checkout.refused(&["read", "--checkpoint", &id], "MODEL_INVALID");
    assert_eq!(error["details"]["path"], "run.sh");
    assert_eq!(error["details"]["checkpoint"], id);

    // A tree of vault files that do not load names the checkpoint, not a temporary folder.
    let id = forge(
        &checkout,
        "Forged02",
        base,
        &[("model.json", config.trim())],
    );
    let error = checkout.refused(&["read", "--checkpoint", &id], "MODEL_INVALID");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("checkpoint Forged02"), "{message}");
    assert_eq!(error["details"]["checkpoint"], id);
    assert_eq!(error["details"]["problems"][0]["file"], "model.json");
}

#[cfg(unix)]
#[test]
fn checkpoints_follow_the_users_signing_config() {
    use std::os::unix::fs::PermissionsExt;
    let checkout = committed();
    shop(&checkout);
    let tools = TempDir::new("signer").unwrap();
    let signer = tools.path().join("sign");
    let script = |body: &str| {
        fs::write(&signer, format!("#!/bin/sh\ncat >/dev/null\n{body}")).unwrap();
        fs::set_permissions(&signer, fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(
        "printf '\\n[GNUPG:] SIG_CREATED D 1 8 00 1 FAKE\\n' >&2\n\
         printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nfake\\n-----END PGP SIGNATURE-----\\n'\n",
    );
    git(&checkout, &["config", "commit.gpgsign", "true"]);
    git(&checkout, &["config", "gpg.format", "openpgp"]);
    git(
        &checkout,
        &["config", "gpg.program", signer.to_str().unwrap()],
    );

    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"].clone();
    let commit = git(
        &checkout,
        &["cat-file", "commit", first["commit"].as_str().unwrap()],
    );
    assert!(
        commit.contains("gpgsig -----BEGIN PGP SIGNATURE-----"),
        "{commit}"
    );

    // A failing signer refuses before the ref moves, whatever it prints: with SSH signing, Git
    // reports only the signer's own words.
    script("echo 'no secret key' >&2\nexit 1\n");
    let key = tools.path().join("key");
    fs::write(&key, "").unwrap();
    git(&checkout, &["config", "gpg.format", "ssh"]);
    git(
        &checkout,
        &["config", "gpg.ssh.program", signer.to_str().unwrap()],
    );
    git(
        &checkout,
        &["config", "user.signingkey", key.to_str().unwrap()],
    );
    checkout.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    let before = checkout_state(&checkout);
    let error = checkout.refused(&["retain", "--doing", "move the bank"], "SIGNING_FAILED");
    assert_eq!(error["details"]["stderr"], "error: no secret key");
    let tip = git(&checkout, &["rev-parse", "refs/boundrail/history"]);
    assert_eq!(tip.trim(), first["commit"]);
    assert_eq!(checkout_state(&checkout), before);
}

/// A fresh operation ID for a proposal write.
fn operation() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("write-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The revision digest of the proposal `id`.
fn proposal_digest(checkout: &Checkout, id: &str) -> String {
    let read = checkout.ok(&["read", "--proposal", id]);
    read["revision"]["digest"].as_str().unwrap().to_owned()
}

/// `model propose` named `name` by tester: the new proposal's local ID.
fn propose(checkout: &Checkout, name: &str, from: &str) -> String {
    let out = checkout.ok(&[
        "propose",
        "--name",
        name,
        "--from",
        from,
        "--doing",
        "draw a plan",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ]);
    assert_eq!(out["kind"], "propose");
    local(out["allocated"][0]["id"].as_str().unwrap()).to_owned()
}

/// An edit request by `writer` inside the proposal `id`, against its revision, naming `claim`.
fn proposal_request(checkout: &Checkout, id: &str, writer: &str, changes: Value) -> Value {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let read = checkout.ok(&["read", "--proposal", id]);
    json!({
        "target": {"model": read["manifest"]["id"], "state": id},
        "expected_revision": read["revision"]["digest"],
        "operation_id": format!("proposal-op-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        "writer": writer,
        "changes": changes,
    })
}

fn edit_proposal(checkout: &Checkout, id: &str, request: &Value) -> (i32, Value) {
    checkout.pipe(
        &["edit", "--doing", "plan ahead", "--proposal", id],
        request,
    )
}

/// Applies `changes` inside the proposal `id`: the receipt.
fn edited_proposal(checkout: &Checkout, id: &str, changes: Value) -> Value {
    let request = proposal_request(checkout, id, "tester", changes);
    let (code, out) = edit_proposal(checkout, id, &request);
    assert_eq!(code, 0, "{out:#}");
    out
}

/// `model adopt` of the proposal `id` at its present revision, by tester.
fn adopt(checkout: &Checkout, id: &str, expected: &Value) -> (i32, Value) {
    checkout.run(&[
        "adopt",
        id,
        "--expected-revision",
        expected.as_str().unwrap(),
        "--expected-proposal-revision",
        &proposal_digest(checkout, id),
        "--doing",
        "adopt the plan",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ])
}

fn names(read: &Value) -> Vec<String> {
    let mut names: Vec<String> = read["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// The shop's proposal Next: Billing renamed Payments, Bank removed, Ledger added.
fn next(checkout: &Checkout) -> (String, String) {
    let id = propose(checkout, "Next", "current");
    let out = edited_proposal(
        checkout,
        &id,
        json!([
            {"type": "edit-module", "subject": "Billing", "set": {"name": "Payments"}},
            {"type": "remove-module", "subject": "Bank"},
            {"type": "add-module", "module": {"name": "Ledger", "kind": "datastore", "order": 2}},
        ]),
    );
    let ledger = local(out["allocated"][0]["id"].as_str().unwrap()).to_owned();
    (id, ledger)
}

#[test]
fn a_proposal_is_drawn_edited_and_compared_apart_from_the_current_state() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let before = checkout.ok(&["read"]);
    let id = propose(&checkout, "Next", "current");
    let folder = checkout.vault().join("proposals").join(&id);
    let file: Value =
        serde_json::from_str(&fs::read_to_string(folder.join("proposal.json")).unwrap()).unwrap();
    let (_, schema) = checkout.run(&["schema", "model-proposal"]);
    assert!(
        jsonschema::validator_for(&schema).unwrap().is_valid(&file),
        "{file:#}"
    );
    assert_eq!(file["status"], "draft");
    assert_eq!(
        file["predecessor"]["state"],
        before["manifest"]["current"]["id"]
    );
    assert_eq!(
        file["predecessor"]["revision"],
        before["revision"]["digest"]
    );
    assert_eq!(fs::read_dir(folder.join("modules")).unwrap().count(), 4);
    let drawn = checkout.ok(&["read", "--proposal", &id]);
    assert_eq!(drawn["proposal"], file);
    assert_eq!(drawn["modules"], before["modules"]);
    assert_ne!(drawn["revision"], before["revision"]);
    assert_eq!(checkout.ok(&["read"])["revision"], before["revision"]);

    // A current-state edit written before the proposal changed still applies afterwards.
    let current =
        checkout.request(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 5}}]));
    let out = edited_proposal(
        &checkout,
        &id,
        json!([
            {"type": "edit-module", "subject": "Billing", "set": {"name": "Payments"}},
            {"type": "remove-module", "subject": "Bank"},
            {"type": "add-module", "module": {"name": "Ledger", "kind": "datastore", "order": 2}},
        ]),
    );
    assert_eq!(
        out["subjects"]["removed"],
        json!([q("BankApi1"), q("RelBil01")])
    );
    let ledger = local(out["allocated"][0]["id"].as_str().unwrap()).to_owned();
    // What the proposal removes it keeps aside, so its edits never allocate those IDs again.
    assert_eq!(
        checkout.ok(&["read", "--proposal", &id])["proposal"]["dropped"],
        json!(["BankApi1", "RelBil01"])
    );
    let after = checkout.ok(&["read"]);
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(names(&after), ["Bank", "Billing", "Platform", "Web"]);
    assert_eq!(
        after["manifest"]["retired_ids"],
        json!([]),
        "a proposal retires nothing"
    );
    assert_eq!(
        names(&checkout.ok(&["read", "--proposal", &id])),
        ["Ledger", "Payments", "Platform", "Web"]
    );

    let compared = checkout.ok(&["compare", &id]);
    assert_eq!(compared["against"]["id"], "current");
    assert_eq!(
        compared["modules"]["added"],
        json!([{"id": q(&ledger), "name": "Ledger"}])
    );
    assert_eq!(
        compared["modules"]["removed"],
        json!([{"id": q("BankApi1"), "name": "Bank"}])
    );
    // A module compares without its relationships, which compare on their own.
    assert_eq!(
        compared["modules"]["changed"],
        json!([{"id": q("Billing1"), "name": "Payments", "fields": ["former_names", "name"]}])
    );
    assert_eq!(compared["relationships"]["removed"][0]["id"], q("RelBil01"));
    let (code, text) = checkout.run_raw(&["--format", "markdown", "compare", &id]);
    assert_eq!(code, 0);
    assert!(
        text.contains(&format!("- added module Ledger {}", q(&ledger))),
        "{text}"
    );
    assert!(text.contains("- changed module Payments"), "{text}");

    let (code, out) = checkout.edit(&current);
    assert_eq!(code, 0, "{out:#}");
    let listed = checkout.ok(&["proposals"]);
    assert_eq!(listed["proposals"][0]["id"], id.as_str());
    assert_eq!(listed["proposals"][0]["status"], "draft");
}

#[test]
fn adopting_a_proposal_replaces_the_current_state_and_retires_what_it_drops() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let before = checkout.ok(&["read"]);
    let (id, ledger) = next(&checkout);
    let drawn = checkout.ok(&["read", "--proposal", &id]);
    let expected = before["revision"]["digest"].as_str().unwrap();
    let once = [
        "adopt",
        id.as_str(),
        "--expected-revision",
        expected,
        "--expected-proposal-revision",
        drawn["revision"]["digest"].as_str().unwrap(),
        "--doing",
        "adopt the plan",
        "--writer",
        "tester",
        "--operation-id",
        "adopt-once",
    ];
    let out = checkout.ok(&once);
    assert_eq!(out["kind"], "adopt");
    let state = local(out["allocated"][0]["id"].as_str().unwrap()).to_owned();

    let after = checkout.ok(&["read"]);
    assert_eq!(out["revision"], after["revision"]);
    assert_eq!(after["modules"], drawn["modules"]);
    assert_eq!(
        after["manifest"]["current"],
        json!({"id": state, "name": "Next"})
    );
    let old = before["manifest"]["current"]["id"].as_str().unwrap();
    assert_ne!(old, state);
    let retired = after["manifest"]["retired_ids"].as_array().unwrap();
    for gone in ["BankApi1", "RelBil01", old] {
        assert!(
            retired.contains(&json!(gone)),
            "{gone} not retired: {retired:?}"
        );
    }
    assert!(
        !checkout
            .vault()
            .join("modules/BankApi1.module.json")
            .exists()
    );
    assert!(
        checkout
            .vault()
            .join(format!("modules/{ledger}.module.json"))
            .exists()
    );
    let adopted = checkout.ok(&["read", "--proposal", &id])["proposal"].clone();
    assert_eq!(adopted["status"], "adopted");
    assert_eq!(adopted["adopted_as"], state.as_str());
    assert!(
        out["subjects"]["changed"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("{model}#{id}")))
    );

    // An adopted proposal reads only; the new current state takes edits at its own revision.
    let error = edit_proposal(
        &checkout,
        &id,
        &proposal_request(
            &checkout,
            &id,
            "tester",
            json!([
                {"type": "edit-module", "subject": "Web", "set": {"order": 9}},
            ]),
        ),
    );
    assert_eq!(error.0, 2, "{:#}", error.1);
    assert_eq!(error.1["error"]["code"], "PROPOSAL_CLOSED");
    checkout.edited(json!([{"type": "edit-module", "subject": "Ledger", "set": {"order": 3}}]));
    // Repeating the operation returns its receipt and writes nothing; a new one refuses.
    let digest = checkout.digest();
    let replayed = checkout.ok(&once);
    assert_eq!(replayed["allocated"], out["allocated"]);
    assert_eq!(replayed["after"], out["after"]);
    assert_eq!(checkout.digest(), digest);
    let (code, again) = adopt(&checkout, &id, &digest);
    assert_eq!(code, 2);
    assert_eq!(again["error"]["code"], "PROPOSAL_CLOSED");
}

#[test]
fn adoption_retires_what_the_proposal_dropped() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let out = edited_proposal(
        &checkout,
        &id,
        json!([{"type": "add-module", "module": {"name": "Ledger", "kind": "datastore", "order": 2}}]),
    );
    let ledger = local(out["allocated"][0]["id"].as_str().unwrap()).to_owned();
    // Ledger never reached the current state, so only the dropped list keeps its ID reserved.
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "remove-module", "subject": "Ledger"}]),
    );
    let dropped = checkout.ok(&["read", "--proposal", &id])["proposal"]["dropped"].clone();
    assert_eq!(dropped, json!([ledger]));
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    let retired = checkout.ok(&["read"])["manifest"]["retired_ids"].clone();
    let retired = retired.as_array().unwrap();
    assert!(retired.contains(&json!(ledger)), "{retired:?}");
    assert_eq!(retired.iter().filter(|r| **r == json!(ledger)).count(), 1);
}

#[test]
fn a_dropped_id_must_be_well_formed() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let file = format!("proposals/{id}/proposal.json");
    let mut proposal: Value =
        serde_json::from_str(&fs::read_to_string(checkout.vault().join(&file)).unwrap()).unwrap();
    proposal["dropped"] = json!(["bad"]);
    checkout.write(&file, &serde_json::to_string_pretty(&proposal).unwrap());
    let error = checkout.refused(&["read", "--proposal", &id], "MODEL_INVALID");
    let problem = &error["details"]["problems"][0];
    assert_eq!(problem["file"], file.as_str());
    assert_eq!(problem["subject"], "bad");
    assert_eq!(problem["message"], "dropped ID is not 8 base62 characters");
}

#[test]
fn adopting_after_the_current_state_moved_refuses_predecessor_moved() {
    let checkout = Checkout::new();
    shop(&checkout);
    let drawn_from = checkout.digest();
    let (id, _) = next(&checkout);
    checkout.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 7}}]));
    let moved = checkout.digest();
    let (code, out) = adopt(&checkout, &id, &drawn_from);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "STALE_REVISION");
    let (code, out) = adopt(&checkout, &id, &moved);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "PREDECESSOR_MOVED");
    assert_eq!(
        out["error"]["details"]["predecessor"]["revision"],
        drawn_from
    );
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap()
            .contains("model reconcile")
    );
    assert_eq!(checkout.digest(), moved);
    assert_eq!(
        checkout.ok(&["read", "--proposal", &id])["proposal"]["status"],
        "draft"
    );
}

#[test]
fn a_shelved_proposal_reads_only() {
    let checkout = Checkout::new();
    shop(&checkout);
    let (id, _) = next(&checkout);
    let digest = proposal_digest(&checkout, &id);
    let shelve = |expected: &str, operation: &str| {
        checkout.run(&[
            "shelve",
            &id,
            "--expected-revision",
            expected,
            "--doing",
            "not now",
            "--writer",
            "tester",
            "--operation-id",
            operation,
        ])
    };
    // Shelving names the proposal revision it read, as an edit inside it does.
    let (code, out) = shelve(checkout.digest().as_str().unwrap(), "shelve-0");
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "STALE_REVISION");
    assert_eq!(out["error"]["details"]["current"], digest.as_str());
    let (code, out) = shelve(&digest, "shelve-1");
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "shelve");
    let read = checkout.ok(&["read", "--proposal", &id]);
    assert_eq!(read["proposal"]["status"], "shelved");
    assert_eq!(names(&read), ["Ledger", "Payments", "Platform", "Web"]);
    let request = proposal_request(
        &checkout,
        &id,
        "tester",
        json!([
            {"type": "edit-module", "subject": "Web", "set": {"order": 9}},
        ]),
    );
    let (code, out) = edit_proposal(&checkout, &id, &request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "PROPOSAL_CLOSED");
    let (code, out) = shelve(&proposal_digest(&checkout, &id), "shelve-2");
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "PROPOSAL_CLOSED");
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 2);
    assert_eq!(out["error"]["code"], "PROPOSAL_CLOSED");
}

#[test]
fn a_planning_model_works_in_proposals_until_the_first_adoption() {
    let checkout = Checkout::new();
    let out = checkout.ok(&["init", "--name", "Shop", "--planning"]);
    let model = out["manifest"]["id"].as_str().unwrap().to_owned();
    assert_eq!(out["manifest"]["current"], Value::Null);
    let read = checkout.ok(&["read"]);
    assert_eq!(read["modules"], json!([]));
    let request = json!({
        "target": {"model": model, "state": "State001"}, "expected_revision": read["revision"]["digest"],
        "operation_id": "planning-1", "writer": "tester",
        "changes": [{"type": "add-module", "module": {"name": "Web", "kind": "app", "order": 0}}],
    });
    checkout.edit_refused(&request, "NO_CURRENT_STATE");

    let id = propose(&checkout, "First", "current");
    let drawn = checkout.ok(&["read", "--proposal", &id]);
    assert_eq!(drawn["proposal"]["predecessor"]["state"], Value::Null);
    assert_eq!(drawn["modules"], json!([]));
    edited_proposal(
        &checkout,
        &id,
        json!([
            {"type": "add-module", "module": {"name": "Web", "kind": "app", "order": 0}},
            {"type": "add-module", "module": {"name": "Api", "kind": "service", "order": 1}},
            {"type": "add-relationship", "source": "Web", "relationship": {"to": "Api", "kind": "call", "order": 0}},
        ]),
    );
    let (_, checked) = checkout.run(&["check", "--only", "semantic", "--proposal", &id]);
    assert_eq!(checked["kind"], "check");
    assert_eq!(checked["proposal"]["id"], id.as_str());
    assert_eq!(checkout.ok(&["read"])["modules"], json!([]));

    let (code, out) = adopt(&checkout, &id, &read["revision"]["digest"]);
    assert_eq!(code, 0, "{out:#}");
    let after = checkout.ok(&["read"]);
    assert_eq!(after["manifest"]["current"]["name"], "First");
    assert_eq!(names(&after), ["Api", "Web"]);
    assert_eq!(after["relationships"].as_array().unwrap().len(), 1);
}

#[test]
fn a_claim_on_a_proposal_blocks_another_writers_edit_inside_it() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let claim = checkout.claim("alice", json!([id]));
    assert_eq!(claim["subjects"], json!([format!("{model}#{id}")]));
    let change = json!([{"type": "edit-module", "subject": "Web", "set": {"order": 4}}]);
    let request = proposal_request(&checkout, &id, "bob", change.clone());
    let (code, out) = edit_proposal(&checkout, &id, &request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "CLAIM_CONFLICT");
    // The proposal's claim holds nothing in the current state.
    let (code, out) = checkout.edit(&checkout.request_by("bob", None, change.clone()));
    assert_eq!(code, 0, "{out:#}");
    let mut request = proposal_request(&checkout, &id, "alice", change);
    request["claim"] = claim["id"].clone();
    let (code, out) = edit_proposal(&checkout, &id, &request);
    assert_eq!(code, 0, "{out:#}");
    // A claim targeting the proposal resolves names there, where alice's claim holds them.
    let request = json!({
        "target": {"model": model, "state": id}, "writer": "bob", "reason": "web work",
        "subjects": ["Web"],
    });
    let (code, out) = checkout.pipe(&["claim", "--doing", "reserve web"], &request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "CLAIM_CONFLICT");
}

#[cfg(unix)]
#[test]
fn proposal_files_are_validated_like_the_current_states() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let folder = format!("proposals/{id}");
    let mut bank = module("BankApi1", "Bank", "external", 1);
    bank["parent"] = "Elsewhr1#Nowhere1".into();
    checkout.write(
        &format!("{folder}/modules/BankApi1.module.json"),
        &serde_json::to_string_pretty(&bank).unwrap(),
    );
    let error = checkout.refused(&["read", "--proposal", &id], "MODEL_INVALID");
    assert_eq!(
        error["details"]["problems"][0]["file"],
        format!("{folder}/modules/BankApi1.module.json")
    );
    // The current state keeps loading: each target validates its own files.
    checkout.ok(&["read"]);
    let modules = checkout.vault().join(&folder).join("modules");
    fs::remove_file(modules.join("BankApi1.module.json")).unwrap();
    std::os::unix::fs::symlink(
        checkout.vault().join("modules/BankApi1.module.json"),
        modules.join("BankApi1.module.json"),
    )
    .unwrap();
    let error = checkout.refused(&["read", "--proposal", &id], "MODEL_INVALID");
    assert_eq!(
        error["details"]["problems"][0]["message"],
        "is a symbolic link"
    );
    checkout.refused(&["read", "--proposal", "Missing1"], "SUBJECT_NOT_FOUND");
}

#[test]
fn a_manifest_needs_its_current_key_and_null_reads_as_a_planning_model() {
    let checkout = Checkout::new();
    checkout.init();
    let path = checkout.vault().join("model.json");
    let mut manifest: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("current");
    checkout.write(
        "model.json",
        &serde_json::to_string_pretty(&manifest).unwrap(),
    );
    let error = checkout.refused(&["read"], "MODEL_INVALID");
    let problem = &error["details"]["problems"][0];
    assert_eq!(problem["file"], "model.json");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("missing field `current`"),
        "{problem}"
    );
    manifest["current"] = Value::Null;
    checkout.write(
        "model.json",
        &serde_json::to_string_pretty(&manifest).unwrap(),
    );
    let read = checkout.ok(&["read"]);
    assert_eq!(read["manifest"]["current"], Value::Null);
    let request = json!({
        "target": {"model": manifest["id"], "state": "State001"}, "expected_revision": read["revision"]["digest"],
        "operation_id": "planning-1", "writer": "tester",
        "changes": [{"type": "add-module", "module": {"name": "Web", "kind": "app", "order": 0}}],
    });
    checkout.edit_refused(&request, "NO_CURRENT_STATE");
}

#[test]
fn adopting_a_proposal_that_holds_a_retired_id_refuses() {
    let checkout = Checkout::new();
    shop(&checkout);
    checkout.edited(json!([{"type": "remove-module", "subject": "Bank"}]));
    let id = propose(&checkout, "Next", "current");
    // IDs are random and checked only against the state being edited, the retired list and the
    // proposal folders; a proposal holding a retired ID loads, but its adoption refuses.
    checkout.write(
        &format!("proposals/{id}/modules/BankApi1.module.json"),
        &serde_json::to_string_pretty(&module("BankApi1", "Bank", "external", 1)).unwrap(),
    );
    checkout.ok(&["read", "--proposal", &id]);
    let digest = checkout.digest();
    let (code, out) = adopt(&checkout, &id, &digest);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "MODEL_INVALID");
    let problems = out["error"]["details"]["problems"].as_array().unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p["subject"] == "BankApi1" && p["message"] == "ID is retired"),
        "{problems:#?}"
    );
    assert_eq!(checkout.digest(), digest);
}

#[test]
fn a_copy_of_a_proposal_keeps_its_predecessor() {
    let checkout = Checkout::new();
    checkout.init();
    checkout.edited(
        json!([{"type": "add-module", "module": {"name": "Web", "kind": "app", "order": 0}}]),
    );
    let first = propose(&checkout, "First", "current");
    checkout.edited(json!([{"type": "add-module", "module": {"name": "Billing", "kind": "service", "order": 1}}]));
    let copy = propose(&checkout, "Copy", &first);
    let drawn =
        |id: &str| checkout.ok(&["read", "--proposal", id])["proposal"]["predecessor"].clone();
    assert_eq!(drawn(&copy), drawn(&first));
    // Adopting the copy would drop Billing, which its source never saw.
    let (code, out) = adopt(&checkout, &copy, &checkout.digest());
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "PREDECESSOR_MOVED");

    // An adopted proposal copies only while it still is the current state, unchanged.
    let next = propose(&checkout, "Next", "current");
    let (code, out) = adopt(&checkout, &next, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    let again = propose(&checkout, "Again", &next);
    let fresh = propose(&checkout, "Fresh", "current");
    assert_eq!(drawn(&again), drawn(&fresh));
    checkout.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 4}}]));
    checkout.refused(
        &[
            "propose",
            "--name",
            "Late",
            "--from",
            &next,
            "--doing",
            "draw",
            "--writer",
            "tester",
            "--operation-id",
            "late",
        ],
        "PREDECESSOR_MOVED",
    );
}

#[test]
fn adopting_respects_claims_on_what_it_changes() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "remove-module", "subject": "Billing"}]),
    );
    checkout.claim("bob", json!(["Billing"]));
    let digest = checkout.digest();
    let (code, out) = adopt(&checkout, &id, &digest);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "CLAIM_CONFLICT");
    assert_eq!(out["error"]["details"]["conflicts"][0]["writer"], "bob");
    assert_eq!(checkout.digest(), digest);
}

#[test]
fn claims_never_overlap_across_targets() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    checkout.claim("bob", json!(["Web"]));
    let error = checkout.claim_refused("alice", json!([id]), "CLAIM_CONFLICT");
    assert_eq!(error["details"]["conflicts"][0]["writer"], "bob");
    // Likewise the other way round: a held proposal holds what it contains.
    let other = Checkout::new();
    shop(&other);
    let id = propose(&other, "Other", "current");
    other.claim("alice", json!([id]));
    other.claim_refused("dave", json!(["Billing"]), "CLAIM_CONFLICT");
}

#[test]
fn claims_clash_by_the_containment_of_every_draft() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Nested", "current");
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "edit-module", "subject": "Web", "set": {"parent": "Billing"}}]),
    );
    // Nobody claims the draft, yet inside it Billing holds Web.
    checkout.claim("alice", json!(["Web"]));
    let error = checkout.claim_refused("bob", json!(["Billing"]), "CLAIM_CONFLICT");
    assert_eq!(error["details"]["conflicts"][0]["writer"], "alice");
}

#[test]
fn a_claim_on_an_adopted_proposal_blocks_no_other_claim() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    // The adopted proposal reads only, so what it holds is no longer contained by it.
    checkout.claim("alice", json!([id]));
    checkout.claim("bob", json!(["Web"]));
}

#[cfg(unix)]
#[test]
fn a_write_through_a_linked_folder_refuses_before_it_is_journaled() {
    let checkout = Checkout::new();
    shop(&checkout);
    let elsewhere = checkout.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, checkout.vault().join("proposals")).unwrap();
    let error = checkout.refused(
        &[
            "propose",
            "--name",
            "Next",
            "--doing",
            "draw",
            "--writer",
            "tester",
            "--operation-id",
            "draw-1",
        ],
        "MODEL_INVALID",
    );
    assert_eq!(error["details"]["path"], "proposals");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    let pending = checkout.path().join(".git/boundrail/operations/pending");
    assert!(!pending.exists() || fs::read_dir(&pending).unwrap().count() == 0);
    // Current-state edits list the proposal folders too, so they refuse until the link goes,
    // and nothing waits in the journal afterwards.
    let request =
        checkout.request(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
    let (code, out) = checkout.edit(&request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["details"]["path"], "proposals");
    fs::remove_file(checkout.vault().join("proposals")).unwrap();
    checkout.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
}

#[test]
fn proposals_list_each_folder_even_when_one_does_not_read() {
    let checkout = Checkout::new();
    shop(&checkout);
    let good = propose(&checkout, "Good", "current");
    let bad = propose(&checkout, "Bad", "current");
    checkout.write(&format!("proposals/{bad}/proposal.json"), "{");
    let listed = checkout.ok(&["proposals"])["proposals"].clone();
    let entry = |id: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()
            .clone()
    };
    let good = entry(&good);
    assert_eq!(good["name"], "Good");
    assert_eq!(good["status"], "draft");
    assert_eq!(good["predecessor"]["revision"], checkout.digest());
    assert!(
        entry(&bad)["error"].as_str().unwrap().contains("parsing"),
        "{listed:#}"
    );
}

#[test]
fn adopt_checks_the_compared_proposal_revision() {
    let checkout = Checkout::new();
    shop(&checkout);
    let (id, _) = next(&checkout);
    let compared = checkout.ok(&["read", "--proposal", &id])["revision"]["digest"].clone();
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "edit-module", "subject": "Web", "set": {"order": 6}}]),
    );
    let digest = checkout.digest();
    let with = |revision: &Value| {
        checkout.run(&[
            "adopt",
            &id,
            "--expected-revision",
            digest.as_str().unwrap(),
            "--expected-proposal-revision",
            revision.as_str().unwrap(),
            "--doing",
            "adopt the plan",
            "--writer",
            "tester",
            "--operation-id",
            &operation(),
        ])
    };
    let (code, out) = with(&compared);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "STALE_REVISION");
    let now = checkout.ok(&["read", "--proposal", &id])["revision"]["digest"].clone();
    assert_eq!(out["error"]["details"]["current"], now);
    let (code, out) = with(&now);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["revision"], checkout.ok(&["read"])["revision"]);
}

#[test]
fn an_interrupted_adoption_is_one_journaled_write_that_recovery_finishes() {
    let checkout = Checkout::new();
    shop(&checkout);
    let expected = checkout.digest();
    let (id, _) = next(&checkout);
    let compared = proposal_digest(&checkout, &id);
    let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model", "adopt", &id, "--expected-revision"])
        .arg(expected.as_str().unwrap())
        .args(["--expected-proposal-revision", &compared])
        .args([
            "--doing",
            "adopt the plan",
            "--writer",
            "tester",
            "--operation-id",
            "adopt-1",
        ])
        .env(STOP.0, STOP.1)
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let pending: Value = serde_json::from_str(
        &fs::read_to_string(
            checkout
                .path()
                .join(".git/boundrail/operations/pending/adopt-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(pending["kind"], "adopt");
    assert_eq!(pending["scope"], json!({"adoption": id}));
    let paths: Vec<&str> = (pending["writes"].as_array().unwrap().iter())
        .map(|w| w["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"model.json")
            && paths.contains(&format!("proposals/{id}/proposal.json").as_str()),
        "{paths:?}"
    );
    // Nothing landed, and other writes wait for the recovery.
    assert_eq!(checkout.digest(), expected);
    checkout.refused(
        &[
            "propose",
            "--name",
            "Other",
            "--doing",
            "draw",
            "--writer",
            "tester",
            "--operation-id",
            "other",
        ],
        "RECOVERY_REQUIRED",
    );
    let recovered = checkout.ok(&["recover"]);
    assert_eq!(recovered["completed"][0]["operation_id"], "adopt-1");
    assert_eq!(recovered["completed"][0]["kind"], "adopt");
    assert_eq!(recovered["completed"][0]["scope"], json!({"adoption": id}));
    assert_eq!(
        names(&checkout.ok(&["read"])),
        ["Ledger", "Payments", "Platform", "Web"]
    );
    assert_eq!(
        checkout.ok(&["read", "--proposal", &id])["proposal"]["status"],
        "adopted"
    );
}

/// Asserts `svg` is well-formed XML as far as a render can break it: every tag closes in order,
/// quotes pair inside tags, and every `&` starts an entity. The crate has no XML parser.
fn assert_xml(svg: &str) {
    let entities = |text: &str| {
        for (at, _) in text.match_indices('&') {
            let tail = &text[at..];
            let known = ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"];
            assert!(
                known.iter().any(|e| tail.starts_with(e)),
                "bare & in {text}"
            );
        }
    };
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "{svg}"
    );
    let mut open: Vec<&str> = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        entities(&rest[..start]);
        let end = start + rest[start..].find('>').expect("a tag closes");
        let tag = &rest[start + 1..end];
        assert!(
            !tag.contains('<') && tag.matches('"').count().is_multiple_of(2),
            "{tag}"
        );
        entities(tag);
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(open.pop(), Some(name), "closing {name}");
        } else if !tag.ends_with('/') {
            open.push(tag.split_whitespace().next().unwrap());
        }
        rest = &rest[end + 1..];
    }
    assert!(open.is_empty(), "unclosed {open:?}");
    assert_eq!(rest.trim(), "");
}

impl Checkout {
    /// `model render` with `args`: the SVG it prints, checked to be well-formed.
    fn render(&self, args: &[&str]) -> String {
        let mut all = vec!["render"];
        all.extend(args);
        let (code, svg) = self.run_raw(&all);
        assert_eq!(code, 0, "{svg}");
        assert_xml(&svg);
        svg
    }
}

/// The first `<g>` element with class `class` that holds `text`, up to its first `</g>`.
fn group_holding<'a>(svg: &'a str, class: &str, text: &str) -> Option<&'a str> {
    let opening = format!("<g class=\"{class}\"");
    svg.match_indices(&opening).find_map(|(at, _)| {
        let body = &svg[at..at + svg[at..].find("</g>")?];
        body.contains(text).then_some(body)
    })
}

#[test]
fn render_draws_every_module_and_an_arrow_per_relationship() {
    let checkout = Checkout::new();
    shop(&checkout);
    let svg = checkout.render(&[]);
    for name in ["Platform", "Web", "Billing", "Bank"] {
        assert!(svg.contains(&format!(">{name}</text>")), "{name}: {svg}");
    }
    assert_eq!(svg.matches("<g class=\"relationship\"").count(), 2, "{svg}");
    assert!(svg.contains(">charges</text>") && svg.contains(">Call</text>"));
    // Bank is external: a dashed border.
    let bank = group_holding(&svg, "module", ">Bank<").unwrap();
    assert!(
        bank.contains("BankApi1") && bank.contains("stroke-dasharray=\"5 3\""),
        "{bank}"
    );
    assert!(svg.contains(r#" viewBox="0 0 "#) && svg.contains("font-family=\"sans-serif\""));
    // The same input draws the same bytes.
    assert_eq!(checkout.render(&[]), svg);

    // Depth 1 collapses Platform's parts; their relationship to Bank lifts to Platform.
    let collapsed = checkout.render(&["--depth", "1"]);
    assert!(!collapsed.contains(">Web</text>"));
    assert!(collapsed.contains("[System] +2 parts"), "{collapsed}");
    assert_eq!(collapsed.matches("<g class=\"relationship\"").count(), 1);

    // A scope draws its parts; relationships leaving it are left out.
    let scoped = checkout.render(&["--scope", "Platform"]);
    assert!(scoped.contains(">Web</text>") && !scoped.contains(">Bank</text>"));
    assert_eq!(scoped.matches("<g class=\"relationship\"").count(), 1);
    checkout.refused(&["render", "--scope", "Nowhere"], "SUBJECT_NOT_FOUND");
}

#[test]
fn render_a_sequence_with_its_decision_and_loop_frames() {
    let checkout = Checkout::new();
    shop(&checkout);
    add_checkout(&checkout);
    let svg = checkout.render(&["--flow", "Checkout"]);
    assert_eq!(svg.matches("<g class=\"lifeline\"").count(), 3, "{svg}");
    for text in [
        ">alt</text>",
        ">card valid?</text>",
        ">[yes]</text>",
        ">[no]</text>",
        ">loop</text>",
        ">[each item]</text>",
        ">charge x3</text>",
        ">place order</text>",
    ] {
        assert!(svg.contains(text), "{text}: {svg}");
    }
    assert!(svg.contains("<g class=\"frame alt\"") && svg.contains("<g class=\"frame loop\""));
    // A reply is dashed.
    let decline = group_holding(&svg, "message", ">decline<").unwrap();
    assert!(decline.contains("stroke-dasharray=\"6 4\""), "{decline}");
    assert_eq!(
        checkout.render(&["--flow", "Checkout", "--view", "sequence"]),
        svg
    );
}

#[test]
fn render_a_flowchart_of_a_flow() {
    let checkout = Checkout::new();
    shop(&checkout);
    add_checkout(&checkout);
    let svg = checkout.render(&["--flow", "Checkout", "--view", "flowchart"]);
    for text in [
        ">Web -&gt; Billing: place order</text>",
        ">Billing -&gt; Bank: charge x3</text>",
        ">card valid?</text>",
        ">yes</text>",
        ">no</text>",
        ">loop: each item</text>",
        ">repeat</text>",
    ] {
        assert!(svg.contains(text), "{text}: {svg}");
    }
    assert_eq!(svg.matches("<g class=\"step message\"").count(), 4);
    assert_eq!(svg.matches("<g class=\"step decision\"").count(), 1);
    // The branches rejoin at one point.
    assert_eq!(svg.matches("<circle").count(), 1);
}

#[test]
fn render_marks_what_a_proposal_changes() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let ids = add_checkout(&checkout);
    let id = propose(&checkout, "Fraud", "current");
    edited_proposal(
        &checkout,
        &id,
        json!([
            {"type": "add-module", "module": {"name": "Fraud", "kind": "service", "order": 2}},
            {"type": "add-relationship", "source": "Billing", "relationship": {"to": "Fraud", "kind": "rpc", "order": 1, "label": "score"}},
            {"type": "edit-module", "subject": "Billing", "set": {"name": "Payments"}},
            {"type": "remove-step", "subject": ids[7]},
        ]),
    );
    let svg = checkout.render(&["--proposal", &id, "--against", "current"]);
    let fraud = group_holding(&svg, "module added", ">Fraud<").unwrap();
    assert!(fraud.contains("stroke=\"#2f9e44\""), "{fraud}");
    assert!(
        group_holding(&svg, "module changed", ">Payments<").is_some(),
        "{svg}"
    );
    assert!(group_holding(&svg, "relationship added", ">score<").is_some());
    let legend = group_holding(&svg, "legend", ">added<").unwrap();
    assert!(
        legend.contains(">removed<") && legend.contains(">changed<"),
        "{legend}"
    );
    assert!(svg.contains(">Shop, proposal Fraud, compared with the current state</text>"));
    // A proposal alone, or named by --against, is drawn against the current state the same way.
    assert_eq!(checkout.render(&["--proposal", &id]), svg);
    assert_eq!(checkout.render(&["--against", &id]), svg);
    // Unmarked when nothing is compared.
    let plain = checkout.render(&[]);
    assert!(!plain.contains("class=\"legend\"") && !plain.contains(" added\""));
    checkout.refused(&["render", "--against", "current"], "REQUEST_INVALID");
    checkout.refused(
        &["render", "--proposal", &id, "--against", &id],
        "REQUEST_INVALID",
    );
    // The local and qualified IDs name the same proposal.
    let qualified = format!("{model}#{id}");
    for (drawn, against) in [(id.as_str(), qualified.as_str()), (&qualified, &id)] {
        checkout.refused(
            &["render", "--proposal", drawn, "--against", against],
            "REQUEST_INVALID",
        );
    }

    // Of two proposals, --proposal is drawn against --against: what the first lacks is removed,
    // faded and struck.
    let copy = propose(&checkout, "Plain", "current");
    let back = checkout.render(&["--proposal", &copy, "--against", &id]);
    assert!(back.contains(">Shop, proposal Plain, compared with proposal Fraud</text>"));
    let gone = group_holding(&back, "module removed", ">Fraud<").unwrap();
    assert!(gone.contains("text-decoration=\"line-through\""), "{gone}");
    assert!(gone.contains("stroke=\"#e03131\"") && gone.contains("opacity=\"0.4\""));
    assert!(group_holding(&back, "relationship removed", ">score<").is_some());

    // In a flow, the removed step is drawn where it stood.
    let sequence = checkout.render(&[
        "--proposal",
        &id,
        "--against",
        "current",
        "--flow",
        "Checkout",
    ]);
    assert!(
        group_holding(&sequence, "message removed", ">decline<").is_some(),
        "{sequence}"
    );
    assert!(group_holding(&sequence, "lifeline changed", ">Payments<").is_some());
    let flowchart = checkout.render(&[
        "--proposal",
        &id,
        "--against",
        "current",
        "--flow",
        "Checkout",
        "--view",
        "flowchart",
    ]);
    assert!(group_holding(&flowchart, "step message removed", "decline<").is_some());
    assert!(flowchart.contains("class=\"legend\""));
}

#[test]
fn render_draws_the_current_state_against_an_older_checkpoint() {
    let checkout = committed();
    shop(&checkout);
    let first = checkout.ok(&["retain", "--doing", "the shop as built"])["checkpoint"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 3}},
    ]));
    let svg = checkout.render(&["--checkpoint", &first, "--against", "current"]);
    let title = format!(">Shop, the current state, compared with checkpoint {first}</text>");
    assert!(svg.contains(&title), "{svg}");
    assert!(
        group_holding(&svg, "module added", ">Ledger<").is_some(),
        "{svg}"
    );
    // The checkpoint alone is drawn unmarked.
    let old = checkout.render(&["--checkpoint", &first]);
    assert!(!old.contains(">Ledger<") && !old.contains("class=\"legend\""));
    assert!(old.contains(&format!(">Shop, checkpoint {first}</text>")));
}

#[test]
fn render_writes_an_output_file_or_json() {
    let checkout = Checkout::new();
    shop(&checkout);
    let svg = checkout.render(&[]);
    let (code, stdout) = checkout.run_raw(&["render", "--output", "out/model.svg"]);
    assert_eq!(code, 2, "a missing folder refuses: {stdout}");
    assert!(stdout.contains("REQUEST_INVALID"));
    fs::create_dir(checkout.path().join("out")).unwrap();
    let (code, stdout) = checkout.run_raw(&["render", "--output", "out/model.svg"]);
    assert_eq!((code, stdout.as_str()), (0, ""));
    let written = fs::read_to_string(checkout.path().join("out/model.svg")).unwrap();
    assert_eq!(written, svg);
    let error = checkout.refused(
        &["render", "--output", ".boundrail/model/x.svg"],
        "REQUEST_INVALID",
    );
    assert!(
        error["message"].as_str().unwrap().contains("vault"),
        "{error}"
    );
    assert!(!checkout.vault().join("x.svg").exists());

    let out = checkout.ok(&["render", "--format", "json"]);
    assert_eq!(out["kind"], "render");
    assert_eq!(out["view"], "architecture");
    assert_eq!(out["svg"], svg);
}

#[test]
fn render_refuses_flags_that_do_not_go_together() {
    let checkout = Checkout::new();
    shop(&checkout);
    add_checkout(&checkout);
    for args in [
        vec!["render", "--view", "sequence"],
        vec!["render", "--view", "flowchart"],
        vec!["render", "--view", "architecture", "--flow", "Checkout"],
        vec!["render", "--flow", "Checkout", "--scope", "Platform"],
        vec!["render", "--flow", "Checkout", "--depth", "3"],
        vec!["render", "--depth", "0"],
        vec!["render", "--proposal", "Abcdefgh", "--checkpoint", "abcd"],
    ] {
        checkout.refused(&args, "REQUEST_INVALID");
    }
    // Render has no Markdown form; the refusal itself prints as Markdown.
    let (code, stdout) = checkout.run_raw(&["render", "--format", "markdown"]);
    assert_eq!(code, 2);
    assert!(stdout.starts_with("REQUEST_INVALID: "), "{stdout}");
    checkout.refused(&["render", "--flow", "Nowhere"], "SUBJECT_NOT_FOUND");
}

/// Writes `files` into the checkout.
fn sources(checkout: &Checkout, files: &[(&str, &str)]) {
    for (path, text) in files {
        let file = checkout.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
}

/// A committed TypeScript checkout on `main` with modules Web, Billing and Bank bound to their
/// folders and Docs unbound. Web imports Billing's charge through its index and its fee past
/// it; Billing's charge calls Bank. Billing's index also exports `refund`, which nobody imports.
fn measured() -> (Checkout, String) {
    let checkout = committed();
    git(&checkout, &["branch", "-M", "main"]);
    sources(
        &checkout,
        &[
            ("package.json", r#"{"name": "fixture"}"#),
            (
                "src/web/index.ts",
                "import { charge } from \"../billing\";\nimport { fee } from \"../billing/fee\";\nexport const page = () => charge(1) + fee;\n",
            ),
            (
                "src/billing/index.ts",
                "export { charge } from \"./charge\";\nexport const refund = 2;\n",
            ),
            (
                "src/billing/charge.ts",
                "import { pay } from \"../bank\";\nexport const charge = (n: number) => pay(n);\n",
            ),
            ("src/billing/fee.ts", "export const fee = 1;\n"),
            (
                "src/bank/index.ts",
                "export const pay = (n: number) => n;\n",
            ),
        ],
    );
    let model = checkout.init();
    for (id, name, path, order) in [
        ("WebApp01", "Web", "src/web", 0),
        ("Billing1", "Billing", "src/billing", 1),
        ("BankApi1", "Bank", "src/bank", 2),
    ] {
        let mut value = module(id, name, "module", order);
        value["binding"] = json!({"path": path});
        checkout.module(value);
    }
    checkout.module(module("Docs0001", "Docs", "module", 3));
    let mut web = checkout.read_module("WebApp01");
    web["outgoing"] =
        json!([{"id": "RelWeb01", "to": format!("{model}#Billing1"), "kind": "call", "order": 0}]);
    checkout.module(web);
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "shop"]);
    (checkout, model)
}

/// A committed checkout of `files` with Web and Billing bound to `web` and `billing`, and
/// `model inspect`'s Billing entry, so the live scan of that language runs end to end.
fn inspect_billing(files: &[(&str, &str)], web: &str, billing: &str) -> Value {
    let checkout = committed();
    sources(&checkout, files);
    checkout.init();
    for (id, name, path, order) in [
        ("WebApp01", "Web", web, 0),
        ("Billing1", "Billing", billing, 1),
    ] {
        let mut value = module(id, name, "module", order);
        value["binding"] = json!({"path": path});
        checkout.module(value);
    }
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "shop"]);
    let out = checkout.ok(&["inspect", "Billing"]);
    assert_eq!(out["skips"], json!([]), "{out:#}");
    out["modules"][0].clone()
}

#[test]
fn inspect_scans_a_python_package_under_src() {
    let billing = inspect_billing(
        &[
            ("pyproject.toml", "[project]\nname = \"shop\"\n"),
            ("src/shop/__init__.py", ""),
            (
                "src/shop/web/__init__.py",
                "from shop.billing.charge import charge\n",
            ),
            ("src/shop/billing/__init__.py", ""),
            (
                "src/shop/billing/charge.py",
                "def charge(n):\n    return n\n",
            ),
        ],
        "src/shop/web",
        "src/shop/billing",
    );
    assert_eq!(billing["files"], 2, "{billing:#}");
    assert_eq!(billing["incoming"][0]["edges"], 1, "{billing:#}");
}

#[test]
fn inspect_scans_java_packages() {
    let billing = inspect_billing(
        &[
            ("pom.xml", "<project></project>\n"),
            (
                "src/main/java/com/shop/web/Page.java",
                "package com.shop.web;\nimport com.shop.billing.Charge;\npublic class Page { Charge c; }\n",
            ),
            (
                "src/main/java/com/shop/billing/Charge.java",
                "package com.shop.billing;\npublic class Charge {}\n",
            ),
        ],
        "src/main/java/com/shop/web",
        "src/main/java/com/shop/billing",
    );
    assert_eq!(billing["files"], 1, "{billing:#}");
    assert_eq!(billing["incoming"][0]["edges"], 1, "{billing:#}");
}

#[test]
fn inspect_counts_entry_points_deep_imports_and_unused_exports() {
    let (checkout, model) = measured();
    let q = |id: &str| format!("{model}#{id}");
    let out = checkout.ok(&["inspect"]);
    assert_eq!(out["kind"], "inspect");
    assert_eq!(out["revision"]["digest"], checkout.digest());
    assert_eq!(
        out["skips"],
        json!([{"module": q("Docs0001"), "reason": "unbound"}])
    );
    let billing = &out["modules"][1];
    assert_eq!(billing["name"], "Billing");
    assert_eq!(billing["files"], 3);
    assert_eq!(billing["entry_points"], json!(["src/billing/index.ts"]));
    // The TypeScript scan reports incomplete symbol coverage, so `refund`, which nobody
    // imports, is withheld as a candidate, as the scan's surface withholds it.
    assert_eq!(
        billing["symbols"],
        json!({"exported": 2, "used": 1, "unused": null, "source": "scan"})
    );
    assert_eq!(
        billing["deep_imports"],
        json!([{"from": "src/web/index.ts", "to": "src/billing/fee.ts"}])
    );
    assert_eq!(
        billing["incoming"],
        json!([{"module": q("WebApp01"), "edges": 2, "relationships": [q("RelWeb01")]}])
    );
    assert_eq!(
        billing["outgoing"],
        json!([{"module": q("BankApi1"), "edges": 1, "relationships": []}])
    );

    let one = checkout.ok(&["inspect", "Billing"]);
    assert_eq!(one["modules"].as_array().unwrap().len(), 1);
    assert_eq!(one["skips"], json!([]));
    checkout.refused(&["inspect", "Nowhere"], "SUBJECT_NOT_FOUND");
    let (code, text) = checkout.run_raw(&["inspect", "Billing", "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        text,
        "Billing: 3 files, 1 entry point, 2 exported, 1 used outside\n  \
         entry point src/billing/index.ts\n  \
         unused outside: withheld, symbol coverage is incomplete\n  \
         deep import: src/web/index.ts -> src/billing/fee.ts\n  \
         -> Bank: 1 edge, undrawn\n  \
         <- Web: 2 edges, drawn as Web -> Billing [call]\n"
    );
}

/// Appends a line to each of `paths` and commits them.
fn change(checkout: &Checkout, paths: &[&str], message: &str) {
    for path in paths {
        let file = checkout.path().join(path);
        let mut text = fs::read_to_string(&file).unwrap_or_default();
        text.push_str("// changed\n");
        fs::write(file, text).unwrap();
    }
    git(checkout, &["add", "-A"]);
    git(checkout, &["commit", "-q", "-m", message]);
}

#[test]
fn locality_shows_a_local_module_and_a_hidden_coupling() {
    let (checkout, model) = measured();
    let q = |id: &str| format!("{model}#{id}");
    let start = git(&checkout, &["rev-parse", "HEAD"]);
    let start = start.trim();
    for n in 0..3 {
        let paths = ["src/web/index.ts", "src/bank/index.ts"];
        change(&checkout, &paths, &format!("web and bank {n}"));
    }
    change(&checkout, &["src/billing/fee.ts"], "billing");
    change(&checkout, &["src/billing/charge.ts"], "billing again");

    let out = checkout.ok(&["locality", "--since", start]);
    assert_eq!(out["kind"], "locality");
    let limit = boundrail::cochange::HISTORY_LIMIT;
    assert_eq!(
        out["history"],
        json!({"range": format!("{start}..HEAD"), "limit": limit, "commits": 5, "sweeps": 0, "max_files": 30, "shallow": false})
    );
    let billing = &out["modules"][1];
    assert_eq!(
        (
            &billing["name"],
            &billing["commits"],
            &billing["local_share"]
        ),
        (&json!("Billing"), &json!(2), &json!(1.0))
    );
    assert_eq!(
        out["hidden_couplings"],
        json!([{"modules": [q("BankApi1"), q("WebApp01")], "commits": 3, "jaccard": 1.0}])
    );
    assert_eq!(out["unbound_commits"], 0);

    // The whole history holds the commit that added every module at once.
    let out = checkout.ok(&["locality", "--limit", "2"]);
    assert_eq!(out["history"]["commits"], 2);
    assert_eq!(out["history"]["range"], "HEAD");
    assert_eq!(out["hidden_couplings"], json!([]));
    checkout.refused(&["locality", "--since", "nowhere"], "REQUEST_INVALID");
    let (code, text) = checkout.run_raw(&["locality", "--since", start, "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.ends_with("Hidden couplings:\n- Bank and Web: 3 commits together, no relationship\n"),
        "{text}"
    );

    // Moving a file from Billing to Bank touches both: renames are not followed.
    let moved = git(&checkout, &["rev-parse", "HEAD"]);
    git(&checkout, &["mv", "src/billing/fee.ts", "src/bank/fee.ts"]);
    git(&checkout, &["commit", "-q", "-m", "move the fee"]);
    let out = checkout.ok(&["locality", "--since", moved.trim()]);
    let touched: Vec<&Value> = out["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["name"])
        .collect();
    assert_eq!(touched, [&json!("Bank"), &json!("Billing")]);
}

#[test]
fn footprint_shows_the_touched_modules_and_the_model_change() {
    let (checkout, model) = measured();
    let q = |id: &str| format!("{model}#{id}");
    let base = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&checkout, &["checkout", "-q", "-b", "feature"]);
    change(&checkout, &["src/billing/fee.ts"], "fees");
    // Main moves on with a Bank change the branch never saw.
    git(&checkout, &["checkout", "-q", "main"]);
    change(&checkout, &["src/bank/index.ts"], "bank on main");
    git(&checkout, &["checkout", "-q", "feature"]);
    let mut fees = module("Fees0001", "Fees", "module", 4);
    fees["binding"] = json!({"path": "src/billing/fee.ts"});
    fees["parent"] = q("Billing1").into();
    checkout.module(fees);
    sources(&checkout, &[("NOTES.md", "draft\n")]);

    // The working tree since the merge-base: the new module owns the changed fee, which Billing
    // contains; main's Bank change is not the branch's.
    let out = checkout.ok(&["footprint"]);
    assert_eq!(out["kind"], "footprint");
    assert_eq!(
        out["base"],
        json!({"commit": base, "source": "merge-base", "ref": "main"})
    );
    assert_eq!(out["head"], Value::Null);
    assert_eq!(
        out["modules"],
        json!([
            {"module": q("Billing1"), "name": "Billing", "files": [], "in_parts": 1},
            {"module": q("Fees0001"), "name": "Fees", "files": ["src/billing/fee.ts"], "in_parts": 0},
        ])
    );
    assert_eq!(out["outside"], json!(["NOTES.md"]));
    // Web imports the fee past Billing's index: the crossing lifts to Web -> Billing's line.
    assert_eq!(
        out["crossings"],
        json!([{"from": q("WebApp01"), "to": q("Fees0001"), "edges": 1, "hops": 0, "relationships": [q("RelWeb01")]}])
    );
    assert_eq!(out["crossings_skipped"], Value::Null);
    assert_eq!(
        out["model"]["modules"]["added"],
        json!([{"id": q("Fees0001"), "name": "Fees"}])
    );

    // A committed head reads its own model; an explicit base also starts at the merge-base.
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "fees module"]);
    let head = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    let out = checkout.ok(&["footprint", "--base", "main", "--head", "HEAD"]);
    assert_eq!(
        out["base"],
        json!({"commit": base, "source": "explicit", "ref": "main"})
    );
    assert_eq!(out["head"], head);
    assert_eq!(out["model"]["modules"]["added"][0]["id"], q("Fees0001"));
    assert_eq!(out["crossings"][0]["to"], q("Fees0001"));

    // From main, the default base is the merge-base with the head named, and the checkout's
    // scan cannot show that head's edges.
    git(&checkout, &["checkout", "-q", "main"]);
    let out = checkout.ok(&["footprint", "--head", "feature"]);
    assert_eq!(out["base"]["commit"], base);
    let touched: Vec<&Value> = out["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["name"])
        .collect();
    assert_eq!(touched, [&json!("Billing"), &json!("Fees")]);
    assert_eq!(out["crossings"], Value::Null);
    assert_eq!(out["crossings_skipped"], "head-not-checked-out");
    assert_eq!(out["model"]["modules"]["added"][0]["id"], q("Fees0001"));
    let (code, text) =
        checkout.run_raw(&["footprint", "--head", "feature", "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Crossings not checked: head-not-checked-out\n"),
        "{text}"
    );

    let out = checkout.ok(&["footprint", "--base", "HEAD", "--head", "HEAD"]);
    assert_eq!(out["modules"], json!([]));
    assert!(
        out["model"]["modules"]["added"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(out["crossings"], json!([]));
    checkout.refused(&["footprint", "--base", "nowhere"], "REQUEST_INVALID");
    // A base without a vault has no model to compare.
    let first = git(&checkout, &["rev-list", "--max-parents=0", "HEAD"]);
    let out = checkout.ok(&["footprint", "--base", first.trim()]);
    assert_eq!(out["model"], Value::Null);
    assert_eq!(out.get("model_error"), Some(&Value::Null), "{out:#}");
    assert_eq!(out.get("scan_error"), Some(&Value::Null), "{out:#}");
}

#[test]
fn footprint_content_checks_a_working_tree_file_whose_stat_changed() {
    let (checkout, model) = measured();
    let index = checkout.path().join(".git/index");
    let before = fs::read(&index).unwrap();
    // A touch changes the file's stat, not its content.
    let fee = checkout.path().join("src/billing/fee.ts");
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    (fs::File::options().write(true).open(&fee).unwrap())
        .set_modified(old)
        .unwrap();
    let out = checkout.ok(&["footprint"]);
    assert_eq!(out["modules"], json!([]), "{out:#}");
    assert_eq!(out["outside"], json!([]), "{out:#}");
    assert_eq!(
        fs::read(&index).unwrap(),
        before,
        "the user's index is not written"
    );

    fs::write(&fee, "export const fee = 2;\n").unwrap();
    let out = checkout.ok(&["footprint"]);
    assert_eq!(out["modules"][0]["module"], format!("{model}#Billing1"));
    assert_eq!(out["modules"][0]["files"], json!(["src/billing/fee.ts"]));
}

#[test]
fn footprint_reports_a_base_model_that_does_not_load() {
    let (checkout, _) = measured();
    git(&checkout, &["checkout", "-q", "-b", "broken"]);
    checkout.write("modules/Bad00001.module.json", "{");
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "broken model"]);
    let broken = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    // The branch repairs the vault and commits a module file executable, which still loads.
    git(&checkout, &["checkout", "-q", "-b", "repaired"]);
    fs::remove_file(checkout.vault().join("modules/Bad00001.module.json")).unwrap();
    git(&checkout, &["add", "-A"]);
    git(
        &checkout,
        &[
            "update-index",
            "--chmod=+x",
            ".boundrail/model/modules/Billing1.module.json",
        ],
    );
    git(&checkout, &["commit", "-q", "-m", "repaired model"]);

    let out = checkout.ok(&["footprint", "--base", "broken", "--head", "repaired"]);
    assert_eq!(out["base"]["commit"], broken);
    assert_eq!(out["model"], Value::Null);
    let error = out["model_error"].as_str().unwrap();
    assert!(error.contains(&format!("commit {broken}")), "{error}");
    assert!(!error.contains("committed"), "{error}");
}

#[test]
fn footprint_prints_a_pull_request_comment() {
    let (checkout, model) = measured();
    git(&checkout, &["checkout", "-q", "-b", "feature"]);
    change(
        &checkout,
        &["src/billing/fee.ts", "src/billing/charge.ts"],
        "fees",
    );
    let mut fees = module("Fees0001", "Fees", "module", 4);
    fees["binding"] = json!({"path": "src/billing/fee.ts"});
    fees["parent"] = format!("{model}#Billing1").into();
    checkout.module(fees);
    sources(&checkout, &[("NOTES.md", "draft\n")]);

    let (code, text) = checkout.run_raw(&["footprint", "--base", "main", "--format", "pr-comment"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with("<!-- boundrail:footprint -->\n### Architecture footprint\n"),
        "{text}"
    );
    for line in [
        " (`main`) to the working tree.\n",
        "| `Billing` | 1 | 1 |\n",
        "| `Fees` | 1 | 0 |\n",
        "Outside every binding: `NOTES.md`\n",
        "| `Billing` | `Bank` | 1 | 0 | undrawn |\n",
        "| `Web` | `Fees` | 1 | 0 | `Web -> Billing [call]` |\n",
        "- Model modules: 1 added, 0 removed, 0 changed\n",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    checkout.refused(&["read", "--format", "pr-comment"], "REQUEST_INVALID");
    checkout.refused(&["check", "--format", "pr-comment"], "REQUEST_INVALID");
}

/// The measured shop with a committed proposal on main that adds Fees, bound to the fee inside
/// Billing, describes Docs and makes the changes `more`; the checkout is left on a new branch
/// `feature`. The proposal's local ID.
fn fees_proposal(checkout: &Checkout, more: &[Value]) -> String {
    let id = propose(checkout, "Fees", "current");
    let mut changes = vec![
        json!({"type": "add-module", "module": {"name": "Fees", "kind": "module", "order": 4, "parent": "Billing", "binding": {"path": "src/billing/fee.ts"}}}),
        json!({"type": "edit-module", "subject": "Docs", "set": {"description": "How to pay"}}),
    ];
    changes.extend_from_slice(more);
    edited_proposal(checkout, &id, Value::from(changes));
    git(checkout, &["add", "-A"]);
    git(checkout, &["commit", "-q", "-m", "plan fees"]);
    git(checkout, &["checkout", "-q", "-b", "feature"]);
    id
}

/// The qualified IDs of a footprint's proposal subjects under `key`, with their `holds`.
fn subjects(out: &Value, key: &str) -> Vec<(String, Value)> {
    let listed = out["proposal"][key].as_array().unwrap();
    (listed.iter())
        .map(|s| (s["module"].as_str().unwrap().to_owned(), s["holds"].clone()))
        .collect()
}

#[test]
fn footprint_sets_the_changes_beside_what_a_proposal_names() {
    let (checkout, model) = measured();
    let q = |id: &str| format!("{model}#{id}");
    let id = fees_proposal(&checkout, &[]);
    let drawn = checkout.ok(&["read", "--proposal", &id]);
    let fees = (drawn["modules"].as_array().unwrap().iter())
        .find(|m| m["name"] == "Fees")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Without a proposal the key is null, and a small change is not large.
    let out = checkout.ok(&["footprint"]);
    assert_eq!(out["proposal"], Value::Null);
    assert_eq!(out["large"], false);

    // The fee belongs to Fees, which only the proposal binds: Fees is named and changed, and
    // Billing changed only through it, so Fees holds Billing's change. Docs is named but its
    // code never changed.
    change(&checkout, &["src/billing/fee.ts"], "fees");
    let out = checkout.ok(&["footprint", "--proposal", &id]);
    let against = &out["proposal"];
    assert_eq!(against["id"], q(&id));
    assert_eq!(against["name"], "Fees");
    assert_eq!(
        subjects(&out, "named_and_changed"),
        [(fees.clone(), json!([]))]
    );
    assert_eq!(
        subjects(&out, "changed_not_named"),
        [(q("Billing1"), json!([fees]))]
    );
    assert_eq!(
        subjects(&out, "named_not_changed"),
        [(q("Docs0001"), json!([]))]
    );
    assert_eq!(against["outside"], json!([]));
    assert_eq!(against["changed_not_named"][0]["name"], "Billing");
    // The head's own modules keep the head's bindings.
    assert_eq!(out["modules"][0]["files"], json!(["src/billing/fee.ts"]));
    let (code, _) = checkout.run(&["footprint", "--proposal", &id, "--fail-outside"]);
    assert_eq!(code, 0);

    // A change Billing holds itself leaves nothing held, and Web is outside the proposal.
    change(
        &checkout,
        &["src/billing/charge.ts", "src/web/index.ts"],
        "more",
    );
    let out = checkout.ok(&["footprint", "--proposal", &id]);
    assert_eq!(
        subjects(&out, "changed_not_named"),
        [(q("Billing1"), json!([])), (q("WebApp01"), json!([]))]
    );
    assert_eq!(
        out["proposal"]["outside"],
        json!([q("Billing1"), q("WebApp01")])
    );
    let gate = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model", "footprint", "--proposal", &id, "--fail-outside"])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert_eq!(gate.status.code(), Some(1), "{gate:?}");
    let stdout: Value = serde_json::from_slice(&gate.stdout).unwrap();
    assert_eq!(stdout["success"], true);
    let stderr = String::from_utf8(gate.stderr).unwrap();
    let web = format!("Web ({}) changed outside the proposal", q("WebApp01"));
    assert!(stderr.contains(&web), "{stderr}");
    assert!(stderr.contains("Billing ("), "{stderr}");

    // The branch's model changes count: describing Docs in the vault changes it.
    let mut docs = checkout.read_module("Docs0001");
    docs["description"] = "How to pay".into();
    checkout.module(docs);
    let out = checkout.ok(&["footprint", "--proposal", &id]);
    let named: Vec<String> = (subjects(&out, "named_and_changed").into_iter())
        .map(|s| s.0)
        .collect();
    // Lists are in ID order, and Fees has a random ID.
    let (docs, fees) = (q("Docs0001"), fees);
    let mut both = [(docs.as_str(), "Docs"), (fees.as_str(), "Fees")];
    both.sort_unstable();
    let ids: Vec<&str> = both.iter().map(|(id, _)| *id).collect();
    assert_eq!(named, ids);
    assert_eq!(out["proposal"]["named_not_changed"], json!([]));

    let args = ["footprint", "--proposal", &id, "--format", "markdown"];
    let (code, text) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{text}");
    let [(_, first), (_, second)] = both;
    let expected = format!(
        "Against proposal Fees ({}):\n\
         - Named and changed: {first}, {second}\n\
         - Changed, not named: Billing, Web\n\
         - Named, not changed: none\n",
        q(&id)
    );
    assert!(text.ends_with(&expected), "{text}");
    let args = ["footprint", "--proposal", &id, "--format", "pr-comment"];
    let (code, text) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{text}");
    let expected = format!(
        "\nAgainst proposal `Fees` (`{}`):\n\n\
         - Named and changed: `{first}`, `{second}`\n\
         - Changed, not named: `Billing`, `Web`\n\
         - Named, not changed: none\n",
        q(&id)
    );
    assert!(text.ends_with(&expected), "{text}");
}

#[test]
fn footprint_names_what_a_proposal_removes_rebinds_or_relates() {
    let (checkout, model) = measured();
    let q = |id: &str| format!("{model}#{id}");
    sources(
        &checkout,
        &[("src/pages/home.ts", "export const home = 1;\n")],
    );
    let id = fees_proposal(
        &checkout,
        &[
            json!({"type": "remove-module", "subject": "Bank"}),
            json!({"type": "edit-module", "subject": "Web", "set": {"binding": {"path": "src/pages"}}}),
        ],
    );
    change(
        &checkout,
        &["src/bank/index.ts", "src/pages/home.ts"],
        "pages",
    );

    // Bank, which the proposal removes, counts where the head still binds it; the page, which
    // no head binding holds, maps to Web through the proposal's binding. Both are named.
    let out = checkout.ok(&["footprint", "--proposal", &id]);
    assert_eq!(out["outside"], json!(["src/pages/home.ts"]));
    assert_eq!(
        subjects(&out, "named_and_changed"),
        [(q("BankApi1"), json!([])), (q("WebApp01"), json!([]))]
    );
    assert_eq!(out["proposal"]["changed_not_named"], json!([]));
    assert_eq!(out["proposal"]["outside"], json!([]));

    // A proposal that only adds a relationship names both its ends.
    let links = propose(&checkout, "Links", "current");
    edited_proposal(
        &checkout,
        &links,
        json!([{"type": "add-relationship", "source": "Billing", "relationship": {"to": "Bank", "kind": "call", "order": 0}}]),
    );
    let out = checkout.ok(&["footprint", "--proposal", &links]);
    assert_eq!(
        subjects(&out, "named_and_changed"),
        [(q("BankApi1"), json!([]))]
    );
    assert_eq!(
        subjects(&out, "named_not_changed"),
        [(q("Billing1"), json!([]))]
    );
}

#[test]
fn footprint_shows_held_parts_and_flags_a_large_change() {
    let (checkout, _) = measured();
    let id = fees_proposal(&checkout, &[]);
    change(&checkout, &["src/billing/fee.ts"], "fees");
    let args = ["footprint", "--proposal", &id, "--format", "markdown"];
    let (code, text) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{text}");
    let held = "- Changed, not named: Billing (held by Fees)\n";
    assert!(text.contains(held), "{text}");
    assert!(!text.contains("Large change"), "{text}");
    let args = ["footprint", "--proposal", &id, "--format", "pr-comment"];
    let (code, text) = checkout.run_raw(&args);
    assert_eq!(code, 0, "{text}");
    let held = "- Changed, not named: `Billing` (held by `Fees`)\n";
    assert!(text.contains(held), "{text}");
    assert!(!text.contains("Large change"), "{text}");

    // More files than a sweep holds make the change large; files outside every binding count.
    let notes: Vec<String> = (0..=boundrail::cochange::MAX_FILES)
        .map(|n| format!("notes/{n}.md"))
        .collect();
    let notes: Vec<(&str, &str)> = notes.iter().map(|p| (p.as_str(), "note\n")).collect();
    sources(&checkout, &notes);
    let out = checkout.ok(&["footprint", "--proposal", &id]);
    assert_eq!(out["large"], true);
    // Files no binding holds never fail the gate.
    assert_eq!(out["proposal"]["outside"], json!([]));
    let (code, _) = checkout.run(&["footprint", "--proposal", &id, "--fail-outside"]);
    assert_eq!(code, 0);
    let (code, text) = checkout.run_raw(&["footprint", "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Large change: more than 30 files\n"),
        "{text}"
    );
    let (code, text) = checkout.run_raw(&["footprint", "--format", "pr-comment"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("\nLarge change: more than 30 files.\n"),
        "{text}"
    );
}

#[test]
fn footprint_refuses_an_unreadable_ref_or_an_unknown_proposal() {
    let (checkout, _) = measured();
    let id = fees_proposal(&checkout, &[]);
    for args in [
        &["footprint", "--proposal", &id, "--head", "nowhere"][..],
        &["footprint", "--proposal", &id, "--base", "nowhere"],
        &[
            "footprint",
            "--proposal",
            &id,
            "--fail-outside",
            "--base",
            "nowhere",
        ],
    ] {
        let error = checkout.refused(args, "REQUEST_INVALID");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("nowhere"), "{error:#}");
    }
    checkout.refused(
        &["footprint", "--proposal", "Nope0000"],
        "SUBJECT_NOT_FOUND",
    );
    let args = ["footprint", "--proposal", "Nope0000", "--fail-outside"];
    checkout.refused(&args, "SUBJECT_NOT_FOUND");
    // The gate needs a proposal to judge against.
    let (code, _) = checkout.run_raw(&["footprint", "--fail-outside"]);
    assert_eq!(code, 2);
}

#[test]
fn a_shallow_checkout_needs_a_base_and_drops_its_boundary_commit() {
    let (origin, _) = measured();
    change(&origin, &["src/billing/fee.ts"], "billing");
    let shallow = Checkout::outside();
    let url = format!("file://{}", origin.path().display());
    let out = Command::new("git")
        .args(["clone", "-q", "--depth", "2", &url, "."])
        .current_dir(shallow.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    git(&shallow, &["remote", "remove", "origin"]);
    git(&shallow, &["branch", "-m", "main", "work"]);

    let error = shallow.refused(&["footprint"], "REQUEST_INVALID");
    assert!(
        error["message"].as_str().unwrap().contains("--base"),
        "{error:#}"
    );
    // The boundary commit shows its whole tree as added; only the commit after it counts.
    let out = shallow.ok(&["locality"]);
    assert_eq!(out["history"]["shallow"], true);
    assert_eq!(out["history"]["commits"], 1);
    assert_eq!(out["modules"][0]["name"], "Billing");
}

/// A running `model view --no-open --port 0`, killed when dropped.
struct View {
    child: std::process::Child,
    port: u16,
    token: String,
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl View {
    /// Starts the view and reads the URL it prints first.
    fn start(checkout: &Checkout) -> Self {
        use std::io::BufRead as _;
        let mut child = Command::new(env!("CARGO_BIN_EXE_boundrail"))
            .args(["model", "view", "--no-open", "--port", "0"])
            .current_dir(checkout.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut view = View {
            child,
            port: 0,
            token: String::new(),
        };
        let mut url = String::new();
        std::io::BufReader::new(stdout).read_line(&mut url).unwrap();
        let rest = url.trim().strip_prefix("http://127.0.0.1:").expect(&url);
        let (port, token) = rest.split_once("/?t=").expect(&url);
        view.port = port.parse().unwrap();
        view.token = token.to_owned();
        view
    }

    fn connect(&self) -> std::net::TcpStream {
        let stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        stream
    }

    /// Sends `method target` with `host` and returns the status and the whole response.
    fn send(&self, method: &str, target: &str, host: &str) -> (u16, String) {
        use std::io::Read as _;
        let mut stream = self.connect();
        write!(stream, "{method} {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response[9..12].parse().unwrap();
        (status, response)
    }

    fn get(&self, target: &str) -> (u16, String) {
        self.send("GET", target, &format!("127.0.0.1:{}", self.port))
    }

    /// The body of a response, after its head.
    fn body(response: &str) -> &str {
        response.split_once("\r\n\r\n").unwrap().1
    }
}

#[test]
fn view_serves_the_page_and_the_model_to_its_token_only() {
    let checkout = Checkout::new();
    shop(&checkout);
    let view = View::start(&checkout);
    let t = &view.token;

    let (status, page) = view.get(&format!("/?t={t}"));
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Content-Type: text/html"), "{page}");
    assert!(page.contains("<script src=\"app.js\">"), "{page}");
    assert!(page.contains(
        "Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self';"
    ));
    let (status, script) = view.get("/app.js");
    assert_eq!(status, 200);
    assert!(script.contains("EventSource"));

    let (status, read) = view.get(&format!("/api/model?t={t}"));
    assert_eq!(status, 200, "{read}");
    assert!(read.contains("Content-Type: application/json"));
    let read: Value = serde_json::from_str(View::body(&read)).unwrap();
    assert_eq!(
        read,
        checkout.ok(&["read"]),
        "the same output as `model read`"
    );

    let (status, refused) = view.get("/api/model");
    assert_eq!(status, 403, "{refused}");
    let refused: Value = serde_json::from_str(View::body(&refused)).unwrap();
    assert_eq!(refused["kind"], "refusal");
    assert_eq!(view.get(&format!("/api/model?t={}x", &t[1..])).0, 403);

    // A page on another site that resolved its own name to this machine names itself as Host.
    let foreign = view.send(
        "GET",
        &format!("/api/model?t={t}"),
        &format!("evil.example:{}", view.port),
    );
    assert_eq!(foreign.0, 403, "{}", foreign.1);
    assert_eq!(
        view.send("GET", "/", &format!("localhost:{}", view.port)).0,
        200
    );

    let (status, svg) = view.get(&format!("/api/render?t={t}&view=architecture&depth=1"));
    assert_eq!(status, 200, "{svg}");
    assert!(svg.contains("Content-Type: image/svg+xml"));
    assert!(
        svg.contains("Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n")
    );
    let svg = View::body(&svg);
    assert_xml(svg);
    assert!(svg.starts_with("<svg") && svg.contains("Platform"), "{svg}");

    // A refusal keeps its JSON, with a 4xx status.
    let (status, refused) = view.get(&format!("/api/render?t={t}&view=sequence"));
    assert_eq!(status, 400, "{refused}");
    let refused: Value = serde_json::from_str(View::body(&refused)).unwrap();
    assert_eq!(refused["error"]["code"], "REQUEST_INVALID");
    let (status, _) = view.get(&format!("/api/model?t={t}&checkpoint=ffff"));
    assert_eq!(status, 404);
    // One state at a time, as `model read` takes.
    let (status, refused) = view.get(&format!("/api/model?t={t}&checkpoint=ffff&proposal=P"));
    assert_eq!(status, 400, "{refused}");
    let refused: Value = serde_json::from_str(View::body(&refused)).unwrap();
    assert_eq!(refused["error"]["code"], "REQUEST_INVALID");

    for path in ["proposals", "checkpoints", "check"] {
        let (status, out) = view.get(&format!("/api/{path}?t={t}"));
        assert_eq!(status, 200, "{out}");
        let out: Value = serde_json::from_str(View::body(&out)).unwrap();
        assert_eq!(out["kind"], path, "{out:#}");
    }

    let (status, response) = view.send(
        "POST",
        &format!("/api/model?t={t}"),
        &format!("127.0.0.1:{}", view.port),
    );
    assert_eq!(status, 405, "{response}");
    assert!(response.contains("Allow: GET"));
}

#[test]
fn view_events_tell_the_page_when_the_model_changes() {
    use std::io::Read as _;
    let checkout = Checkout::new();
    shop(&checkout);
    let view = View::start(&checkout);
    let mut events = view.connect();
    write!(
        events,
        "GET /api/events?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        view.token, view.port
    )
    .unwrap();
    let mut seen = String::new();
    let read_until = |stream: &mut std::net::TcpStream, seen: &mut String, needle: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut chunk = [0u8; 1024];
        while !seen.contains(needle) {
            assert!(
                std::time::Instant::now() < deadline,
                "no {needle:?} in {seen}"
            );
            let n = stream.read(&mut chunk).expect(seen);
            assert!(n > 0, "the stream closed: {seen}");
            seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    };
    read_until(&mut events, &mut seen, "retry:");
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    for header in [
        "Content-Type: text/event-stream",
        "Connection: close",
        "Referrer-Policy: no-referrer",
    ] {
        assert!(seen.contains(&format!("{header}\r\n")), "{seen}");
    }

    let out = checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "service", "order": 2}},
    ]));
    let revision = out["after"]["digest"].as_str().unwrap();
    // The edit's lock and journal are Boundrail state too, so `state` may come with `model`.
    let changed =
        format!("event: changed\ndata: {{\"revision\":\"{revision}\",\"parts\":[\"model\"");
    read_until(&mut events, &mut seen, &changed);

    // A claim changes no vault file; the page reloads claims and checks only. The edit made the
    // state folder, which the watcher follows from then on.
    seen.clear();
    checkout.claim("tester", json!(["Billing"]));
    let claimed = format!("data: {{\"revision\":\"{revision}\",\"parts\":[\"state\"]}}");
    read_until(&mut events, &mut seen, &claimed);
}

#[test]
fn proposals_lists_its_keys_in_the_order_they_always_had() {
    let checkout = Checkout::new();
    shop(&checkout);
    let (code, out) = checkout.run_raw(&["proposals"]);
    assert_eq!(code, 0, "{out}");
    let at = |key: &str| out.find(&format!("\n  \"{key}\":")).expect(key);
    let keys = ["schema", "kind", "success", "vault", "proposals"];
    let positions: Vec<usize> = keys.iter().map(|k| at(k)).collect();
    assert!(positions.is_sorted(), "{out}");
}

/// Writes `files` into the checkout.
fn source(checkout: &Checkout, files: &[(&str, &str)]) {
    for (path, text) in files {
        let file = checkout.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
}

const COMPOSE: &str = "services:
  api:
    build: ./api
    environment:
      - QUEUE_URL=amqp://rabbit:5672
  worker:
    build:
      context: ./worker
    env_file: worker.env
  rabbit:
    image: rabbitmq:3
  cache:
    image: redis:7
";

/// A compose checkout: an api and a worker built from TypeScript folders, RabbitMQ and an unbound
/// cache. The api names Rabbit's address and the worker the api's. The model holds Api, Worker
/// and Rabbit, bound to the folders and the compose service, with Api -> Rabbit [queue].
fn composed(checkout: &Checkout) -> String {
    source(
        checkout,
        &[
            ("package.json", r#"{"name": "shop"}"#),
            ("api/index.ts", "export const api = 1;\n"),
            ("worker/index.ts", "export const worker = 1;\n"),
            ("docker-compose.yml", COMPOSE),
            ("worker.env", "# results\nAPI_URL=http://api:8080\n"),
        ],
    );
    let model = checkout.init();
    let mut api = module("ApiSvc01", "Api", "service", 0);
    api["binding"] = json!({"path": "api"});
    api["outgoing"] =
        json!([{"id": "RelQue01", "to": format!("{model}#Rabbit01"), "kind": "queue", "order": 0}]);
    checkout.module(api);
    let mut worker = module("Worker01", "Worker", "service", 1);
    worker["binding"] = json!({"path": "worker"});
    checkout.module(worker);
    let mut rabbit = module("Rabbit01", "Rabbit", "queue", 2);
    rabbit["binding"] = json!({"path": "docker-compose.yml", "symbol": "rabbit"});
    checkout.module(rabbit);
    model
}

#[test]
fn check_judges_relationships_by_configured_addresses_in_compose() {
    let checkout = Checkout::new();
    let model = composed(&checkout);
    let q = |id: &str| format!("{model}#{id}");

    // Api names Rabbit's address: supported. The worker names the api's, undrawn.
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_UNDRAWN Worker01, ApiSvc01"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Worker uses Api but no relationship is drawn: worker.env:2 API_URL names api"
    );
    assert_eq!(out["skips"], json!([]));
    let unit = |name: &str, build: Value, owner: Value, reason: Value| {
        json!({"kind": "compose-service", "name": name, "file": "docker-compose.yml",
               "build": build, "owner": owner, "reason": reason})
    };
    assert_eq!(
        out["units"],
        json!([
            unit("api", "api".into(), q("ApiSvc01").into(), Value::Null),
            unit("cache", Value::Null, Value::Null, "infra-unbound".into()),
            unit("rabbit", Value::Null, q("Rabbit01").into(), Value::Null),
            unit("worker", "worker".into(), q("Worker01").into(), Value::Null),
        ])
    );
    let (_, text) = checkout.run_raw(&["check", "--only", "code", "--format", "markdown"]);
    assert!(
        text.ends_with(
            "Skipped:\n- unit infra-unbound: 1\nCheck failed: warnings: 1, information: 0\n"
        ),
        "{text}"
    );

    let mut worker = checkout.read_module("Worker01");
    worker["outgoing"] =
        json!([{"id": "RelApi01", "to": q("ApiSvc01"), "kind": "http", "order": 0}]);
    checkout.module(worker);
    let out = checkout.ok(&["check", "--only", "code"]);
    assert_eq!(out["diagnostics"], json!([]), "{out:#}");

    // Without the address the queue has no evidence.
    let compose = COMPOSE.replace(
        "    environment:\n      - QUEUE_URL=amqp://rabbit:5672\n",
        "",
    );
    source(&checkout, &[("docker-compose.yml", &compose)]);
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_UNSUPPORTED RelQue01"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Api -> Rabbit [queue] has no evidence: Api names no address of Rabbit"
    );

    // Drawn the other way, the address runs against the line.
    source(&checkout, &[("docker-compose.yml", COMPOSE)]);
    let mut api = checkout.read_module("ApiSvc01");
    api["outgoing"] = json!([]);
    checkout.module(api);
    let mut rabbit = checkout.read_module("Rabbit01");
    rabbit["outgoing"] =
        json!([{"id": "RelBack1", "to": q("ApiSvc01"), "kind": "queue", "order": 0}]);
    checkout.module(rabbit);
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_BACKWARDS RelBack1"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Rabbit -> Api [queue] runs backwards: only Api names the address of Rabbit"
    );
}

#[test]
fn check_supports_a_dependency_with_a_workspace_manifest() {
    let checkout = Checkout::new();
    let app = r#"{"name": "@mono/app", "dependencies": {"@mono/lib": "*"}}"#;
    source(
        &checkout,
        &[
            (
                "package.json",
                r#"{"name": "mono", "workspaces": ["packages/*"]}"#,
            ),
            ("packages/app/package.json", app),
            ("packages/app/index.ts", "export const app = 1;\n"),
            ("packages/lib/package.json", r#"{"name": "@mono/lib"}"#),
            ("packages/lib/index.ts", "export const lib = 1;\n"),
        ],
    );
    let model = checkout.init();
    let mut app = module("AppPkg01", "App", "package", 0);
    app["binding"] = json!({"path": "packages/app"});
    app["outgoing"] = json!([
        {"id": "RelDep01", "to": format!("{model}#LibPkg01"), "kind": "dependency", "order": 0}
    ]);
    checkout.module(app);
    let mut lib = module("LibPkg01", "Lib", "package", 1);
    lib["binding"] = json!({"path": "packages/lib"});
    checkout.module(lib);

    let out = checkout.ok(&["check", "--only", "code"]);
    assert_eq!(out["diagnostics"], json!([]), "{out:#}");
    assert_eq!(out["skips"], json!([]));
    assert_eq!(out["units"], json!([]));

    // Without the dependency nothing supports it, and with no unit held by App or Lib the
    // configured-path evidence a dependency also accepts is not collected: an unbound database
    // elsewhere in the checkout changes nothing.
    let app = r#"{"name": "@mono/app"}"#;
    let compose = "services:\n  postgres:\n    image: postgres:16\n";
    source(
        &checkout,
        &[
            ("packages/app/package.json", app),
            ("docker-compose.yml", compose),
        ],
    );
    let out = checkout.ok(&["check", "--only", "code"]);
    assert_eq!(
        out["skips"],
        json!([{"subject": format!("{model}#RelDep01"), "site": "relationship", "reason": "evidence-unavailable"}])
    );
}

/// A compose checkout of a web, an api and a worker built from TypeScript folders bound to Web,
/// Api and Worker, and an unbound database. The worker names the api's address.
const BUILT: &str = "services:
  web:
    build: ./web
  api:
    build: ./api
  worker:
    build: ./worker
    env_file: worker.env
  db:
    image: postgres:16
";

#[test]
fn infrastructure_is_judged_only_once_a_module_binds_a_unit() {
    let checkout = Checkout::new();
    source(
        &checkout,
        &[
            ("package.json", r#"{"name": "shop"}"#),
            ("web/index.ts", "export const web = 1;\n"),
            ("api/index.ts", "export const api = 1;\n"),
            ("worker/index.ts", "export const worker = 1;\n"),
            ("docker-compose.yml", BUILT),
            ("worker.env", "API_URL=http://api:8080\n"),
        ],
    );
    let model = checkout.init();
    let services = [
        ("WebApp01", "Web", "web"),
        ("ApiSvc01", "Api", "api"),
        ("Worker01", "Worker", "worker"),
    ];
    for (order, (id, name, path)) in services.into_iter().enumerate() {
        let mut module = module(id, name, "service", order as u32);
        module["binding"] = json!({"path": path});
        checkout.module(module);
    }
    draw(
        &checkout,
        &model,
        "WebApp01",
        &[("WebApi01", "ApiSvc01", "http")],
    );

    // Build contexts alone opt nothing in: Web -> Api keeps its skip, and the worker naming the
    // api's address reveals no undrawn relationship.
    let out = checkout.ok(&["check", "--only", "code"]);
    assert_eq!(out["diagnostics"], json!([]), "{out:#}");
    let skip = json!({"subject": format!("{model}#WebApi01"), "site": "relationship", "reason": "evidence-unavailable"});
    assert_eq!(out["skips"], json!([skip]));
    let owned = |out: &Value| {
        (out["units"].as_array().unwrap().iter())
            .filter(|u| u["owner"].is_string())
            .count()
    };
    assert_eq!(owned(&out), 3);

    // Binding the database opts the model in.
    let mut db = module("DbStore1", "Db", "datastore", 3);
    db["binding"] = json!({"path": "docker-compose.yml", "symbol": "db"});
    checkout.module(db);
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(
        found(&out),
        [
            "RELATIONSHIP_UNDRAWN Worker01, ApiSvc01",
            "RELATIONSHIP_UNSUPPORTED WebApi01"
        ]
    );
    assert_eq!(owned(&out), 4);
}

#[cfg(unix)]
#[test]
fn a_failed_infrastructure_read_skips_unit_bindings_as_a_failed_scan_does() {
    let checkout = committed();
    source(&checkout, &[("docker-compose.yml", COMPOSE)]);
    let model = checkout.init();
    let mut rabbit = module("Rabbit01", "Rabbit", "queue", 0);
    rabbit["binding"] = json!({"path": "docker-compose.yml", "symbol": "rabbit"});
    checkout.module(rabbit);
    checkout.module(module("Cache001", "Cache", "datastore", 1));
    // The exclusion policy refuses an ignore file outside the checkout.
    let outside = TempDir::new("arch-model-outside").unwrap();
    fs::write(outside.path().join("ignore"), "").unwrap();
    let ignore = checkout.path().join(".archgraphignore");
    std::os::unix::fs::symlink(outside.path().join("ignore"), ignore).unwrap();
    let user = TempDir::new("arch-model-user").unwrap();
    let error = ".archgraphignore must stay inside the scan root";

    let (code, out, stderr) = checkout.run_served(user.path(), &["check"]);
    assert_eq!(code, 1, "{out:#}");
    let warning = format!("warning: reading the checkout's infrastructure failed: {error}");
    assert!(stderr.contains(&warning), "{stderr}");
    let codes = found(&out);
    assert!(
        codes.contains(&"SCAN_FAILED Rabbit01".to_owned()),
        "{out:#}"
    );
    assert!(
        codes.iter().any(|c| c.starts_with("ROOT_UNCONNECTED")),
        "{out:#}"
    );
    let failed = (out["diagnostics"].as_array().unwrap().iter())
        .find(|d| d["code"] == "SCAN_FAILED")
        .unwrap();
    let message = format!("reading the checkout's infrastructure failed: {error}");
    assert_eq!(failed["message"], message);
    let skip =
        json!({"subject": format!("{model}#Rabbit01"), "site": "binding", "reason": "scan-failed"});
    assert!(out["skips"].as_array().unwrap().contains(&skip), "{out:#}");

    let out = checkout.ok(&["inspect"]);
    assert_eq!(out["scan_error"], error);
    let skip = json!({"module": format!("{model}#Rabbit01"), "reason": "scan-failed"});
    assert!(out["skips"].as_array().unwrap().contains(&skip), "{out:#}");

    let out = checkout.ok(&["footprint"]);
    assert_eq!(out["crossings"], Value::Null, "{out:#}");
    assert_eq!(out["crossings_skipped"], "scan-failed");
    assert_eq!(out["scan_error"], error);
}

const K8S: &str = "kind: Deployment
metadata:
  name: web
  namespace: shop
spec:
  template:
    metadata:
      labels: {app: web}
    spec:
      containers:
        - name: web
          env:
            - name: API_URL
              valueFrom:
                configMapKeyRef: {name: web-config, key: api}
---
kind: ConfigMap
metadata: {name: web-config, namespace: shop}
data:
  api: http://api.shop:8080
---
kind: Service
metadata: {name: api, namespace: shop}
spec:
  selector: {app: api}
---
kind: Deployment
metadata: {name: api, namespace: shop}
spec:
  template:
    metadata:
      labels: {app: api}
    spec:
      containers: [{name: api}]
";

#[test]
fn kubernetes_service_names_support_http_and_show_in_inspect() {
    let checkout = Checkout::new();
    source(&checkout, &[("k8s/app.yaml", K8S)]);
    let model = checkout.init();
    let q = |id: &str| format!("{model}#{id}");
    let mut web = module("WebApp01", "Web", "app", 0);
    web["binding"] = json!({"path": "k8s/app.yaml", "symbol": "web"});
    web["outgoing"] = json!([{"id": "RelHtp01", "to": q("ApiSvc01"), "kind": "http", "order": 0}]);
    checkout.module(web);
    let mut api = module("ApiSvc01", "Api", "service", 1);
    api["binding"] = json!({"path": "k8s/app.yaml", "symbol": "api"});
    checkout.module(api);

    // No code is bound, so nothing is scanned.
    let out = checkout.ok(&["check", "--only", "code"]);
    assert_eq!(out["diagnostics"], json!([]), "{out:#}");
    assert_eq!(out["skips"], json!([]));
    let owners: Vec<(&str, &str)> = (out["units"].as_array().unwrap().iter())
        .map(|u| (u["name"].as_str().unwrap(), u["owner"].as_str().unwrap()))
        .collect();
    assert_eq!(
        owners,
        [
            ("web", q("WebApp01").as_str()),
            ("api", q("ApiSvc01").as_str())
        ]
    );

    let out = checkout.ok(&["inspect", "Web"]);
    let hop = json!({"module": q("ApiSvc01"), "evidence": "configured-path", "file": "k8s/app.yaml",
                     "line": 20, "variable": "API_URL", "address": "api.shop"});
    assert_eq!(out["modules"][0]["hops_out"], json!([hop]));
    assert_eq!(out["modules"][0]["hops_in"], json!([]));
    let text = checkout
        .run_raw(&["inspect", "Api", "--format", "markdown"])
        .1;
    assert!(
        text.contains("  <- Web: configured-path at k8s/app.yaml:20 API_URL = api.shop\n"),
        "{text}"
    );

    // Code evidence never applies between modules bound to units.
    let mut api = checkout.read_module("ApiSvc01");
    api["outgoing"] = json!([{"id": "RelCal01", "to": q("WebApp01"), "kind": "call", "order": 0}]);
    checkout.module(api);
    let out = checkout.ok(&["check", "--only", "code"]);
    let skip =
        json!({"subject": q("RelCal01"), "site": "relationship", "reason": "evidence-not-code"});
    assert_eq!(out["skips"], json!([skip]));

    // A unit binding must name a unit its file's folder declares.
    let mut ghost = module("Ghost001", "Ghost", "app", 2);
    ghost["binding"] = json!({"path": "k8s/app.yaml", "symbol": "ghost"});
    checkout.module(ghost);
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["BINDING_SYMBOL_MISSING Ghost001"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Ghost is bound to k8s/app.yaml ghost, but no unit there is named ghost"
    );

    // A unit file the walk never reads, in a hidden folder, is not scanned, and neither are the
    // relationships of the module bound to it.
    let devcontainer = "services:\n  app:\n    image: node:22\n";
    source(
        &checkout,
        &[(".devcontainer/docker-compose.yml", devcontainer)],
    );
    let mut dev = module("DevBox01", "Dev", "app", 3);
    dev["binding"] = json!({"path": ".devcontainer/docker-compose.yml", "symbol": "app"});
    dev["outgoing"] = json!([{"id": "DevWeb01", "to": q("WebApp01"), "kind": "http", "order": 0}]);
    checkout.module(dev);
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["BINDING_SYMBOL_MISSING Ghost001"]);
    let skips = out["skips"].as_array().unwrap();
    for (subject, site) in [("DevBox01", "binding"), ("DevWeb01", "relationship")] {
        let skip = json!({"subject": q(subject), "site": site, "reason": "not-scanned"});
        assert!(skips.contains(&skip), "{skip} in {out:#}");
    }

    // A manifest with no document that parses was not read; one declaring only a Namespace was,
    // and a binding to it finds the units of its folder.
    source(
        &checkout,
        &[
            (
                "deploy/web.yaml",
                "kind: Deployment\nmetadata: {name: web\n",
            ),
            ("k8s/ns.yaml", "kind: Namespace\nmetadata: {name: shop}\n"),
        ],
    );
    for (id, name, path) in [
        ("Broken01", "Broken", "deploy/web.yaml"),
        ("Spaced01", "Spaced", "k8s/ns.yaml"),
    ] {
        let mut bound = module(id, name, "app", 4);
        bound["binding"] = json!({"path": path, "symbol": "web"});
        checkout.module(bound);
    }
    let (code, out) = checkout.run(&["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    // Spaced stands for the web unit, so web's hop to Api is its own.
    assert_eq!(
        found(&out),
        [
            "BINDING_SYMBOL_MISSING Ghost001",
            "RELATIONSHIP_UNDRAWN Spaced01, ApiSvc01"
        ]
    );
    let skips = out["skips"].as_array().unwrap();
    let skip = json!({"subject": q("Broken01"), "site": "binding", "reason": "not-scanned"});
    assert!(skips.contains(&skip), "{skip} in {out:#}");
    assert!(!out["skips"].to_string().contains("Spaced01"), "{out:#}");
}

/// The fake language server's program name.
const FAKE_SERVER: &str = "boundrail-fake-lsp";
/// The fake server's script, in the user's config folder.
const SCRIPT: &str = "fake-lsp.script";
/// The fake server again under an allowlisted name, which runs it with that name's arguments.
const ALLOWLISTED: &str = "typescript-language-server";

/// The program file `name` in `dir`, with the platform's executable suffix.
fn program(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// The folder holding the fake language server, built once with the toolchain's `rustc` from
/// `tests/fake_lsp/server.rs`, so no real language server is needed, and a copy under
/// [`ALLOWLISTED`].
fn fake_server_dir() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let name = format!("fake-lsp-{}", std::process::id());
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        fs::create_dir_all(&dir).unwrap();
        let beside_cargo = std::env::var_os("CARGO").map(|cargo| {
            PathBuf::from(cargo).with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX))
        });
        let rustc = (std::env::var_os("RUSTC").map(PathBuf::from))
            .or(beside_cargo.filter(|rustc| rustc.exists()))
            .unwrap_or_else(|| "rustc".into());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_lsp/server.rs");
        let out = Command::new(rustc)
            .args(["--edition", "2024", "-o"])
            .arg(program(&dir, FAKE_SERVER))
            .arg(source)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::copy(program(&dir, FAKE_SERVER), program(&dir, ALLOWLISTED)).unwrap();
        dir
    })
}

/// Configures language servers: the user's config folder, returned, defines `fake`, the fake
/// server answering by `rules` as (method, needle, result), and the servers of `extra`; the
/// repository names `fake` and the extra servers.
fn serve(checkout: &Checkout, rules: &[(&str, &str, String)], extra: &[Value]) -> TempDir {
    let user = TempDir::new("arch-model-user").unwrap();
    let script: String = (rules.iter())
        .map(|(method, needle, result)| format!("{method}\t{needle}\t{result}\n"))
        .collect();
    let script_path = user.path().join(SCRIPT);
    fs::write(&script_path, script).unwrap();
    let mut defined = vec![json!({
        "name": "fake",
        "command": FAKE_SERVER,
        "args": [script_path],
        "languages": ["typescript", "javascript"],
        "extensions": ["ts", ".js"],
    })];
    defined.extend_from_slice(extra);
    fs::create_dir(user.path().join("boundrail")).unwrap();
    let definitions = json!(defined).to_string();
    fs::write(
        user.path().join("boundrail/language-servers.json"),
        definitions,
    )
    .unwrap();
    let named: Vec<Value> = (defined.iter())
        .map(|d| json!({"server": d["name"]}))
        .collect();
    name_servers(checkout, &json!(named));
    user
}

/// Writes the repository's list of language servers.
fn name_servers(checkout: &Checkout, entries: &Value) {
    let dir = checkout.path().join(".boundrail");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("language-servers.json"), entries.to_string()).unwrap();
}

/// The `file:` URI of the checkout path `path`, as the client sends it.
fn uri(checkout: &Checkout, path: &str) -> String {
    let root = fs::canonicalize(checkout.path()).unwrap();
    format!("file://{}/{path}", root.display())
}

/// A `workspace/symbol` answer declaring `name` on line `line` of `path`.
fn declared(checkout: &Checkout, name: &str, path: &str, line: u64) -> String {
    let range =
        json!({"start": {"line": line, "character": 0}, "end": {"line": line, "character": 40}});
    json!([{"name": name, "kind": 13, "location": {"uri": uri(checkout, path), "range": range}}])
        .to_string()
}

/// A `textDocument/references` answer: a use on each (path, 0-based line).
fn uses(checkout: &Checkout, at: &[(&str, u64)]) -> String {
    let locations: Vec<Value> = (at.iter())
        .map(|(path, line)| {
            let start = json!({"line": line, "character": 2});
            json!({"uri": uri(checkout, path), "range": {"start": start, "end": start}})
        })
        .collect();
    json!(locations).to_string()
}

/// The TypeScript checkout with Web, Billing and Bank bound to its three folders, Web calling
/// Billing and Billing calling Bank drawn, and a fake server that finds Web and Billing calling
/// Bank's `pay` and Web calling Billing's `charge`; Web's `page` is used nowhere. Returns the
/// model ID and the user's config folder.
fn served_shop(checkout: &Checkout) -> (String, TempDir) {
    typescript(checkout);
    let model = checkout.init();
    for (order, (id, name, path)) in [
        ("WebApp01", "Web", "src/web"),
        ("Billing1", "Billing", "src/billing"),
        ("BankApi1", "Bank", "src/bank"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut module = module(id, name, "service", order as u32);
        module["binding"] = json!({"path": path});
        checkout.module(module);
    }
    draw(
        checkout,
        &model,
        "WebApp01",
        &[("WebBil01", "Billing1", "call")],
    );
    draw(
        checkout,
        &model,
        "Billing1",
        &[("BilBnk01", "BankApi1", "call")],
    );
    let (web, billing, bank) = (
        "src/web/index.ts",
        "src/billing/index.ts",
        "src/bank/index.ts",
    );
    let rules = [
        ("initialize", "", json!({"capabilities": {}}).to_string()),
        (
            "workspace/symbol",
            r#""query":"pay""#,
            declared(checkout, "pay", bank, 0),
        ),
        (
            "workspace/symbol",
            r#""query":"charge""#,
            declared(checkout, "charge", billing, 1),
        ),
        (
            "workspace/symbol",
            r#""query":"page""#,
            declared(checkout, "page", web, 1),
        ),
        (
            "textDocument/references",
            bank,
            uses(checkout, &[(billing, 1), (web, 1)]),
        ),
        (
            "textDocument/references",
            billing,
            uses(checkout, &[(web, 1)]),
        ),
        ("textDocument/references", web, "[]".to_owned()),
    ];
    let user = serve(checkout, &rules, &[]);
    (model, user)
}

/// Sets the relationships drawn from `id` as (relationship ID, target ID, kind).
fn draw(checkout: &Checkout, model: &str, id: &str, lines: &[(&str, &str, &str)]) {
    let mut module = checkout.read_module(id);
    module["outgoing"] = (lines.iter().enumerate())
        .map(|(order, (rel, to, kind))| {
            json!({"id": rel, "to": format!("{model}#{to}"), "kind": kind, "order": order})
        })
        .collect();
    checkout.module(module);
}

/// Web's relationships to Billing and, with no import behind it, to Bank.
fn draw_web_to_bank(checkout: &Checkout, model: &str) {
    let lines = [
        ("WebBil01", "Billing1", "call"),
        ("WebBnk01", "BankApi1", "call"),
    ];
    draw(checkout, model, "WebApp01", &lines);
}

const CHECK_SERVED: [&str; 4] = ["check", "--only", "code", "--language-servers"];

#[test]
fn language_server_references_are_call_evidence_where_no_import_exists() {
    let checkout = Checkout::new();
    let (model, user) = served_shop(&checkout);
    draw_web_to_bank(&checkout, &model);
    let user = user.path();

    // Without the flag the config is ignored: Web never imports Bank, and the skip says why.
    let (code, out, stderr) = checkout.run_served(user, &["check", "--only", "code"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_UNSUPPORTED WebBnk01"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Web -> Bank [call] has no code evidence: Web never imports Bank"
    );
    let skip = json!({"subject": ".boundrail/language-servers.json", "site": "language-server", "reason": "language-servers-not-enabled"});
    assert_eq!(out["skips"], json!([skip]));
    assert!(!stderr.contains("language server"), "{stderr}");

    // The repository names `fake`; the user's config defines its program and arguments.
    let (code, out, stderr) = checkout.run_served(user, &CHECK_SERVED);
    assert_eq!(code, 0, "{out:#} {stderr}");
    assert_eq!(out["diagnostics"], json!([]));
    assert_eq!(out["skips"], json!([]));
    let program = program(fake_server_dir(), FAKE_SERVER);
    let line = format!(
        "boundrail: starting language server fake: {} {}\n",
        program.display(),
        user.join(SCRIPT).display()
    );
    assert!(stderr.contains(&line), "{stderr}");

    // Inspect counts uses from the references; Web's `page` is used by no one.
    let (_, out, _) = checkout.run_served(user, &["inspect"]);
    assert_eq!(out["modules"][0]["symbols"]["source"], "scan");
    assert_eq!(out["language_servers"], json!([skip]));
    let (code, out, _) = checkout.run_served(user, &["inspect", "--language-servers"]);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["language_servers"], json!([]));
    let symbols: Vec<(&str, Value)> = (out["modules"].as_array().unwrap().iter())
        .map(|m| (m["name"].as_str().unwrap(), m["symbols"].clone()))
        .collect();
    let page = json!({"file": "src/web/index.ts", "name": "page", "target": {"file": "src/web/index.ts", "name": "page"}});
    let served = |used, unused| json!({"exported": 1, "used": used, "unused": unused, "source": "language-server"});
    assert_eq!(
        symbols,
        [
            ("Bank", served(1, json!([]))),
            ("Billing", served(1, json!([]))),
            ("Web", served(0, json!([page]))),
        ]
    );
}

#[test]
fn an_allowlisted_server_runs_its_own_name_with_fixed_arguments() {
    let checkout = Checkout::new();
    let (model, user) = served_shop(&checkout);
    draw_web_to_bank(&checkout, &model);
    // No user definition of the name: the allowlist's program and `--stdio` run.
    name_servers(
        &checkout,
        &json!([{"server": ALLOWLISTED, "extensions": ["ts"]}]),
    );
    let (code, out, stderr) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 0, "{out:#} {stderr}");
    let program = program(fake_server_dir(), ALLOWLISTED);
    let line = format!(
        "boundrail: starting language server {ALLOWLISTED}: {} --stdio\n",
        program.display()
    );
    assert!(stderr.contains(&line), "{stderr}");
}

#[test]
fn language_server_references_run_backwards_and_find_undrawn_calls() {
    let checkout = Checkout::new();
    let (model, user) = served_shop(&checkout);
    let user = user.path();
    draw(
        &checkout,
        &model,
        "BankApi1",
        &[("BnkWeb01", "WebApp01", "call")],
    );

    let (code, out, _) = checkout.run_served(user, &CHECK_SERVED);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_BACKWARDS BnkWeb01"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Bank -> Web [call] runs backwards: only Web calls Bank"
    );

    draw(&checkout, &model, "BankApi1", &[]);
    let (code, out, _) = checkout.run_served(user, &CHECK_SERVED);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(found(&out), ["RELATIONSHIP_UNDRAWN WebApp01, BankApi1"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Web uses Bank but no relationship is drawn: src/web/index.ts:2 calls pay"
    );
}

#[test]
fn imports_stand_in_for_calls_into_a_module_with_no_symbol_a_server_found() {
    let checkout = Checkout::new();
    let (model, user) = served_shop(&checkout);
    // Web imports Shared for its side effects; Shared exports nothing.
    let web = "import { charge } from \"../billing\";\nexport const page = () => charge(1);\nimport \"../shared\";\n";
    source(
        &checkout,
        &[
            ("src/web/index.ts", web),
            ("src/shared/index.ts", "console.log(\"ready\");\n"),
        ],
    );
    let mut shared = module("Shared01", "Shared", "service", 3);
    shared["binding"] = json!({"path": "src/shared"});
    checkout.module(shared);
    let lines = [
        ("WebBil01", "Billing1", "call"),
        ("WebBnk01", "BankApi1", "call"),
        ("WebShr01", "Shared01", "call"),
    ];
    draw(&checkout, &model, "WebApp01", &lines);

    let (code, out, stderr) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 0, "{out:#} {stderr}");
    assert_eq!(out["diagnostics"], json!([]));
}

#[test]
fn a_reference_belongs_to_the_module_its_use_site_imports_the_symbol_through() {
    let checkout = Checkout::new();
    source(
        &checkout,
        &[
            ("package.json", r#"{"name": "fixture"}"#),
            (
                "src/shop/cart.ts",
                "import { charge } from \"../shared\";\nexport const buy = () => charge(1);\n",
            ),
            (
                "src/shared/index.ts",
                "export { charge } from \"../billing\";\n",
            ),
            (
                "src/billing/index.ts",
                "export const charge = (n: number) => n;\n",
            ),
        ],
    );
    let model = checkout.init();
    for (order, (id, name, path)) in [
        ("ShopApp1", "Shop", "src/shop"),
        ("Shared01", "Shared", "src/shared"),
        ("Billing1", "Billing", "src/billing"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut module = module(id, name, "service", order as u32);
        module["binding"] = json!({"path": path});
        checkout.module(module);
    }
    draw(
        &checkout,
        &model,
        "ShopApp1",
        &[("ShpShr01", "Shared01", "call")],
    );
    draw(
        &checkout,
        &model,
        "Shared01",
        &[("ShrBil01", "Billing1", "call")],
    );
    let (cart, shared, billing) = (
        "src/shop/cart.ts",
        "src/shared/index.ts",
        "src/billing/index.ts",
    );
    let rules = [
        ("initialize", "", json!({"capabilities": {}}).to_string()),
        (
            "workspace/symbol",
            r#""query":"charge""#,
            declared(&checkout, "charge", billing, 0),
        ),
        (
            "workspace/symbol",
            r#""query":"buy""#,
            declared(&checkout, "buy", cart, 1),
        ),
        (
            "textDocument/references",
            billing,
            uses(&checkout, &[(cart, 1), (shared, 0)]),
        ),
        ("textDocument/references", cart, "[]".to_owned()),
    ];
    let user = serve(&checkout, &rules, &[]);

    // The cart calls `charge` through the shared barrel: the call is Shop's to Shared.
    let (code, out, stderr) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 0, "{out:#} {stderr}");
    assert_eq!(out["diagnostics"], json!([]));
}

#[test]
fn language_servers_confirm_or_refute_symbols_the_scan_cannot_index() {
    let checkout = Checkout::new();
    typescript(&checkout);
    let model = checkout.init();
    let pay = "src/bank/pay.js";
    fs::write(
        checkout.path().join(pay),
        "function charge(n) { return n; }\nmodule.exports = { charge };\n",
    )
    .unwrap();
    let mut billing = module("Billing1", "Billing", "service", 0);
    billing["binding"] = json!({"path": pay, "symbol": "charge"});
    checkout.module(billing);
    let mut web = module("WebApp01", "Web", "app", 1);
    web["binding"] = json!({"path": "src/web"});
    web["outgoing"] = json!([{
        "id": "WebBil01", "to": format!("{model}#Billing1"), "kind": "other", "order": 0,
        "anchor": {"path": pay, "symbol": "refund"},
    }]);
    checkout.module(web);
    let rules = [
        (
            "workspace/symbol",
            r#""query":"charge""#,
            declared(&checkout, "charge", pay, 0),
        ),
        ("workspace/symbol", r#""query":"refund""#, "[]".to_owned()),
    ];
    let user = serve(&checkout, &rules, &[]);
    let incomplete = |out: &Value| -> Vec<String> {
        (out["skips"].as_array().unwrap().iter())
            .filter(|s| s["reason"] == "symbols-incomplete")
            .map(|s| {
                s["subject"]
                    .as_str()
                    .unwrap()
                    .split_once('#')
                    .unwrap()
                    .1
                    .to_owned()
            })
            .collect()
    };

    let (code, out, _) = checkout.run_served(user.path(), &["check", "--only", "code"]);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(incomplete(&out), ["Billing1", "WebBil01"]);

    // `charge` is found; an empty answer for `refund` refutes nothing.
    let (code, out, _) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(incomplete(&out), ["WebBil01"]);
    assert_eq!(found(&out), Vec::<String>::new());

    // A server listing the file's symbols without `refund` refutes it.
    let listed = r#"[{"name": "charge", "kind": 12, "children": [{"name": "n"}]}]"#.to_owned();
    let initialized = json!({"capabilities": {"documentSymbolProvider": true}}).to_string();
    let rules = [
        rules[0].clone(),
        rules[1].clone(),
        ("initialize", "", initialized),
        ("textDocument/documentSymbol", pay, listed),
    ];
    let user = serve(&checkout, &rules, &[]);
    let (code, out, _) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(incomplete(&out), Vec::<String>::new());
    assert_eq!(found(&out), ["ANCHOR_SYMBOL_MISSING WebBil01"]);
    assert_eq!(
        out["diagnostics"][0]["message"],
        "Web -> Billing is anchored at src/bank/pay.js refund, but no file there declares refund"
    );

    // A default export lists under its own name, so a list without `default` refutes nothing:
    // the CommonJS file's `module.exports` is its default export.
    let mut billing = checkout.read_module("Billing1");
    billing["binding"] = json!({"path": pay, "symbol": "default"});
    checkout.module(billing);
    let (code, out, _) = checkout.run_served(user.path(), &CHECK_SERVED);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(incomplete(&out), ["Billing1"]);
    assert_eq!(found(&out), ["ANCHOR_SYMBOL_MISSING WebBil01"]);
}

#[test]
fn the_repository_names_servers_but_never_their_commands() {
    let checkout = Checkout::new();
    let (_, user) = served_shop(&checkout);
    let user = user.path();
    let refused = |entries: Value, expected: &str| {
        name_servers(&checkout, &entries);
        // Without the flag the list is not even read.
        let (code, out, _) = checkout.run_served(user, &["check", "--only", "code"]);
        assert_eq!(code, 0, "{out:#}");
        assert_eq!(out["skips"][0]["reason"], "language-servers-not-enabled");
        for args in [&CHECK_SERVED[..], &["inspect", "--language-servers"]] {
            let (code, out, stderr) = checkout.run_served(user, args);
            assert_eq!(
                (code, &out["error"]["code"]),
                (2, &json!("REQUEST_INVALID")),
                "{out:#}"
            );
            let message = out["error"]["message"].as_str().unwrap();
            assert!(message.contains(expected), "{message}");
            assert!(!stderr.contains("starting language server"), "{stderr}");
        }
    };
    let entry = json!({"server": "fake", "command": "./boundrail-fake-lsp", "args": []});
    refused(json!([entry]), "unknown field `command`");
    refused(
        json!([{"server": "gopls", "initialization_options": {}}]),
        "unknown field `initialization_options`",
    );
    let config = user.join("boundrail/language-servers.json");
    let unknown = format!(
        "the language server \"evil\" is not one of typescript-language-server, \
         basedpyright-langserver, pyright-langserver, rust-analyzer, gopls, jdtls, nor defined by \
         the user; define it in {}",
        config.display()
    );
    refused(json!([{"server": "evil"}]), &unknown);

    // The user's own config may name an absolute program, never one relative to the checkout.
    let fake = fs::read_to_string(&config).unwrap();
    for command in ["./boundrail-fake-lsp", "bin\\server", ""] {
        let local = json!([{"name": "fake", "command": command, "args": [], "languages": [], "extensions": ["ts"]}]);
        fs::write(&config, local.to_string()).unwrap();
        refused(
            json!([{"server": "fake"}]),
            "name a program on PATH or an absolute path",
        );
    }
    let mut absolute: Value = serde_json::from_str(&fake).unwrap();
    absolute[0]["command"] = json!(program(fake_server_dir(), FAKE_SERVER));
    fs::write(&config, absolute.to_string()).unwrap();
    // The server answers: only it finds Web calling Bank.
    let (code, out, stderr) = checkout.run_served(user, &CHECK_SERVED);
    assert_eq!(code, 1, "{out:#} {stderr}");
    assert_eq!(found(&out), ["RELATIONSHIP_UNDRAWN WebApp01, BankApi1"]);
    let line = format!(
        "boundrail: starting language server fake: {} ",
        program(fake_server_dir(), FAKE_SERVER).display()
    );
    assert!(stderr.contains(&line), "{stderr}");
}

#[test]
fn a_failed_language_server_is_skipped_and_the_check_falls_back_to_imports() {
    let checkout = Checkout::new();
    let absent = json!({"name": "absent", "command": "boundrail-no-such-server", "args": [], "languages": ["typescript"], "extensions": ["ts"]});
    let (model, _) = served_shop(&checkout);
    draw_web_to_bank(&checkout, &model);
    let user = serve(
        &checkout,
        &[("initialize", "", "hang".to_owned())],
        &[absent],
    );

    let started = Instant::now();
    let mut args = CHECK_SERVED.to_vec();
    args.extend(["--tool-timeout", "2"]);
    let (code, out, stderr) = checkout.run_served(user.path(), &args);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(code, 1, "{out:#}");
    // Web imports Billing, so only the line to Bank lacks evidence.
    assert_eq!(found(&out), ["RELATIONSHIP_UNSUPPORTED WebBnk01"]);
    let failed = |name: &str| json!({"subject": name, "site": "language-server", "reason": "language-server-failed"});
    assert_eq!(out["skips"], json!([failed("absent"), failed("fake")]));
    assert!(stderr.contains("warning: language server fake failed: initialize got no answer in time; stderr: fake server started | fake server hangs on initialize"), "{stderr}");
    assert!(
        stderr.contains(
            "warning: language server absent failed: boundrail-no-such-server is not on PATH"
        ),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn a_server_never_finds_programs_in_relative_path_folders() {
    let checkout = Checkout::new();
    let (model, user) = served_shop(&checkout);
    draw_web_to_bank(&checkout, &model);
    // A program the checkout plants under the fake server's name, on a relative PATH entry.
    let planted = checkout.path().join("planted");
    fs::create_dir(&planted).unwrap();
    let script = format!(
        "#!/bin/sh\ntouch {}\nexit 1\n",
        checkout.path().join("ran").display()
    );
    fs::write(program(&planted, FAKE_SERVER), script).unwrap();
    let mode = <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
    fs::set_permissions(program(&planted, FAKE_SERVER), mode).unwrap();
    // The server is `env`, which looks the fake server up on the PATH it is given.
    let config = user.path().join("boundrail/language-servers.json");
    let mut defined: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    let args = defined[0]["args"].clone();
    defined[0]["command"] = json!("/usr/bin/env");
    defined[0]["args"] = json!([FAKE_SERVER, args[0]]);
    fs::write(&config, defined.to_string()).unwrap();

    let dirs = [Path::new("planted"), fake_server_dir()];
    let (code, out, stderr) = checkout.run_served_after(&dirs, user.path(), &CHECK_SERVED);
    assert!(!checkout.path().join("ran").exists(), "{stderr}");
    assert_eq!(code, 0, "{out:#} {stderr}");
    assert_eq!(out["skips"], json!([]));
}

/// Tour over the shop with Checkout: the architecture with Billing and Web in focus, the parts of
/// Platform one level deep, and Checkout as a sequence with Bank in focus.
fn tour_changes() -> Value {
    json!([{"type": "add-walkthrough", "walkthrough": {"name": "Tour", "order": 0,
        "description": "How a purchase works", "beats": [
        {"title": "The shop", "narration": "Web sells.\nBilling charges.",
         "reading": {"view": "architecture", "focus": ["Billing", "Web", "Billing"]}},
        {"title": "Inside the platform", "narration": "Two parts.",
         "reading": {"view": "architecture", "scope": "Platform", "depth": 1}},
        {"title": "A purchase", "narration": "Step by step.",
         "reading": {"view": "sequence", "flow": "Checkout", "focus": ["Bank"]}},
    ]}}])
}

/// Adds Checkout and Tour: Checkout's IDs, then Tour's and its three beats'.
fn add_tour(checkout: &Checkout) -> (Vec<String>, Vec<String>) {
    let flows = add_checkout(checkout);
    let out = checkout.edited(tour_changes());
    let allocated = out["allocated"].as_array().unwrap();
    let tour = allocated
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect();
    (flows, tour)
}

#[test]
fn a_walkthrough_with_three_beats_reads_back_as_json_and_a_script() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let flows = add_checkout(&checkout);
    let request = checkout.request(tour_changes());
    let (_, schema) = checkout.run(&["schema", "model-edit"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&request));
    let (code, out) = checkout.edit(&request);
    assert_eq!(code, 0, "{out:#}");
    let ids: Vec<&str> = (out["allocated"].as_array().unwrap().iter())
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    let names: Vec<&Value> = (out["allocated"].as_array().unwrap().iter())
        .map(|a| &a["name"])
        .collect();
    assert_eq!(
        names,
        [
            "Tour",
            "beat The shop",
            "beat Inside the platform",
            "beat A purchase"
        ]
    );
    let tour = ids[0];
    assert_eq!(out["subjects"]["added"], json!([tour]));

    let read = checkout.ok(&["read", "--walkthrough", "Tour"]);
    assert_eq!(read["kind"], "read-walkthrough");
    assert_eq!(read["revision"]["digest"], checkout.digest());
    assert_eq!(
        read["walkthrough"],
        json!({"id": tour, "name": "Tour", "order": 0, "description": "How a purchase works", "beats": [
            {"id": ids[1], "title": "The shop", "narration": "Web sells.\nBilling charges.",
             "reading": {"view": "architecture", "focus": [q("Billing1"), q("WebApp01")]}},
            {"id": ids[2], "title": "Inside the platform", "narration": "Two parts.",
             "reading": {"view": "architecture", "scope": q("Platfrm1"), "depth": 1}},
            {"id": ids[3], "title": "A purchase", "narration": "Step by step.",
             "reading": {"view": "sequence", "flow": flows[1], "focus": [q("BankApi1")]}},
        ]})
    );
    assert_eq!(
        checkout.ok(&["read", local(tour)])["walkthrough"],
        read["walkthrough"]
    );
    let (code, text) = checkout.run_raw(&["read", "--walkthrough", tour, "--format", "markdown"]);
    assert_eq!(code, 0);
    assert_eq!(
        text,
        format!(
            "Walkthrough Tour {tour}\n\
             How a purchase works\n\
             1. The shop\n   \
                Web sells.\n   \
                Billing charges.\n   \
                Shows: architecture in the current state, focus on Billing, Web\n\
             2. Inside the platform\n   \
                Two parts.\n   \
                Shows: architecture in the current state, parts of Platform, depth 1\n\
             3. A purchase\n   \
                Step by step.\n   \
                Shows: sequence of flow Checkout in the current state, focus on Bank\n"
        )
    );

    let listed = json!([{"id": tour, "name": "Tour", "order": 0,
        "description": "How a purchase works", "beats": 3}]);
    let whole = checkout.ok(&["read"]);
    assert_eq!(whole["walkthroughs"], listed);
    let (_, text) = checkout.run_raw(&["read", "--format", "markdown"]);
    assert!(
        text.ends_with(&format!("Walkthroughs:\n- Tour {tour}: 3 beats\n")),
        "{text}"
    );
    let path = format!("walkthroughs/{}.walkthrough.json", local(tour));
    let files = whole["revision"]["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["path"] == path.as_str()));
    let file: Value =
        serde_json::from_str(&fs::read_to_string(checkout.vault().join(&path)).unwrap()).unwrap();
    let (_, schema) = checkout.run(&["schema", "model-walkthrough"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&file));
    let mut bad = file.clone();
    bad["beats"][0]["reading"]["zoom"] = 2.into();
    assert!(!validator.is_valid(&bad));
    checkout.refused(&["read", "--walkthrough", "Missing"], "SUBJECT_NOT_FOUND");
}

#[test]
fn a_beat_moves_across_walkthroughs() {
    let checkout = Checkout::new();
    shop(&checkout);
    let (_, tour) = add_tour(&checkout);
    let out = checkout.edited(json!([
        {"type": "add-walkthrough", "walkthrough": {"name": "Short", "order": 1}},
        {"type": "move-beat", "subject": tour[3], "walkthrough": "Short", "position": 0},
        {"type": "move-beat", "subject": tour[2], "walkthrough": "Tour", "position": 0},
    ]));
    let short = out["allocated"][0]["id"].as_str().unwrap();
    assert_eq!(out["subjects"]["added"], json!([short]));
    assert_eq!(out["subjects"]["changed"], json!([tour[0]]));
    let beats = |subject: &str| -> Vec<String> {
        let read = checkout.ok(&["read", "--walkthrough", subject]);
        (read["walkthrough"]["beats"].as_array().unwrap().iter())
            .map(|b| b["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(beats("Tour"), [tour[2].clone(), tour[1].clone()]);
    assert_eq!(beats(short), [tour[3].clone()]);
    let read = checkout.ok(&["read", "--walkthrough", short]);
    assert_eq!(read["walkthrough"]["beats"][0]["title"], "A purchase");
}

#[test]
fn removing_a_module_a_beat_reads_refuses() {
    let checkout = Checkout::new();
    shop(&checkout);
    let out = checkout.edited(
        json!([{"type": "add-walkthrough", "walkthrough": {"name": "Tour", "order": 0,
        "beats": [{"title": "The bank", "narration": "Outside us.",
                   "reading": {"view": "architecture", "focus": ["Bank"]}}]}}]),
    );
    let beat = out["allocated"][1]["id"].as_str().unwrap();
    let error = checkout.edit_refused(
        &checkout.request(json!([{"type": "remove-module", "subject": "Bank"}])),
        "REFERENCED_BY_WALKTHROUGH",
    );
    assert_eq!(error["details"]["beats"], json!([beat]));
    assert_eq!(error["details"]["change"], 0);
    checkout.edited(json!([
        {"type": "edit-beat", "subject": beat, "clear": ["focus"]},
        {"type": "remove-module", "subject": "Bank"},
    ]));

    // Proposals hold no walkthroughs.
    let id = propose(&checkout, "Next", "current");
    let request = proposal_request(
        &checkout,
        &id,
        "tester",
        json!([{"type": "add-walkthrough", "walkthrough": {"name": "Plan", "order": 0}}]),
    );
    let (code, out) = edit_proposal(&checkout, &id, &request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "REQUEST_INVALID");
}

#[test]
fn rendering_a_beat_with_focus_dims_the_other_modules() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let (_, tour) = add_tour(&checkout);
    let dimmed = |svg: &str, class: &str, id: &str| {
        svg.contains(&format!(
            "<g class=\"dimmed\" opacity=\"0.3\">\n<g class=\"{class}\" data-id=\"{id}\">"
        ))
    };
    let first = checkout.render(&["--walkthrough", "Tour", "--beat", "1"]);
    assert!(dimmed(&first, "module", &q("BankApi1")), "{first}");
    assert!(!dimmed(&first, "module", &q("Billing1")));
    assert!(!dimmed(&first, "module", &q("WebApp01")));
    assert!(!dimmed(&first, "module", &q("Platfrm1")));
    assert_eq!(
        first,
        checkout.render(&["--walkthrough", &tour[0], "--beat", &tour[1]])
    );
    let third = checkout.render(&["--walkthrough", "Tour", "--beat", "3"]);
    assert!(dimmed(&third, "lifeline", &q("WebApp01")), "{third}");
    assert!(!dimmed(&third, "lifeline", &q("BankApi1")));
    let second = checkout.render(&["--walkthrough", "Tour", "--beat", "2"]);
    assert!(!second.contains("dimmed"));
    assert!(second.contains("parts of Platform"), "{second}");

    let out = checkout.ok(&[
        "render",
        "--walkthrough",
        "Tour",
        "--beat",
        "3",
        "--format",
        "json",
    ]);
    assert_eq!(out["view"], "sequence");
    checkout.refused(
        &["render", "--walkthrough", "Tour", "--beat", "4"],
        "SUBJECT_NOT_FOUND",
    );
    checkout.refused(&["render", "--walkthrough", "Tour"], "REQUEST_INVALID");
    checkout.refused(
        &[
            "render",
            "--walkthrough",
            "Tour",
            "--beat",
            "1",
            "--view",
            "sequence",
        ],
        "REQUEST_INVALID",
    );
}

#[test]
fn readings_of_checkpoints_skip_without_the_history_ref() {
    let checkout = Checkout::new();
    checkout.init();
    let then = json!({"view": "architecture", "state": {"checkpoint": "Check001"}});
    let out = checkout.edited(json!([
        {"type": "add-walkthrough", "walkthrough": {"name": "Past", "order": 0, "beats": [
            {"title": "Then", "narration": "How it was.", "reading": then}]}},
        {"type": "add-picture", "picture": {"name": "Old", "order": 0, "output": "docs/old.svg",
            "reading": then}},
    ]));
    let ids: Vec<&str> = (out["allocated"].as_array().unwrap().iter())
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    let (beat, picture) = (ids[1], ids[2]);
    for (part, subject, site) in [
        ("semantic", beat, "walkthrough"),
        ("embeds", picture, "picture"),
    ] {
        let (code, out) = checkout.run(&["check", "--only", part]);
        assert_eq!(code, 0, "{out:#}");
        assert_eq!(
            out["skips"],
            json!([{"subject": subject, "site": site, "reason": "history-not-local"}]),
            "{out:#}"
        );
    }
}

#[test]
fn check_warns_about_beats_whose_target_state_is_missing() {
    let checkout = committed();
    let model = shop(&checkout);
    // With the history ref present, an unknown checkpoint warns.
    checkout.ok(&["retain", "--doing", "the shop as built"]);
    let proposal = propose(&checkout, "Next", "current");
    let out = checkout.edited(json!([
        {"type": "add-walkthrough", "walkthrough": {"name": "Plans", "order": 0, "beats": [
            {"title": "Next", "narration": "The plan.",
             "reading": {"view": "architecture", "state": {"proposal": proposal}, "scope": "Platfrm1",
                         "focus": ["BankApi1"]}},
            {"title": "Then", "narration": "How it was.",
             "reading": {"view": "architecture", "state": {"checkpoint": "Check001"}}},
        ]}},
        {"type": "add-walkthrough", "walkthrough": {"name": "Empty", "order": 1}},
    ]));
    let ids: Vec<&str> = (out["allocated"].as_array().unwrap().iter())
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    // Allocated IDs are random, so diagnostics of one code can come in either order.
    let pair = |walkthrough: &str, beat: &str| json!([walkthrough, beat]);
    let found = |checkout: &Checkout| -> Vec<(String, Value, String)> {
        let (code, out) = checkout.run(&["check", "--only", "semantic"]);
        assert_eq!(code, 1, "{out:#}");
        (out["diagnostics"].as_array().unwrap().iter())
            .filter(|d| d["code"].as_str().unwrap().starts_with("WALKTHROUGH_"))
            .map(|d| {
                let message = d["message"].as_str().unwrap().to_owned();
                (
                    d["code"].as_str().unwrap().to_owned(),
                    d["subjects"].clone(),
                    message,
                )
            })
            .collect()
    };
    let first = found(&checkout);
    assert_eq!(first.len(), 2, "{first:#?}");
    assert_eq!(first[0].0, "WALKTHROUGH_TARGET_MISSING");
    assert_eq!(first[0].1, pair(ids[0], ids[2]));
    assert!(
        first[0]
            .2
            .starts_with("Plans, beat 2 (Then): checkpoint Check001 does not load"),
        "{}",
        first[0].2
    );
    assert_eq!(
        (first[1].0.as_str(), &first[1].1, first[1].2.as_str()),
        ("WALKTHROUGH_EMPTY", &json!([ids[3]]), "Empty has no beats")
    );
    // A beat drawing the proposal focuses on modules the proposal holds.
    edited_proposal(
        &checkout,
        &proposal,
        json!([{"type": "remove-module", "subject": "Bank"}]),
    );
    let dropped = found(&checkout);
    assert_eq!(dropped.len(), 3, "{dropped:#?}");
    let focus = (dropped.iter())
        .find(|(_, subjects, _)| *subjects == pair(ids[0], ids[1]))
        .unwrap();
    assert_eq!(
        focus.2,
        format!(
            "Plans, beat 1 (Next): focus {model}#BankApi1 is not a module of proposal {model}#{proposal}"
        )
    );

    let digest = proposal_digest(&checkout, &proposal);
    let shelved = checkout.ok(&[
        "shelve",
        &proposal,
        "--expected-revision",
        &digest,
        "--doing",
        "not now",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ]);
    assert_eq!(shelved["kind"], "shelve");
    assert_eq!(found(&checkout).len(), 3, "a shelved proposal still loads");
    fs::remove_dir_all(checkout.vault().join("proposals").join(&proposal)).unwrap();
    let after = found(&checkout);
    assert_eq!(after.len(), 3, "{after:#?}");
    let gone = (after.iter())
        .find(|(_, subjects, _)| *subjects == pair(ids[0], ids[1]))
        .unwrap();
    assert_eq!(gone.0, "WALKTHROUGH_TARGET_MISSING");
    assert!(
        gone.2.starts_with(&format!(
            "Plans, beat 1 (Next): proposal {model}#{proposal} does not load"
        )),
        "{}",
        gone.2
    );
}

#[test]
fn view_serves_walkthroughs_and_plays_them() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let (_, tour) = add_tour(&checkout);
    let view = View::start(&checkout);
    let t = &view.token;

    let (status, read) = view.get(&format!("/api/walkthrough?t={t}&id=Tour"));
    assert_eq!(status, 200, "{read}");
    let read: Value = serde_json::from_str(View::body(&read)).unwrap();
    assert_eq!(read, checkout.ok(&["read", "--walkthrough", "Tour"]));
    assert_eq!(view.get("/api/walkthrough?id=Tour").0, 403);
    assert_eq!(view.get(&format!("/api/walkthrough?t={t}")).0, 400);
    assert_eq!(view.get(&format!("/api/walkthrough?t={t}&id=Gone")).0, 404);
    let (status, body) = view.get(&format!("/api/render?t={t}&beat=1"));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("REQUEST_INVALID"), "{body}");

    let (status, svg) = view.get(&format!(
        "/api/render?t={t}&walkthrough={}&beat=1",
        tour[0].replace('#', "%23")
    ));
    assert_eq!(status, 200, "{svg}");
    let svg = View::body(&svg);
    assert_xml(svg);
    let bank = format!("<g class=\"module\" data-id=\"{model}#BankApi1\">");
    assert!(
        svg.contains(&format!("<g class=\"dimmed\" opacity=\"0.3\">\n{bank}")),
        "{svg}"
    );

    let (_, page) = view.get("/");
    assert!(page.contains("id=\"walkthroughs\"") && page.contains("id=\"play\""));
    let (_, script) = view.get("/app.js");
    for control in [
        "api('walkthrough'",
        "playWalkthrough",
        "'Previous'",
        "'Next'",
        "ArrowRight",
        "#walkthrough=",
        "aria-current': k === index ? 'step'",
    ] {
        assert!(script.contains(control), "{control}");
    }
}

#[test]
fn walkthrough_edits_leave_a_draft_adoptable() {
    let checkout = Checkout::new();
    shop(&checkout);
    let out = checkout.edited(
        json!([{"type": "add-walkthrough", "walkthrough": {"name": "Tour", "order": 0,
        "beats": [{"title": "The bank", "narration": "Outside us.",
                   "reading": {"view": "architecture", "focus": ["Bank"]}}]}}]),
    );
    let beat = out["allocated"][1]["id"].as_str().unwrap().to_owned();
    // The draft drops Bank, which the beat focuses on.
    let (id, _) = next(&checkout);
    checkout.edited(json!([
        {"type": "edit-beat", "subject": beat, "set": {"narration": "Still outside us."}},
    ]));
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "MODEL_INVALID");
    let problems = out["error"]["details"]["problems"].to_string();
    assert!(problems.contains("is not a module"), "{problems}");
    checkout.edited(json!([{"type": "edit-beat", "subject": beat, "clear": ["focus"]}]));
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "adopt");
}

#[test]
fn removing_a_beat_after_drawing_a_draft_leaves_it_adoptable() {
    let checkout = Checkout::new();
    shop(&checkout);
    let out = checkout.edited(json!([{"type": "add-walkthrough", "walkthrough": {
        "name": "Tour", "order": 0, "beats": [
            {"title": "The web", "narration": "Where it starts.", "reading": {"view": "architecture"}},
            {"title": "Again", "narration": "Once more.", "reading": {"view": "architecture"}}]}}]));
    let beat = out["allocated"][2]["id"].as_str().unwrap().to_owned();
    let (id, _) = next(&checkout);
    checkout.edited(json!([{"type": "remove-beat", "subject": beat}]));
    let retired = checkout.ok(&["read"])["manifest"]["retired_ids"].clone();
    assert!(
        retired.as_array().unwrap().contains(&json!(local(&beat))),
        "{retired}"
    );
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "adopt");
}

#[test]
fn a_draft_recording_the_earlier_predecessor_digest_still_adopts() {
    let checkout = Checkout::new();
    shop(&checkout);
    let out = checkout.edited(json!([{"type": "add-module", "module":
        {"name": "Spare", "kind": "service", "order": 9}}]));
    let spare = out["allocated"][0]["id"].as_str().unwrap().to_owned();
    checkout.edited(json!([{"type": "remove-module", "subject": spare}]));
    let (id, _) = next(&checkout);
    // With no walkthrough or picture files, the earlier digest is the whole revision's, model.json
    // with its retired IDs included.
    let file = format!("proposals/{id}/proposal.json");
    let mut proposal: Value =
        serde_json::from_str(&fs::read_to_string(checkout.vault().join(&file)).unwrap()).unwrap();
    let earlier = checkout.digest();
    assert_ne!(proposal["predecessor"]["revision"], earlier);
    proposal["predecessor"]["revision"] = earlier;
    checkout.write(
        &file,
        &(serde_json::to_string_pretty(&proposal).unwrap() + "\n"),
    );
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "adopt");
}

#[test]
fn removing_a_module_after_drawing_a_draft_refuses_predecessor_moved() {
    let checkout = Checkout::new();
    shop(&checkout);
    let out = checkout.edited(json!([{"type": "add-module", "module":
        {"name": "Spare", "kind": "service", "order": 9}}]));
    let spare = out["allocated"][0]["id"].as_str().unwrap().to_owned();
    let (id, _) = next(&checkout);
    checkout.edited(json!([{"type": "remove-module", "subject": spare}]));
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "PREDECESSOR_MOVED");
}

/// Adds the picture Shop: the architecture with Billing in focus, drawn to `output`. Returns its
/// qualified ID.
fn add_picture(checkout: &Checkout, output: &str) -> String {
    let out = checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Shop", "order": 0, "description": "The whole shop", "output": output,
        "reading": {"view": "architecture", "focus": ["Billing"]}}}]));
    out["allocated"][0]["id"].as_str().unwrap().to_owned()
}

/// `model` with `path` as PATH: exit code and stdout as JSON.
fn run_on_path(checkout: &Checkout, path: &std::ffi::OsStr, args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(["model"])
        .args(args)
        .current_dir(checkout.path())
        .env("PATH", path)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    (out.status.code().unwrap(), value)
}

/// The codes and subjects of the `PICTURE_` diagnostics of `model check --only embeds`, with its
/// exit code.
fn embeds(checkout: &Checkout) -> (i32, Vec<(String, Value)>) {
    let (code, out) = checkout.run(&["check", "--only", "embeds"]);
    let found = (out["diagnostics"].as_array().unwrap().iter())
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_owned(),
                d["subjects"].clone(),
            )
        })
        .collect();
    (code, found)
}

#[test]
fn an_svg_picture_is_written_checked_and_goes_stale_after_an_edit() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let id = add_picture(&checkout, "docs/shop.svg");
    let read = checkout.ok(&["read", "--picture", "Shop"]);
    assert_eq!(read["kind"], "read-picture");
    let entry = json!({"id": id, "name": "Shop", "order": 0, "description": "The whole shop",
        "output": "docs/shop.svg", "reading": {"view": "architecture", "focus": [format!("{model}#Billing1")]}});
    assert_eq!(read["picture"], entry);
    assert_eq!(checkout.ok(&["read", local(&id)])["picture"], entry);
    assert_eq!(checkout.ok(&["read"])["pictures"], json!([entry]));
    let (_, text) = checkout.run_raw(&["read", "--picture", &id, "--format", "markdown"]);
    assert_eq!(
        text,
        format!(
            "Picture Shop {id}\nThe whole shop\nWrites: docs/shop.svg\n\
             Shows: architecture in the current state, focus on Billing\n"
        )
    );
    let path = format!("pictures/{}.picture.json", local(&id));
    let file: Value =
        serde_json::from_str(&fs::read_to_string(checkout.vault().join(&path)).unwrap()).unwrap();
    let (_, schema) = checkout.run(&["schema", "model-picture"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&file));
    let mut bad = file.clone();
    bad["output"] = "docs/shop.gif".into();
    assert!(!validator.is_valid(&bad));

    let missing = json!([("PICTURE_MISSING", json!([id]))]);
    let (code, found) = embeds(&checkout);
    assert_eq!((code, json!(found)), (1, missing));
    let listed = checkout.ok(&["pictures"]);
    assert_eq!(listed["kind"], "pictures");
    assert_eq!(
        listed["pictures"],
        json!([{"id": id, "name": "Shop", "output": "docs/shop.svg", "format": "svg",
                "reading": entry["reading"], "status": "missing", "written": false}])
    );

    let revision = checkout.digest();
    let written = checkout.ok(&["pictures", "--write"]);
    assert_eq!(written["pictures"][0]["status"], "fresh");
    assert_eq!(written["pictures"][0]["written"], true);
    assert_eq!(
        checkout.digest(),
        revision,
        "writing pictures leaves the vault alone"
    );
    let svg = fs::read_to_string(checkout.path().join("docs/shop.svg")).unwrap();
    assert_eq!(svg, checkout.render(&["--picture", "Shop"]));
    assert!(svg.contains("class=\"dimmed\""), "the focus dims the rest");
    assert!(
        svg.lines()
            .next()
            .unwrap()
            .ends_with(" data-generator=\"boundrail\">")
    );
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    // A checkout that stores text with CRLF line endings keeps the picture fresh.
    let crlf = svg.replace('\n', "\r\n");
    fs::write(checkout.path().join("docs/shop.svg"), &crlf).unwrap();
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    let again = checkout.ok(&["pictures", "--write"]);
    assert_eq!(again["pictures"][0]["written"], false);

    checkout.edited(
        json!([{"type": "add-module", "module": {"name": "Audit", "kind": "service", "order": 2}}]),
    );
    let stale = json!([("PICTURE_STALE", json!([id]))]);
    let (code, found) = embeds(&checkout);
    assert_eq!((code, json!(found)), (1, stale));
    let (_, text) = checkout.run_raw(&["check", "--only", "embeds", "--format", "markdown"]);
    assert!(
        text.contains("PICTURE_STALE Shop: docs/shop.svg does not match the model"),
        "{text}"
    );
    let (_, text) = checkout.run_raw(&["pictures", "--format", "markdown"]);
    assert_eq!(text, format!("- Shop {id}: docs/shop.svg stale\n"));
    checkout.ok(&["pictures", "--write", "--only", "Shop"]);
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    checkout.refused(&["pictures", "--only", "Nothing"], "SUBJECT_NOT_FOUND");
    checkout.refused(
        &["render", "--picture", "Shop", "--view", "sequence"],
        "REQUEST_INVALID",
    );

    fs::remove_file(checkout.path().join("docs/shop.svg")).unwrap();
    let (code, found) = embeds(&checkout);
    assert_eq!((code, found[0].0.as_str()), (1, "PICTURE_MISSING"));
}

#[test]
fn removing_a_module_a_picture_reads_refuses() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = add_picture(&checkout, "docs/shop.svg");
    // A claim on the picture holds it.
    let claim = checkout.claim("alice", json!([id]));
    let edit = json!([{"type": "edit-picture", "subject": "Shop", "set": {"name": "Store"}}]);
    let request = checkout.request_by("bob", None, edit);
    checkout.edit_refused(&request, "CLAIM_CONFLICT");
    assert_eq!(checkout.release(&claim, "alice").0, 0);
    let error = checkout.edit_refused(
        &checkout.request(json!([{"type": "remove-module", "subject": "Billing"}])),
        "REFERENCED_BY_PICTURE",
    );
    assert_eq!(error["details"]["pictures"], json!([id]));
    assert_eq!(error["details"]["change"], 0);
    checkout.edited(json!([
        {"type": "edit-picture", "subject": "Shop", "clear": ["focus"]},
        {"type": "remove-module", "subject": "Billing"},
    ]));

    // Proposals hold no pictures.
    let proposal = propose(&checkout, "Next", "current");
    let request = proposal_request(
        &checkout,
        &proposal,
        "tester",
        json!([{"type": "remove-picture", "subject": "Shop"}]),
    );
    let (code, out) = edit_proposal(&checkout, &proposal, &request);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "REQUEST_INVALID");
}

#[test]
fn picture_outputs_stay_in_the_repository_outside_boundrail_and_git() {
    let checkout = Checkout::new();
    shop(&checkout);
    for output in [
        ".boundrail/shop.svg",
        ".git/shop.svg",
        "/tmp/shop.svg",
        "../shop.svg",
        "docs/../../shop.svg",
        "docs/shop.jpg",
    ] {
        let request = checkout.request(json!([{"type": "add-picture", "picture": {
            "name": "Shop", "order": 0, "output": output, "reading": {"view": "architecture"}}}]));
        let error = checkout.edit_refused(&request, "MODEL_INVALID");
        let problems = error["details"]["problems"].to_string();
        assert!(problems.contains("output"), "{output}: {problems}");
    }
}

#[test]
fn a_picture_whose_state_is_gone_cannot_be_checked() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let proposal = propose(&checkout, "Next", "current");
    let out = checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Plan", "order": 0, "output": "docs/plan.svg",
        "reading": {"view": "architecture", "state": {"proposal": proposal}}}}]));
    let id = out["allocated"][0]["id"].as_str().unwrap();
    checkout.ok(&["pictures", "--write"]);
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    fs::remove_dir_all(checkout.vault().join("proposals").join(&proposal)).unwrap();
    let (code, out) = checkout.run(&["check", "--only", "embeds"]);
    assert_eq!(code, 1, "{out:#}");
    let diagnostics = out["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{out:#}");
    assert_eq!(diagnostics[0]["code"], "PICTURE_TARGET_MISSING");
    assert_eq!(diagnostics[0]["severity"], "warning");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("Plan: proposal {model}#{proposal} does not load")),
        "{message}"
    );
    assert_eq!(
        out["skips"],
        json!([{"subject": id, "site": "picture", "reason": "unrenderable"}])
    );
    let listed = checkout.ok(&["pictures"]);
    assert_eq!(listed["pictures"][0]["reason"], "render-failed");
    checkout.refused(
        &["check", "--only", "embeds", "--proposal", &proposal],
        "REQUEST_INVALID",
    );
}

#[test]
fn a_picture_whose_proposal_drops_its_focus_reports_a_missing_target() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let proposal = propose(&checkout, "Next", "current");
    let out = checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Plan", "order": 0, "output": "docs/plan.svg",
        "reading": {"view": "architecture", "state": {"proposal": proposal}, "focus": ["BankApi1"]}}}]));
    let id = out["allocated"][0]["id"].as_str().unwrap().to_owned();
    edited_proposal(
        &checkout,
        &proposal,
        json!([{"type": "remove-module", "subject": "Bank"}]),
    );
    checkout.ok(&["pictures", "--write"]);
    let (code, out) = checkout.run(&["check", "--only", "embeds"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(out["diagnostics"][0]["code"], "PICTURE_TARGET_MISSING");
    assert_eq!(out["diagnostics"][0]["subjects"], json!([id]));
    assert_eq!(
        out["diagnostics"][0]["message"],
        format!("Plan: focus {model}#BankApi1 is not a module of proposal {model}#{proposal}")
    );
}

#[test]
fn pictures_overwrite_only_files_boundrail_drew() {
    let checkout = Checkout::new();
    shop(&checkout);
    let favicon = "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"4\"/></svg>\n";
    fs::create_dir_all(checkout.path().join("public")).unwrap();
    fs::write(checkout.path().join("public/favicon.svg"), favicon).unwrap();
    add_picture(&checkout, "public/favicon.svg");
    checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Plain", "order": 1, "output": "docs/plain.svg", "reading": {"view": "architecture"}}}]));
    let (code, out) = checkout.run(&["pictures", "--write"]);
    assert_eq!(
        code, 0,
        "a blocked picture still prints its status: {out:#}"
    );
    let blocked = &out["pictures"][0];
    assert_eq!(blocked["reason"], "output-blocked", "{out:#}");
    let message = blocked["message"].as_str().unwrap();
    assert!(
        message.contains("was not drawn by Boundrail; delete it to draw it here"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(checkout.path().join("public/favicon.svg")).unwrap(),
        favicon
    );
    // The missing file was written anyway.
    assert_eq!(out["pictures"][1]["written"], true);
    let (code, found) = embeds(&checkout);
    assert_eq!((code, found[0].0.as_str()), (1, "PICTURE_UNRENDERABLE"));

    // A picture Boundrail drew is redrawn over.
    checkout.edited(
        json!([{"type": "add-module", "module": {"name": "Audit", "kind": "service", "order": 2}}]),
    );
    let before = fs::read_to_string(checkout.path().join("docs/plain.svg")).unwrap();
    let out = checkout.ok(&["pictures", "--write", "--only", "Plain"]);
    assert_eq!(out["pictures"][0]["written"], true);
    assert_ne!(
        fs::read_to_string(checkout.path().join("docs/plain.svg")).unwrap(),
        before
    );
}

/// A valid 1x1 grayscale PNG, as a rasterizer would write one.
fn tiny_png() -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend(kind);
        out.extend(data);
        let crc = boundrail::arch::picture::crc32(&[kind, data]);
        out.extend(crc.to_be_bytes());
    };
    chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
    chunk(
        b"IDAT",
        &[0x78, 0x01, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01],
    );
    chunk(b"IEND", &[]);
    out
}

/// A folder holding a fake `resvg` that writes `tiny_png` to its second argument once its first,
/// the SVG, exists; and PATH with that folder first.
#[cfg(unix)]
fn fake_rasterizer() -> (TempDir, std::ffi::OsString) {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("fake-resvg").unwrap();
    let png = dir.path().join("tiny.png");
    fs::write(&png, tiny_png()).unwrap();
    let script = dir.path().join("resvg");
    let body = format!(
        "#!/bin/sh\ntest -s \"$1\" || exit 3\ncat '{}' > \"$2\"\n",
        png.display()
    );
    fs::write(&script, body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(dir.path().to_path_buf()).chain(std::env::split_paths(&path));
    let path = std::env::join_paths(dirs).unwrap();
    (dir, path)
}

#[cfg(unix)]
#[test]
fn a_png_picture_carries_the_digest_of_the_svg_it_was_drawn_from() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = add_picture(&checkout, "docs/shop.png");
    let (_bin, path) = fake_rasterizer();
    let (code, out) = run_on_path(&checkout, &path, &["pictures", "--write"]);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["pictures"][0]["format"], "png");
    assert_eq!(out["pictures"][0]["status"], "fresh", "{out:#}");
    assert_eq!(out["pictures"][0]["written"], true);
    let png = fs::read(checkout.path().join("docs/shop.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let svg = checkout.render(&["--picture", "Shop"]);
    assert_eq!(
        boundrail::arch::picture::source(&png),
        Some(sha256(svg.as_bytes()))
    );
    // Checking reads the digest; it needs no rasterizer.
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    checkout.edited(
        json!([{"type": "add-module", "module": {"name": "Audit", "kind": "service", "order": 2}}]),
    );
    let (code, found) = embeds(&checkout);
    assert_eq!(
        (code, json!(found)),
        (1, json!([("PICTURE_STALE", json!([id]))]))
    );
    let (code, out) = run_on_path(&checkout, &path, &["pictures", "--write"]);
    assert_eq!((code, &out["pictures"][0]["written"]), (0, &json!(true)));
    assert_eq!(embeds(&checkout), (0, Vec::new()));
    // A PNG without the chunk was not drawn by Boundrail: it is never overwritten.
    fs::write(checkout.path().join("docs/shop.png"), tiny_png()).unwrap();
    assert_eq!(embeds(&checkout).1[0].0, "PICTURE_UNRENDERABLE");
    let (code, out) = run_on_path(&checkout, &path, &["pictures", "--write"]);
    assert_eq!(
        (code, &out["pictures"][0]["reason"]),
        (0, &json!("output-blocked"))
    );
    assert_eq!(
        fs::read(checkout.path().join("docs/shop.png")).unwrap(),
        tiny_png()
    );
}

#[cfg(unix)]
#[test]
fn without_a_rasterizer_png_pictures_are_unrenderable_and_svgs_still_written() {
    let checkout = Checkout::new();
    shop(&checkout);
    add_picture(&checkout, "docs/shop.png");
    checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Plain", "order": 1, "output": "docs/plain.svg", "reading": {"view": "architecture"}}}]));
    // PATH holds Git alone.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let git = (std::env::split_paths(&path))
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap();
    let bin = TempDir::new("git-only").unwrap();
    std::os::unix::fs::symlink(git, bin.path().join("git")).unwrap();
    let only_git = bin.path().as_os_str();

    let (code, out) = run_on_path(&checkout, only_git, &["pictures", "--write"]);
    assert_eq!(
        code, 0,
        "an unrenderable picture still prints its status: {out:#}"
    );
    let png = &out["pictures"][0];
    assert_eq!(png["status"], "unrenderable", "{out:#}");
    assert_eq!(png["reason"], "no-rasterizer");
    assert!(
        png["message"]
            .as_str()
            .unwrap()
            .contains("resvg, rsvg-convert, magick"),
        "{png}"
    );
    assert_eq!(out["pictures"][1]["status"], "fresh");
    assert_eq!(out["pictures"][1]["written"], true);
    assert!(checkout.path().join("docs/plain.svg").is_file());
    assert!(!checkout.path().join("docs/shop.png").exists());
    // The check needs no rasterizer: the PNG is missing, not unrenderable.
    let (code, out) = run_on_path(&checkout, only_git, &["check", "--only", "embeds"]);
    assert_eq!(code, 1, "{out:#}");
    assert_eq!(out["diagnostics"][0]["code"], "PICTURE_MISSING");
}

#[cfg(unix)]
#[test]
fn pictures_never_write_through_a_linked_folder() {
    let checkout = Checkout::new();
    shop(&checkout);
    let elsewhere = TempDir::new("picture-elsewhere").unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), checkout.path().join("docs")).unwrap();
    add_picture(&checkout, "docs/shop.svg");
    let (code, out) = checkout.run(&["pictures", "--write"]);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["pictures"][0]["status"], "unrenderable");
    assert_eq!(out["pictures"][0]["reason"], "output-blocked");
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn view_lists_pictures_and_draws_one_with_its_focus() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let id = add_picture(&checkout, "docs/shop.svg");
    let view = View::start(&checkout);
    let t = &view.token;
    let (status, listed) = view.get(&format!("/api/pictures?t={t}"));
    assert_eq!(status, 200, "{listed}");
    let listed: Value = serde_json::from_str(View::body(&listed)).unwrap();
    // Without `status` the recipes alone come back, nothing drawn.
    assert_eq!(listed["pictures"], checkout.ok(&["read"])["pictures"]);
    let (status, checked) = view.get(&format!("/api/pictures?t={t}&status=1"));
    assert_eq!(status, 200, "{checked}");
    let checked: Value = serde_json::from_str(View::body(&checked)).unwrap();
    assert_eq!(checked, checkout.ok(&["pictures"]));
    assert_eq!(view.get("/api/pictures").0, 403);
    let (status, svg) = view.get(&format!(
        "/api/render?t={t}&picture={}",
        id.replace('#', "%23")
    ));
    assert_eq!(status, 200, "{svg}");
    let bank = format!("<g class=\"module\" data-id=\"{model}#BankApi1\">");
    assert!(
        View::body(&svg).contains(&format!("<g class=\"dimmed\" opacity=\"0.3\">\n{bank}")),
        "{svg}"
    );
    let (_, page) = view.get("/");
    assert!(page.contains("id=\"pictures\""));
    let (_, script) = view.get("/app.js");
    for control in [
        "api('pictures')",
        "api('pictures', { status: 1 })",
        "showPicture",
        "{ picture: id }",
    ] {
        assert!(script.contains(control), "{control}");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_write_blocks_only_its_own_picture() {
    use std::os::unix::fs::PermissionsExt;
    let checkout = Checkout::new();
    shop(&checkout);
    let locked = checkout.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    add_picture(&checkout, "locked/shop.svg");
    checkout.edited(json!([{"type": "add-picture", "picture": {
        "name": "Plain", "order": 1, "output": "docs/plain.svg", "reading": {"view": "architecture"}}}]));
    let (code, out) = checkout.run(&["pictures", "--write"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["pictures"][0]["reason"], "output-blocked", "{out:#}");
    assert!(
        out["pictures"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("writing locked/shop.svg")
    );
    assert_eq!(out["pictures"][1]["written"], true);
}

/// `model reconcile` of the proposal `id` at the present revisions, by tester.
fn reconcile(checkout: &Checkout, id: &str) -> (i32, Value) {
    checkout.run(&[
        "reconcile",
        id,
        "--expected-revision",
        checkout.digest().as_str().unwrap(),
        "--expected-proposal-revision",
        &proposal_digest(checkout, id),
        "--doing",
        "catch up with the current state",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ])
}

/// Refuses reconciling `id` with `expected`, leaving both revisions as they were: the conflicts.
fn reconcile_refused(checkout: &Checkout, id: &str, expected: &str) -> Value {
    let before = (checkout.digest(), proposal_digest(checkout, id));
    let (code, out) = reconcile(checkout, id);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], expected, "{out:#}");
    assert_eq!((checkout.digest(), proposal_digest(checkout, id)), before);
    out["error"]["details"]["conflicts"].clone()
}

#[test]
fn reconciling_a_draft_merges_what_the_current_state_gained_and_it_then_adopts() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let q = |id: &str| format!("{model}#{id}");
    let id = propose(&checkout, "Next", "current");
    let folder = checkout.vault().join("proposals").join(&id);
    let file: Value =
        serde_json::from_str(&fs::read_to_string(folder.join("proposal.json")).unwrap()).unwrap();
    assert_eq!(file["base"], true);
    assert!(folder.join("base/modules/Billing1.module.json").is_file());
    // The base stays out of what a proposal reads.
    assert_eq!(
        names(&checkout.ok(&["read", "--proposal", &id])),
        names(&checkout.ok(&["read"]))
    );
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "edit-module", "subject": "Billing", "set": {"name": "Payments"}}]),
    );
    let out = checkout.edited(json!([
        {"type": "add-module", "module": {"name": "Ledger", "kind": "datastore", "order": 2}},
        {"type": "add-relationship", "source": "Billing", "relationship": {"to": "Ledger", "kind": "call", "order": 1}},
    ]));
    let ledger = local(out["allocated"][0]["id"].as_str().unwrap()).to_owned();
    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(out["error"]["code"], "PREDECESSOR_MOVED", "{code} {out:#}");

    let (code, out) = reconcile(&checkout, &id);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "reconcile");
    assert!(
        out["subjects"]["added"]
            .as_array()
            .unwrap()
            .contains(&json!(q(&ledger)))
    );
    let read = checkout.ok(&["read", "--proposal", &id]);
    assert_eq!(
        names(&read),
        ["Bank", "Ledger", "Payments", "Platform", "Web"]
    );
    // The rename and the new relationship from the renamed module both landed.
    let payments = checkout.ok(&["read", "--proposal", &id, "Payments"]);
    assert_eq!(payments["outgoing"].as_array().unwrap().len(), 2);
    assert!(
        folder
            .join(format!("base/modules/{ledger}.module.json"))
            .is_file()
    );

    let (code, out) = adopt(&checkout, &id, &checkout.digest());
    assert_eq!(code, 0, "{out:#}");
    let current = checkout.ok(&["read"]);
    assert_eq!(
        names(&current),
        ["Bank", "Ledger", "Payments", "Platform", "Web"]
    );
}

#[test]
fn reconcile_refuses_both_sides_changing_a_module_until_the_proposal_matches_one() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let set = |text: &str| json!([{"type": "edit-module", "subject": "Billing", "set": {"responsibility": text}}]);
    edited_proposal(&checkout, &id, set("Takes payments"));
    checkout.edited(set("Bills customers"));
    let conflicts = reconcile_refused(&checkout, &id, "RECONCILE_CONFLICT");
    assert_eq!(
        conflicts,
        json!([{"subject": format!("{model}#Billing1"), "kind": "both-changed"}])
    );
    // Matching the current state resolves it.
    edited_proposal(&checkout, &id, set("Bills customers"));
    let (code, out) = reconcile(&checkout, &id);
    assert_eq!(code, 0, "{out:#}");
}

#[test]
fn reconcile_refuses_a_proposal_change_to_a_module_the_current_state_removed() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    edited_proposal(
        &checkout,
        &id,
        json!([{"type": "edit-module", "subject": "Web", "set": {"order": 5}}]),
    );
    checkout.edited(json!([{"type": "remove-module", "subject": "Web"}]));
    let conflicts = reconcile_refused(&checkout, &id, "RECONCILE_CONFLICT");
    assert_eq!(
        conflicts,
        json!([{"subject": format!("{model}#WebApp01"), "kind": "changed-removed"}])
    );
}

#[test]
fn reconcile_refuses_a_retired_id_the_proposal_still_holds() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    checkout.edited(json!([{"type": "remove-module", "subject": "Web"}]));
    let id = propose(&checkout, "Next", "current");
    checkout.write(
        &format!("proposals/{id}/modules/WebApp01.module.json"),
        &serde_json::to_string_pretty(&module("WebApp01", "Web", "app", 1)).unwrap(),
    );
    let conflicts = reconcile_refused(&checkout, &id, "RECONCILE_CONFLICT");
    assert_eq!(
        conflicts,
        json!([{"subject": format!("{model}#WebApp01"), "kind": "retired"}])
    );
}

#[test]
fn reconcile_refuses_a_draft_without_a_base_and_a_closed_proposal() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Old", "current");
    // A draft drawn before bases existed.
    let folder = checkout.vault().join("proposals").join(&id);
    let path = folder.join("proposal.json");
    let mut file: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    file.as_object_mut().unwrap().remove("base");
    fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();
    fs::remove_dir_all(folder.join("base")).unwrap();
    reconcile_refused(&checkout, &id, "RECONCILE_NO_BASE");
    // A copy of it has no base either.
    let copy = propose(&checkout, "Copy", &id);
    reconcile_refused(&checkout, &copy, "RECONCILE_NO_BASE");

    let shelved = propose(&checkout, "Shelved", "current");
    checkout.ok(&[
        "shelve",
        &shelved,
        "--expected-revision",
        &proposal_digest(&checkout, &shelved),
        "--doing",
        "set aside",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ]);
    reconcile_refused(&checkout, &shelved, "PROPOSAL_CLOSED");
}

/// The bundle `model export` prints with `args`.
fn export(checkout: &Checkout, args: &[&str]) -> Value {
    let mut all = vec!["export"];
    all.extend(args);
    let (code, out) = checkout.run(&all);
    assert_eq!(code, 0, "{out:#}");
    out
}

/// `model import` of `bundle` by tester: the exit code and output.
fn import(checkout: &Checkout, bundle: &Value) -> (i32, Value) {
    let path = checkout.path().join("in.bundle.json");
    fs::write(&path, bundle.to_string()).unwrap();
    checkout.run(&[
        "import",
        path.to_str().unwrap(),
        "--expected-revision",
        checkout.digest().as_str().unwrap(),
        "--doing",
        "bring in a bundle",
        "--writer",
        "tester",
        "--operation-id",
        &operation(),
    ])
}

/// The local ID of the draft `model import` of `bundle` drew.
fn imported(checkout: &Checkout, bundle: &Value) -> String {
    let (code, out) = import(checkout, bundle);
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(out["kind"], "import");
    local(out["allocated"][0]["id"].as_str().unwrap()).to_owned()
}

fn compares_equal(compared: &Value) -> bool {
    ["modules", "relationships", "collections", "flows", "steps"]
        .iter()
        .all(|section| {
            ["added", "removed", "changed"]
                .iter()
                .all(|list| compared[section][list] == json!([]))
        })
}

#[test]
fn an_exported_current_state_imports_as_a_draft_that_compares_equal() {
    let checkout = Checkout::new();
    let model = shop(&checkout);
    let before = checkout.digest();
    let bundle = export(&checkout, &[]);
    let (_, schema) = checkout.run(&["schema", "model-bundle"]);
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&bundle),
        "{bundle:#}"
    );
    assert_eq!(bundle["schema"], "boundrail/bundle/v1");
    assert_eq!(bundle["model"]["id"], model);
    assert_eq!(bundle["source"]["kind"], "current");
    assert_eq!(bundle["source"]["revision"], before);
    let text = bundle["files"]["modules/Billing1.module.json"]
        .as_str()
        .unwrap();
    assert_eq!(
        bundle["digests"]["modules/Billing1.module.json"],
        sha256(text.as_bytes())
    );

    let id = imported(&checkout, &bundle);
    assert_eq!(
        checkout.digest(),
        before,
        "import changed the current state"
    );
    assert!(compares_equal(&checkout.ok(&["compare", &id])));
    let read = checkout.ok(&["read", "--proposal", &id]);
    assert_eq!(read["proposal"]["status"], "draft");
    assert_eq!(read["proposal"]["base"], true);

    // --output writes the same bundle, never inside the vault or the Git folder.
    let out = checkout.ok(&["export", "--output", "shop.bundle.json"]);
    assert_eq!(out["kind"], "export");
    let written: Value = serde_json::from_str(
        &fs::read_to_string(checkout.path().join("shop.bundle.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written, bundle);
    for inside in [".boundrail/model/x.json", ".git/x.json"] {
        checkout.refused(&["export", "--output", inside], "REQUEST_INVALID");
    }
}

/// A committed shop and a clone of it.
fn clones() -> (Checkout, Checkout) {
    let first = committed();
    shop(&first);
    git(&first, &["add", "-A"]);
    git(&first, &["commit", "-q", "-m", "model"]);
    let second = Checkout::outside();
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(first.path())
        .arg(second.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    (first, second)
}

#[test]
fn an_exported_proposal_imports_into_another_clone_of_the_model() {
    let (first, second) = clones();

    let (id, _) = next(&first);
    let bundle = export(&first, &["--proposal", &id]);
    assert_eq!(bundle["source"]["kind"], "proposal");
    assert!(bundle["files"]["proposal.json"].is_string());
    assert!(bundle["files"]["base/modules/BankApi1.module.json"].is_string());

    let copy = imported(&second, &bundle);
    let read = |checkout: &Checkout, id: &str| names(&checkout.ok(&["read", "--proposal", id]));
    assert_eq!(read(&second, &copy), read(&first, &id));
    assert_eq!(
        second.ok(&["read", "--proposal", &copy])["proposal"]["name"],
        "Next"
    );
    let (code, out) = adopt(&second, &copy, &second.digest());
    assert_eq!(code, 0, "{out:#}");
}

#[test]
fn import_refuses_another_models_bundle_and_a_tampered_one() {
    let checkout = Checkout::new();
    shop(&checkout);
    let bundle = export(&checkout, &[]);
    let other = Checkout::new();
    other.init();
    let (code, out) = import(&other, &bundle);
    assert_eq!(code, 2, "{out:#}");
    assert_eq!(out["error"]["code"], "MODEL_MISMATCH");

    let before = checkout.digest();
    let mut tampered = bundle.clone();
    tampered["files"]["modules/Billing1.module.json"] = "{}".into();
    let (code, out) = import(&checkout, &tampered);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("BUNDLE_INVALID")),
        "{out:#}"
    );
    assert_eq!(
        out["error"]["details"]["path"],
        "modules/Billing1.module.json"
    );

    let mut unknown = bundle.clone();
    unknown["files"]["notes.txt"] = "hello".into();
    unknown["digests"]["notes.txt"] = sha256(b"hello").into();
    let (code, out) = import(&checkout, &unknown);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("BUNDLE_INVALID")),
        "{out:#}"
    );
    assert_eq!(out["error"]["details"]["path"], "notes.txt");
    assert_eq!(checkout.digest(), before);
    assert!(
        checkout.ok(&["proposals"])["proposals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// A bare repository for history tests.
fn bare() -> TempDir {
    let dir = TempDir::new("history-remote").unwrap();
    let out = Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    dir
}

/// A committed checkout with `remote` as its origin.
fn with_origin(remote: &TempDir) -> Checkout {
    let checkout = committed();
    git(
        &checkout,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    checkout
}

fn retain(checkout: &Checkout, doing: &str) -> String {
    let out = checkout.ok(&["retain", "--doing", doing, "--writer", "tester"]);
    out["checkpoint"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn a_pushed_history_fetched_into_another_clone_resolves_checkpoint_readings() {
    let remote = bare();
    let first = with_origin(&remote);
    shop(&first);
    let checkpoint = retain(&first, "the shop as drawn");
    let out = first.ok(&["history", "push"]);
    assert_eq!(out["kind"], "history-push");
    assert_eq!(out["updated"], true);
    assert_eq!(out["checkpoints"], json!([checkpoint]));
    assert_eq!(first.ok(&["history", "push"])["updated"], false);

    let second = with_origin(&remote);
    second.refused(
        &["read", "--checkpoint", &checkpoint],
        "CHECKPOINT_NOT_FOUND",
    );
    let out = second.ok(&["history", "fetch"]);
    assert_eq!(out["kind"], "history-fetch");
    assert_eq!(out["updated"], true);
    assert_eq!(out["checkpoints"], json!([checkpoint]));
    let read = second.ok(&["read", "--checkpoint", &checkpoint]);
    assert_eq!(names(&read), ["Bank", "Billing", "Platform", "Web"]);
    assert_eq!(second.ok(&["history", "fetch"])["updated"], false);
    // A fetched checkpoint exports without a vault in the checkout.
    let (code, bundle) = second.run(&["export", "--checkpoint", &checkpoint]);
    assert_eq!(code, 0, "{bundle:#}");
    assert_eq!(bundle["source"]["checkpoint"], checkpoint);
    assert!(bundle["files"]["modules/Billing1.module.json"].is_string());

    // A fetched history grows by fast-forward.
    first.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
    let later = retain(&first, "web moves");
    first.ok(&["history", "push"]);
    assert_eq!(
        second.ok(&["history", "fetch"])["checkpoints"],
        json!([later])
    );
}

/// Two clones of one committed vault with `remote` as origin, whose histories hold one
/// checkpoint the first retained and pushed, and its ID. The second's model is then edited, so it
/// has a change of its own to retain.
fn clones_on_one_checkpoint(remote: &TempDir) -> (Checkout, Checkout, String) {
    let (first, second) = clones();
    let url = remote.path().to_str().unwrap();
    git(&first, &["remote", "add", "origin", url]);
    git(&second, &["remote", "set-url", "origin", url]);
    for (key, value) in [
        ("user.name", "tester"),
        ("user.email", "tester@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(&second, &["config", key, value]);
    }
    let base = retain(&first, "the shop as drawn");
    first.ok(&["history", "push"]);
    assert_eq!(
        second.ok(&["history", "fetch"])["checkpoints"],
        json!([base])
    );
    second.edited(json!([{"type": "edit-module", "subject": "Bank", "set": {"order": 4}}]));
    (first, second, base)
}

/// `model retain` in `checkout` with the operation ID `local`: the checkpoint it returns.
fn retain_once(checkout: &Checkout) -> Value {
    let args = ["--doing", "a local change", "--writer", "tester"];
    let out = checkout.ok(&[&["retain"][..], &args, &["--operation-id", "local"]].concat());
    out["checkpoint"].clone()
}

/// The checkpoints `model checkpoints` lists in `checkout`, newest first.
fn checkpoints(checkout: &Checkout) -> Vec<Value> {
    checkout.ok(&["checkpoints"])["checkpoints"]
        .as_array()
        .unwrap()
        .clone()
}

/// The commit of the checkpoint `id` in `checkout`'s history.
fn checkpoint_commit(checkout: &Checkout, id: &str) -> String {
    let list = checkpoints(checkout);
    let found = list.iter().find(|c| c["id"] == id).unwrap();
    found["commit"].as_str().unwrap().to_owned()
}

fn history_tip(checkout: &Checkout) -> String {
    git(checkout, &["rev-parse", "refs/boundrail/history"])
}

/// The details of `checkout`'s refused push, whose message says `says`.
fn refused_push(checkout: &Checkout, says: &str) -> Value {
    let error = checkout.refused(&["history", "push"], "HISTORY_DIVERGED");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(says), "{error:#}");
    error["details"].clone()
}

#[test]
fn diverged_histories_refuse_to_push_and_replay_on_fetch() {
    let remote = bare();
    let (first, second, base) = clones_on_one_checkpoint(&remote);
    first.edited(json!([{"type": "edit-module", "subject": "Web", "set": {"order": 3}}]));
    let ahead = retain(&first, "web moves");
    first.ok(&["history", "push"]);
    assert_eq!(refused_push(&second, "is ahead")["local_only"], json!([]));

    let retained = retain_once(&second);
    let (local, original) = (&retained["id"], retained["commit"].as_str().unwrap());
    assert_eq!(
        refused_push(&second, "replays")["local_only"],
        json!([local])
    );
    assert_eq!(history_tip(&second).trim(), original);

    // The replayed checkpoint keeps its message, trailers, dates and tree.
    let format = ["log", "-1", "--format=%B%x1f%ad%x1f%cd%x1f%T"];
    let kept = git(&second, &[&format[..], &[original]].concat());
    let out = second.ok(&["history", "fetch"]);
    assert_eq!(out["checkpoints"], json!([ahead]));
    assert_eq!(out["replayed"], json!([local]));
    let replayed = checkpoint_commit(&second, local.as_str().unwrap());
    assert_ne!(replayed, original);
    assert_eq!(git(&second, &[&format[..], &[&replayed]].concat()), kept);
    let ids: Vec<Value> = checkpoints(&second)
        .iter()
        .map(|c| c["id"].clone())
        .collect();
    assert_eq!(ids, [local.clone(), json!(ahead), json!(base)]);
    // A retry of the retain returns the checkpoint as the history now holds it.
    assert_eq!(retain_once(&second)["commit"], replayed);
    // So does a retry whose record a crash left pending on the commit the fetch replayed.
    let record = second.path().join(".git/boundrail/operations/local.json");
    let pending = fs::read_to_string(&record)
        .unwrap()
        .replace(r#""status": "done""#, r#""status": "pending""#);
    assert!(pending.contains(original), "{pending}");
    assert!(pending.contains(r#""status": "pending""#), "{pending}");
    fs::write(&record, pending).unwrap();
    assert_eq!(retain_once(&second)["commit"], replayed);

    let out = second.ok(&["history", "push"]);
    assert_eq!(out["checkpoints"], json!([local]));
    let out = first.ok(&["history", "fetch"]);
    assert_eq!(out["checkpoints"], json!([local]));
    assert_eq!(out["replayed"], json!([]));
    for clone in [&first, &second] {
        for id in [&base, &ahead, local.as_str().unwrap()] {
            clone.ok(&["read", "--checkpoint", id]);
        }
    }
}

#[test]
fn fetch_refuses_to_replay_a_checkpoint_id_the_remote_holds() {
    let remote = bare();
    let (first, second, base) = clones_on_one_checkpoint(&remote);
    let local = retain(&second, "second clone's change");
    let tip = history_tip(&second);
    // The remote gains a checkpoint with the same ID in another case.
    let taken: String = (local.chars())
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let parent = checkpoint_commit(&first, &base);
    let tree = git(&first, &["rev-parse", &format!("{parent}^{{tree}}")]);
    let message =
        format!("taken\n\nCheckpoint: {taken}\nRevision: {FORGED_REVISION}\nWriter: tester\n");
    let forged = git(
        &first,
        &["commit-tree", tree.trim(), "-p", &parent, "-m", &message],
    );
    let refspec = format!("+{}:refs/boundrail/history", forged.trim());
    git(&first, &["push", "-q", "origin", &refspec]);

    let error = second.refused(&["history", "fetch"], "HISTORY_DIVERGED");
    assert_eq!(error["details"]["clashing"], json!([local]));
    assert_eq!(history_tip(&second), tip);
}

#[test]
fn histories_that_share_no_commit_refuse_to_push_or_fetch() {
    let remote = bare();
    let first = with_origin(&remote);
    shop(&first);
    retain(&first, "first clone's history");
    first.ok(&["history", "push"]);
    let second = with_origin(&remote);
    shop(&second);
    retain(&second, "second clone's history");
    let tip = history_tip(&second);
    assert_eq!(refused_push(&second, "share no commit")["related"], false);
    let error = second.refused(&["history", "fetch"], "HISTORY_DIVERGED");
    assert_eq!(error["details"]["related"], false);
    assert_eq!(history_tip(&second), tip);
}

#[test]
fn history_publication_refuses_a_merge_shaped_like_a_checkpoint() {
    let remote = bare();
    let checkout = with_origin(&remote);
    shop(&checkout);
    retain(&checkout, "drawn");
    checkout.ok(&["history", "push"]);
    let base = history_tip(&checkout).trim().to_owned();
    let tree = git(&checkout, &["rev-parse", &format!("{base}^{{tree}}")]);
    let message =
        format!("merged\n\nCheckpoint: Merge001\nRevision: {FORGED_REVISION}\nWriter: tester\n");
    let parents = ["-p", &base, "-p", "HEAD"];
    let merge = git(
        &checkout,
        &[
            &["commit-tree", tree.trim()][..],
            &parents,
            &["-m", &message],
        ]
        .concat(),
    );
    let merge = merge.trim();

    git(&checkout, &["update-ref", "refs/boundrail/history", merge]);
    let error = checkout.refused(&["history", "push"], "MODEL_INVALID");
    assert_eq!(error["details"]["object"], merge);
    let remote_tip = || git_in(remote.path(), &["rev-parse", "refs/boundrail/history"]);
    assert_eq!(remote_tip(), base);

    git(&checkout, &["update-ref", "refs/boundrail/history", &base]);
    let refspec = format!("+{merge}:refs/boundrail/history");
    git(&checkout, &["push", "-q", "origin", &refspec]);
    let error = checkout.refused(&["history", "fetch"], "MODEL_INVALID");
    assert_eq!(error["details"]["object"], merge);
    assert_eq!(history_tip(&checkout).trim(), base);
}

#[test]
fn a_configured_fetch_refspec_never_moves_the_history_during_a_fetch() {
    let remote = bare();
    let checkout = with_origin(&remote);
    shop(&checkout);
    retain(&checkout, "drawn");
    let tip = history_tip(&checkout);
    git(
        &checkout,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/boundrail/*:refs/boundrail/*",
        ],
    );
    // A code commit, not a checkpoint.
    let source = committed();
    let url = remote.path().to_str().unwrap();
    git(
        &source,
        &["push", "-q", url, "+HEAD:refs/boundrail/history"],
    );
    checkout.refused(&["history", "fetch"], "MODEL_INVALID");
    assert_eq!(history_tip(&checkout), tip);
    let refs = git(
        &checkout,
        &["for-each-ref", "--format=%(refname)", "refs/boundrail"],
    );
    assert_eq!(refs.trim(), "refs/boundrail/history");
}

#[test]
fn history_publication_takes_only_a_configured_remote_name() {
    let remote = bare();
    let checkout = with_origin(&remote);
    shop(&checkout);
    retain(&checkout, "drawn");
    let url = remote.path().to_str().unwrap().to_owned();
    for name in [
        "--remote=--upload-pack=touch pwned",
        "--remote=-o",
        &format!("--remote={url}"),
        "--remote=nope",
    ] {
        checkout.refused(&["history", "push", name], "REQUEST_INVALID");
        checkout.refused(&["history", "fetch", name], "REQUEST_INVALID");
    }
    assert!(!checkout.path().join("pwned").exists());
}

#[test]
fn an_imported_proposal_keeps_its_predecessor_so_reconcile_keeps_both_changes() {
    let (first, second) = clones();
    let id = propose(&first, "Next", "current");
    edited_proposal(
        &first,
        &id,
        json!([{"type": "edit-module", "subject": "Web", "set": {"name": "Site"}}]),
    );
    second.edited(json!([{"type": "edit-module", "subject": "Billing", "set": {"responsibility": "Bills customers"}}]));
    let copy = imported(&second, &export(&first, &["--proposal", &id]));
    let (code, out) = adopt(&second, &copy, &second.digest());
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("PREDECESSOR_MOVED")),
        "{out:#}"
    );
    let (code, out) = reconcile(&second, &copy);
    assert_eq!(code, 0, "{out:#}");
    let (code, out) = adopt(&second, &copy, &second.digest());
    assert_eq!(code, 0, "{out:#}");
    assert_eq!(
        names(&second.ok(&["read"])),
        ["Bank", "Billing", "Platform", "Site"]
    );
    let billing = second.ok(&["read", "Billing"]);
    assert_eq!(billing["module"]["responsibility"], "Bills customers");
}

/// `bundle` with `text` at `path` and its digest.
fn with_file(bundle: &Value, path: &str, text: &str) -> Value {
    let mut bundle = bundle.clone();
    bundle["files"][path] = text.into();
    bundle["digests"][path] = sha256(text.as_bytes()).into();
    bundle
}

#[test]
fn import_refuses_a_bundle_whose_base_or_paths_do_not_hold() {
    let checkout = Checkout::new();
    shop(&checkout);
    let id = propose(&checkout, "Next", "current");
    let bundle = export(&checkout, &["--proposal", &id]);
    let before = checkout.digest();
    let file = |id: &str, kind: &str| serde_json::to_string(&module(id, "E", kind, 0)).unwrap();
    let path = "base/modules/AAAAAAAA.module.json";
    for (path, text) in [
        (path, file("../../../evil", "service")),
        (path, file("ZZZZZZZZ", "service")),
        (
            "base/modules/ZZZZZZZZ.module.json",
            file("ZZZZZZZZ", "nonsense"),
        ),
        (path, "{not json".to_owned()),
    ] {
        let (code, out) = import(&checkout, &with_file(&bundle, path, &text));
        assert_eq!(
            (code, &out["error"]["code"]),
            (2, &json!("BUNDLE_INVALID")),
            "{out:#}"
        );
        assert_eq!(out["error"]["details"]["path"], path, "{out:#}");
    }
    // A current-state path must name a local ID: a backslash is a separator on some hosts.
    let current = export(&checkout, &[]);
    let escape = "modules/..\\..\\evil.module.json";
    let (code, out) = import(
        &checkout,
        &with_file(&current, escape, &file("ZZZZZZZZ", "service")),
    );
    assert_eq!(
        (
            code,
            &out["error"]["code"],
            &out["error"]["details"]["path"]
        ),
        (2, &json!("BUNDLE_INVALID"), &json!(escape)),
        "{out:#}"
    );
    // Paths differing only in case would land on one file on some hosts.
    let shadow = with_file(
        &current,
        "modules/BILLING1.module.json",
        &file("BILLING1", "service"),
    );
    let (code, out) = import(&checkout, &shadow);
    assert_eq!(
        (code, &out["error"]["code"]),
        (2, &json!("BUNDLE_INVALID")),
        "{out:#}"
    );
    assert_eq!(checkout.digest(), before);
    assert_eq!(
        checkout.ok(&["proposals"])["proposals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn bundle_refusals_never_print_the_bundles_own_text() {
    let checkout = Checkout::new();
    shop(&checkout);
    let escape = "\u{1b}]0;pwned\u{7}\u{1b}[31m";
    let file = checkout.path().join("e.bundle.json");
    let file = file.to_str().unwrap();
    let digest = checkout.digest();
    let digest = digest.as_str().unwrap();
    let args = |operation: &'static str| {
        let doing = ["--doing", "bring it in", "--writer", "tester"];
        let rest = ["--expected-revision", digest, "--operation-id", operation];
        [&["import", file][..], &doing, &rest].concat()
    };
    let markdown = |operation| {
        let (code, stdout) =
            checkout.run_raw(&[&["--format", "markdown"][..], &args(operation)].concat());
        assert_eq!(code, 2, "{stdout}");
        assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
        stdout
    };
    let bundle = with_file(&export(&checkout, &[]), &format!("{escape}modules/x"), "x");
    fs::write(file, bundle.to_string()).unwrap();
    assert!(markdown("escape").starts_with("BUNDLE_INVALID: "));

    // An unknown source kind, which serde quotes, refuses with a fixed message.
    let mut unknown = export(&checkout, &[]);
    unknown["source"]["kind"] = format!("{escape}kind").into();
    fs::write(file, unknown.to_string()).unwrap();
    let message = "REQUEST_INVALID: the bundle does not parse as boundrail/bundle/v1";
    assert_eq!(markdown("escape-md").lines().next(), Some(message));
    let details = &checkout.refused(&args("escape-kind"), "REQUEST_INVALID")["details"];
    let error = details["error"].as_str().unwrap();
    assert!(
        error.contains("unknown variant") && !error.contains('\u{1b}'),
        "{error}"
    );
    assert!(details["line"].as_u64().unwrap() >= 1);
    assert!(details["column"].as_u64().unwrap() >= 1);
}

#[test]
fn export_never_writes_into_the_shared_git_folder_from_a_linked_worktree() {
    let checkout = committed();
    shop(&checkout);
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "model"]);
    let linked = worktree(&checkout);
    let exclude = checkout.path().join(".git/info/exclude");
    let kept = fs::read(&exclude).unwrap();
    linked.refused(
        &["export", "--output", exclude.to_str().unwrap()],
        "REQUEST_INVALID",
    );
    assert_eq!(fs::read(&exclude).unwrap(), kept);
}

/// `git` with `args` in the folder `dir`: its trimmed standard output.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn fetch_refuses_a_remote_history_that_retain_never_wrote() {
    let remote = bare();
    let checkout = with_origin(&remote);
    shop(&checkout);
    let local_refs = || git(&checkout, &["for-each-ref", "refs/boundrail"]);
    let blob = checkout.path().join("blob.txt");
    fs::write(&blob, "hi").unwrap();
    let object = git_in(
        remote.path(),
        &["hash-object", "-w", blob.to_str().unwrap()],
    );
    git_in(
        remote.path(),
        &["update-ref", "refs/boundrail/history", &object],
    );
    checkout.refused(&["history", "fetch"], "MODEL_INVALID");
    assert_eq!(local_refs(), "");

    // A code commit, not a checkpoint.
    let source = committed();
    let url = remote.path().to_str().unwrap();
    git(
        &source,
        &["push", "-q", url, "+HEAD:refs/boundrail/history"],
    );
    let error = checkout.refused(&["history", "fetch"], "MODEL_INVALID");
    assert_eq!(
        error["details"]["object"],
        git(&source, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(local_refs(), "");
    retain(&checkout, "still retains");
}

#[test]
fn fetch_refuses_remote_checkpoint_trailers_retain_never_writes() {
    let remote = bare();
    let checkout = with_origin(&remote);
    shop(&checkout);
    let base = retain(&checkout, "drawn");
    checkout.ok(&["history", "push"]);
    let tip = history_tip(&checkout);
    let tree = git(
        &checkout,
        &["rev-parse", &format!("{}^{{tree}}", tip.trim())],
    );
    let trailers = |id: &str, revision: &str| {
        format!("Checkpoint: {id}\nRevision: {revision}\nWriter: tester\n")
    };
    let cases = [
        format!("escaped\n\n{}", trailers("Esc00001", "\u{1b}[31mred")),
        format!(
            "bad source\n\n{}Source: main\n",
            trailers("Src00001", FORGED_REVISION)
        ),
        format!("malformed\n\n{}", trailers("bad/id01", FORGED_REVISION)),
        format!("repeated\n\n{}", trailers(&base, FORGED_REVISION)),
        format!(
            "\u{1b}[2Jcleared\n\n{}",
            trailers("Doing001", FORGED_REVISION)
        ),
    ];
    for message in cases {
        let args = ["commit-tree", tree.trim(), "-p", tip.trim(), "-m", &message];
        let forged = git(&checkout, &args);
        let refspec = format!("+{}:refs/boundrail/history", forged.trim());
        git(&checkout, &["push", "-q", "origin", &refspec]);
        let error = checkout.refused(&["history", "fetch"], "MODEL_INVALID");
        assert_eq!(error["details"]["object"], forged.trim(), "{message:?}");
        assert_eq!(history_tip(&checkout), tip, "{message:?}");
    }
}

#[test]
fn checkpoints_in_markdown_escape_control_characters_from_history_text() {
    let checkout = committed();
    shop(&checkout);
    checkout.ok(&["retain", "--doing", "drawn"]);
    let tree = git(&checkout, &["rev-parse", "refs/boundrail/history^{tree}"]);
    let message = format!(
        "\u{1b}[2J\"quoted\" C:\\path\n\nCheckpoint: Ctrl0001\nRevision: {FORGED_REVISION}\nWriter: a\u{7}b\n"
    );
    let args = [
        "commit-tree",
        tree.trim(),
        "-p",
        "refs/boundrail/history",
        "-m",
        &message,
    ];
    let commit = git(&checkout, &args);
    git(
        &checkout,
        &["update-ref", "refs/boundrail/history", commit.trim()],
    );
    let (code, text) = checkout.run_raw(&["checkpoints", "--format", "markdown"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
    assert!(text.contains(r#"by a\u{7}b at"#), "{text}");
    assert!(text.contains(r#": \u{1b}[2J"quoted" C:\path"#), "{text}");
}
