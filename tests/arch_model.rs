use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

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
            .arg("--no-daemon")
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
            .args(["--no-daemon", "model"])
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
            .args(["--no-daemon", "model", "edit", "--doing", doing])
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
    let cases: [(&str, Break, &str, &str, &str); 14] = [
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
            "model-claim",
            "model-edit",
            "model-manifest",
            "model-release",
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
        .args(["--no-daemon", "model", "read"])
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
            .args(["--no-daemon", "model"])
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
        .args(["--no-daemon", "model", "recover"])
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
