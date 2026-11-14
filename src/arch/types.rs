//! The vault's file formats. The same serde types produce the JSON Schemas `model schema` prints.

use std::collections::BTreeMap;
use std::iter;

use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;
pub const RESPONSIBILITY_MAX: usize = 200;
pub const DESCRIPTION_MAX: usize = 2000;
pub const LABEL_MAX: usize = 60;
/// The module kind for code outside the checkout, which the code check never judges.
pub const EXTERNAL_KIND: &str = "external";
const LOCAL_ID: &str = "^[A-Za-z0-9]{8}$";
const QUALIFIED_ID: &str = "^[A-Za-z0-9]{8}#[A-Za-z0-9]{8}$";
/// A path relative to the repository root, as `vault::is_repo_path` checks it: no leading `/`, no
/// `..` segment, no backslash, and no `:` in the first segment.
const REPO_PATH: &str = r"^(?:[^/\\.:][^/\\:]*|\.(?:[^/\\.:][^/\\:]*)?|\.\.[^/\\:]+)(?:/(?:[^/\\.][^/\\]*|\.(?:[^/\\.][^/\\]*)?|\.\.[^/\\]+)?)*$";

/// `model.json`: the model's identity and its current state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = require_current)]
pub struct Manifest {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    /// `null` in a planning model, which has no current state until a proposal is adopted. The
    /// key is required, so a manifest that lost it never reads as a planning model.
    #[serde(deserialize_with = "present")]
    pub current: Option<State>,
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

/// `proposals/<id>/proposal.json`: a draft of a future architecture, drawn against the current
/// state it would replace. Its modules and collections sit beside it in the current state's formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = DESCRIPTION_MAX))]
    pub description: Option<String>,
    pub status: ProposalStatus,
    pub predecessor: Predecessor,
    /// When it was drawn, in Unix seconds.
    pub created: u64,
    /// The local ID of the current state its adoption made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(pattern(LOCAL_ID))]
    pub adopted_as: Option<String>,
    /// Local IDs this proposal removed that it did not add in the same edit, sorted. Edits
    /// inside it never allocate them again; adoption retires what the adopted state drops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(inner(pattern(LOCAL_ID)))]
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Draft,
    Adopted,
    Shelved,
}

/// Lists `current` as required in the manifest's schema, as [`present`] reads it.
fn require_current(schema: &mut Schema) {
    if let Some(Value::Array(required)) = schema.get_mut("required") {
        required.push("current".into());
    }
}

/// An `Option` whose key must be present, `null` allowed: a plain `Option` field reads a missing
/// key as `None`, and `deserialize_with` turns that off.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

/// The current state a proposal was drawn against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Predecessor {
    /// The current state's local ID; `null` when drawn in a planning model.
    #[schemars(pattern(LOCAL_ID))]
    pub state: Option<String>,
    /// The current state's revision digest when the proposal was drawn.
    pub revision: String,
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Evidence {
    Call,
    Import,
    ConfiguredPath,
    BuildDependency,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Import => "import",
            Self::ConfiguredPath => "configured-path",
            Self::BuildDependency => "build-dependency",
        }
    }
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
    #[schemars(pattern(REPO_PATH))]
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
    #[schemars(pattern(REPO_PATH))]
    pub path: String,
    pub symbol: String,
}

/// The longest flow summary.
pub const SUMMARY_MAX: usize = 200;
/// How many decisions and loops may nest inside each other.
pub const NESTING_MAX: usize = 4;
/// The most steps one flow holds, decisions and loops included.
pub const FLOW_STEPS_MAX: usize = 500;
/// The least `repeat` of a message: once is the default.
pub const REPEAT_MIN: u32 = 2;
/// The fewest branches a decision has.
pub const BRANCHES_MIN: usize = 2;

/// `collections/<id>.collection.json`: a named group of flows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = DESCRIPTION_MAX))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
    #[serde(default)]
    pub flows: Vec<Flow>,
}

