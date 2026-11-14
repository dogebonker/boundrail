use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let cases: [(&str, Break, &str, &str, &str); 12] = [
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
        json!(["model-manifest", "module", "vault-config"])
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
    let (_, schema) = checkout.run(&["schema", "module"]);
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut bad = read_json("modules/Billing1.module.json");
    bad["responsibility"] = "x".repeat(201).into();
    assert!(!validator.is_valid(&bad));
    bad["responsibility"] = "ok".into();
    bad["surprise"] = true.into();
    assert!(!validator.is_valid(&bad));
}
