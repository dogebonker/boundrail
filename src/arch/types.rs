//! The vault's file formats. The same serde types produce the JSON Schemas `model schema` prints.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;
pub const RESPONSIBILITY_MAX: usize = 200;
pub const DESCRIPTION_MAX: usize = 2000;
pub const LABEL_MAX: usize = 60;
const LOCAL_ID: &str = "^[A-Za-z0-9]{8}$";
const QUALIFIED_ID: &str = "^[A-Za-z0-9]{8}#[A-Za-z0-9]{8}$";

/// `model.json`: the model's identity and its current state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    pub current: State,
    /// Local IDs that were removed and may never be allocated again.
    #[serde(default)]
    #[schemars(inner(pattern(LOCAL_ID)))]
    pub retired_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
}

/// `config.yaml`: the vocabulary modules and relationships draw their kinds from.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VaultConfig {
    pub node_kinds: BTreeMap<String, NodeKind>,
    pub relationship_kinds: BTreeMap<String, RelationshipKind>,
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeKind {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RelationshipKind {
    pub name: String,
    pub dash: Dash,
    /// Code evidence that can support a relationship of this kind; empty means never judged.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Group {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Dash {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Evidence {
    Call,
    Import,
    ConfiguredPath,
    BuildDependency,
}

/// `modules/<id>.module.json`: one module and the relationships it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Module {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    pub kind: String,
    pub order: u32,
    /// Qualified ID of the containing module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(pattern(QUALIFIED_ID))]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = RESPONSIBILITY_MAX))]
    pub responsibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = DESCRIPTION_MAX))]
    pub description: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
    #[serde(default)]
    pub former_names: Vec<String>,
    #[serde(default)]
    pub outgoing: Vec<Relationship>,
}

/// The code a module stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    /// Qualified ID of the target module.
    #[schemars(pattern(QUALIFIED_ID))]
    pub to: String,
    pub kind: String,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = LABEL_MAX))]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = DESCRIPTION_MAX))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<Emphasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Emphasis {
    Normal,
    Hero,
    Muted,
}

/// The code site that carries a relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub path: String,
    pub symbol: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        use Evidence::{BuildDependency, Call, ConfiguredPath, Import};
        let node = |name: &str| NodeKind {
            name: name.to_owned(),
            color: None,
        };
        let relationship = |name: &str, dash, evidence: &[Evidence]| RelationshipKind {
            name: name.to_owned(),
            dash,
            evidence: evidence.to_vec(),
        };
        let node_kinds = [
            ("system", "System"),
            ("service", "Service"),
            ("app", "App"),
            ("module", "Module"),
            ("function", "Function"),
            ("route", "Route"),
            ("job", "Job"),
            ("queue", "Queue"),
            ("datastore", "Datastore"),
            ("cache", "Cache"),
            ("external", "External"),
            ("ui", "UI"),
            ("config", "Config"),
            ("test", "Test"),
            ("package", "Package"),
            ("other", "Other"),
        ]
        .into_iter()
        .map(|(key, name)| (key.to_owned(), node(name)))
        .collect();
        let configured = [Call, ConfiguredPath].as_slice();
        let relationship_kinds = [
            ("call", relationship("Call", Dash::Solid, &[Call])),
            ("http", relationship("HTTP", Dash::Solid, configured)),
            ("rpc", relationship("RPC", Dash::Solid, configured)),
            ("event", relationship("Event", Dash::Dashed, configured)),
            ("queue", relationship("Queue", Dash::Dashed, configured)),
            ("data", relationship("Data", Dash::Dotted, configured)),
            (
                "dependency",
                relationship(
                    "Dependency",
                    Dash::Solid,
                    &[Call, ConfiguredPath, Import, BuildDependency],
                ),
            ),
            ("render", relationship("Render", Dash::Solid, &[Call])),
            ("other", relationship("Other", Dash::Dotted, &[])),
        ]
        .into_iter()
        .map(|(key, kind)| (key.to_owned(), kind))
        .collect();
        Self {
            node_kinds,
            relationship_kinds,
            groups: BTreeMap::new(),
        }
    }
}

/// Builds one vault file format's JSON Schema.
pub type SchemaFn = fn() -> Schema;

/// Every vault file format and the edit request, by its `model schema` name.
pub const SCHEMAS: [(&str, SchemaFn); 4] = [
    ("model-edit", root::<super::edit::EditRequest>),
    ("model-manifest", root::<Manifest>),
    ("module", root::<Module>),
    ("vault-config", root::<VaultConfig>),
];

fn root<T: JsonSchema>() -> Schema {
    schemars::schema_for!(T)
}

/// The JSON Schema for one vault file format, by its `model schema` name.
pub fn schema(name: &str) -> Option<Value> {
    SCHEMAS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, schema)| schema().to_value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_id_pattern_agrees_with_the_id_check() {
        let validator =
            jsonschema::validator_for(&serde_json::json!({"type": "string", "pattern": LOCAL_ID}))
                .unwrap();
        for sample in [
            "abcDEF12",
            "00000000",
            "abcDEF1",
            "abcDEF123",
            "abcDEF1_",
            "abcdéf12",
            "",
        ] {
            assert_eq!(
                validator.is_valid(&Value::from(sample)),
                crate::arch::ids::is_local(sample),
                "{sample}"
            );
        }
    }
}