/// An authored exchange between modules: steps in order that read as a sequence diagram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    pub name: String,
    pub order: u32,
    /// One line saying what the flow does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = SUMMARY_MAX))]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = DESCRIPTION_MAX))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One step of a flow. Decisions and loops hold steps of their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// One module sends a message to another.
    Message {
        #[schemars(pattern(LOCAL_ID))]
        id: String,
        /// Qualified ID of the sending module.
        #[schemars(pattern(QUALIFIED_ID))]
        from: String,
        /// Qualified ID of the receiving module.
        #[schemars(pattern(QUALIFIED_ID))]
        to: String,
        #[schemars(length(min = 1, max = LABEL_MAX))]
        label: String,
        /// Qualified ID of the relationship that carries the message: from `from` to `to`, or
        /// from `to` to `from` for a reply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(pattern(QUALIFIED_ID))]
        relationship: Option<String>,
        /// The message answers an earlier one.
        #[serde(default, skip_serializing_if = "is_false")]
        reply: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emphasis: Option<Emphasis>,
        /// The exchange happens this many times.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = REPEAT_MIN))]
        repeat: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = DESCRIPTION_MAX))]
        note: Option<String>,
    },
    /// The flow takes one of two or more branches.
    Decision {
        #[schemars(pattern(LOCAL_ID))]
        id: String,
        #[schemars(length(min = 1, max = LABEL_MAX))]
        label: String,
        #[schemars(length(min = BRANCHES_MIN))]
        branches: Vec<Branch>,
    },
    /// The steps inside repeat.
    Loop {
        #[schemars(pattern(LOCAL_ID))]
        id: String,
        #[schemars(length(min = 1, max = LABEL_MAX))]
        label: String,
        #[serde(default)]
        steps: Vec<Step>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    #[schemars(pattern(LOCAL_ID))]
    pub id: String,
    #[schemars(length(min = 1, max = LABEL_MAX))]
    pub label: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Step {
    pub fn id(&self) -> &str {
        match self {
            Self::Message { id, .. } | Self::Decision { id, .. } | Self::Loop { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Message { label, .. }
            | Self::Decision { label, .. }
            | Self::Loop { label, .. } => label,
        }
    }

    /// The step lists directly inside this step, each with the ID that names it as a parent: a
    /// loop's own list by the loop's ID, a decision's branches by their IDs.
    pub fn lists(&self) -> impl DoubleEndedIterator<Item = (&str, &[Step])> {
        let (branches, own): (&[Branch], _) = match self {
            Self::Message { .. } => (&[], None),
            Self::Decision { branches, .. } => (branches, None),
            Self::Loop { id, steps, .. } => (&[], Some((id.as_str(), steps.as_slice()))),
        };
        branches
            .iter()
            .map(|b| (b.id.as_str(), b.steps.as_slice()))
            .chain(own)
    }

    /// [`Step::lists`], mutable.
    pub fn lists_mut(&mut self) -> impl Iterator<Item = (&str, &mut Vec<Step>)> {
        let (branches, own): (&mut [Branch], _) = match self {
            Self::Message { .. } => (&mut [], None),
            Self::Decision { branches, .. } => (branches, None),
            Self::Loop { id, steps, .. } => (&mut [], Some((id.as_str(), steps))),
        };
        branches
            .iter_mut()
            .map(|Branch { id, steps, .. }| (id.as_str(), steps))
            .chain(own)
    }

    /// Whether `id` names this step or a step, branch or loop inside it.
    pub fn holds(&self, id: &str) -> bool {
        self.id() == id
            || self
                .lists()
                .any(|(list, steps)| list == id || steps.iter().any(|s| s.holds(id)))
    }
}

/// The first step at any depth of `steps`, in reading order, that `wanted` accepts.
pub fn find_step<'a>(steps: &'a [Step], wanted: &impl Fn(&Step) -> bool) -> Option<&'a Step> {
    steps.iter().find_map(|step| {
        if wanted(step) {
            return Some(step);
        }
        step.lists().find_map(|(_, list)| find_step(list, wanted))
    })
}

/// Every step of `steps` at any depth in reading order, each with the number of decisions and
/// loops around it.
pub fn walk(steps: &[Step]) -> Vec<(&Step, usize)> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Step, usize)> = steps.iter().rev().map(|s| (s, 0)).collect();
    while let Some((step, depth)) = stack.pop() {
        out.push((step, depth));
        for (_, list) in step.lists().rev() {
            stack.extend(list.iter().rev().map(|s| (s, depth + 1)));
        }
    }
    out
}

impl Flow {
    /// The flow's ID and those of its steps and branches.
    pub fn ids(&self) -> Vec<&str> {
        let mut ids = vec![self.id.as_str()];
        for (step, _) in walk(&self.steps) {
            ids.push(step.id());
            if let Step::Decision { branches, .. } = step {
                ids.extend(branches.iter().map(|b| b.id.as_str()));
            }
        }
        ids
    }
}

impl Collection {
    /// The collection's ID and those of its flows, steps and branches.
    pub fn ids(&self) -> Vec<&str> {
        iter::once(self.id.as_str())
            .chain(self.flows.iter().flat_map(Flow::ids))
            .collect()
    }
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
            (EXTERNAL_KIND, "External"),
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

/// Every vault file format and request, by its `model schema` name.
pub const SCHEMAS: [(&str, SchemaFn); 8] = [
    ("model-claim", root::<super::claims::ClaimRequest>),
    ("model-collection", root::<Collection>),
    ("model-edit", root::<super::edit::EditRequest>),
    ("model-manifest", root::<Manifest>),
    ("model-proposal", root::<Proposal>),
    ("model-release", root::<super::claims::ReleaseRequest>),
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

    #[test]
    fn repo_path_pattern_agrees_with_the_path_check() {
        let validator =
            jsonschema::validator_for(&serde_json::json!({"type": "string", "pattern": REPO_PATH}))
                .unwrap();
        for sample in [
            "src",
            "src/a.ts",
            "./src",
            ".hidden/x",
            "...",
            "..x/y",
            "a//b",
            "a/./b",
            "a/b:c",
            "src/",
            "",
            "/etc",
            "..",
            "../a",
            "a/../b",
            "a/..",
            "c:/x",
            "C:",
            "http://x",
            "a\\b",
            "\\\\server\\share",
            "a/b\\c",
        ] {
            assert_eq!(
                validator.is_valid(&Value::from(sample)),
                crate::arch::vault::is_repo_path(sample),
                "{sample}"
            );
        }
    }
}
