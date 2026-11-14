//! `model edit`: one batch of module and relationship changes, applied in order in memory and
//! validated as a whole, then written through a journal under the edit lock so an interrupted
//! write can be completed and every operation replays its receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::claims;
use super::request::{Target, WRITER_MAX, check_doing, check_model, check_writer, invalid, parse};
use super::revision::Revision;
use super::state::{self, Locked, OPERATIONS};
use super::types::{Anchor, Binding, Emphasis, Module, Relationship, SCHEMA_VERSION};
use super::vault::{self, FileWrite, Model, relationships};
use super::{Code, Refusal, ids};
use crate::history::refuse_symlink;
use crate::sync_rules::hash;

const CHANGES_MAX: usize = 1000;
const OPERATION_MAX: usize = 64;
/// Debug builds stop after writing the journal when this is set, so tests can simulate a crash.
#[cfg(debug_assertions)]
const STOP_AFTER_JOURNAL: &str = "BOUNDRAIL_TEST_STOP_AFTER_JOURNAL";

/// The `model edit` request: changes to the current state, applied in order as one batch.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub target: Target,
    /// The revision digest the changes were written against.
    pub expected_revision: String,
    /// Repeating an operation ID with the same request replays its receipt.
    #[schemars(length(min = 1, max = OPERATION_MAX), pattern("^[A-Za-z0-9._-]+$"))]
    pub operation_id: String,
    #[schemars(length(min = 1, max = WRITER_MAX))]
    pub writer: String,
    #[schemars(length(min = 1, max = CHANGES_MAX))]
    pub changes: Vec<Change>,
    /// The writer's claim covering every subject the edit touches. Without one, the edit may not
    /// touch a subject that another writer's active claim holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
}

/// One change. A module subject or reference is a qualified ID, a local ID, or a unique current
/// or former name, modules added earlier in the batch included; a relationship subject is a
/// qualified or local relationship ID.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Change {
    AddModule {
        module: NewModule,
    },
    EditModule {
        subject: String,
        #[serde(default)]
        set: ModuleSet,
        #[serde(default)]
        clear: Vec<ModuleField>,
    },
    /// Removes the module and the relationships touching it that the batch does not move.
    RemoveModule {
        subject: String,
    },
    AddRelationship {
        source: String,
        relationship: NewRelationship,
    },
    EditRelationship {
        subject: String,
        /// Moves the relationship, keeping its ID, to this module's outgoing relationships.
        source: Option<String>,
        #[serde(default)]
        set: RelationshipSet,
        #[serde(default)]
        clear: Vec<RelationshipField>,
    },
    RemoveRelationship {
        subject: String,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewModule {
    pub name: String,
    pub kind: String,
    pub order: u32,
    pub parent: Option<String>,
    pub responsibility: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    pub binding: Option<Binding>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModuleSet {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub order: Option<u32>,
    pub parent: Option<String>,
    pub responsibility: Option<String>,
    pub description: Option<String>,
    pub groups: Option<Vec<String>>,
    pub binding: Option<Binding>,
}

/// A module field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModuleField {
    Parent,
    Responsibility,
    Description,
    Groups,
    Binding,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRelationship {
    pub to: String,
    pub kind: String,
    pub order: u32,
    pub label: Option<String>,
    pub description: Option<String>,
    pub emphasis: Option<Emphasis>,
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipSet {
    pub to: Option<String>,
    pub kind: Option<String>,
    pub order: Option<u32>,
    pub label: Option<String>,
    pub description: Option<String>,
    pub emphasis: Option<Emphasis>,
    pub anchor: Option<Anchor>,
}

/// A relationship field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipField {
    Label,
    Description,
    Emphasis,
    Anchor,
}

impl ModuleField {
    fn is_set(self, set: &ModuleSet) -> bool {
        match self {
            Self::Parent => set.parent.is_some(),
            Self::Responsibility => set.responsibility.is_some(),
            Self::Description => set.description.is_some(),
            Self::Groups => set.groups.is_some(),
            Self::Binding => set.binding.is_some(),
        }
    }
}

impl RelationshipField {
    fn is_set(self, set: &RelationshipSet) -> bool {
        match self {
            Self::Label => set.label.is_some(),
            Self::Description => set.description.is_some(),
            Self::Emphasis => set.emphasis.is_some(),
            Self::Anchor => set.anchor.is_some(),
        }
    }
}

/// What an edit did; `model edit` prints it inside the output envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub operation_id: String,
    pub writer: String,
    pub doing: String,
    pub before: Revision,
    pub after: Revision,
    pub subjects: Subjects,
    pub allocated: Vec<Allocation>,
}

/// Qualified IDs of the modules and relationships the edit added, changed or removed. A module
/// counts as changed when its file changed, including its outgoing relationships.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Subjects {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Allocation {
    pub name: String,
    pub id: String,
}

/// A journal record, `<git dir>/boundrail/operations/<operation_id>.json`. A pending record
/// holds every vault write, so a replay or `model recover` can finish an interrupted edit; a
/// done record keeps only the receipt.
#[derive(Serialize, Deserialize)]
struct Record {
    /// SHA-256 of the canonical request; `--doing` is not part of it.
    request_sha256: String,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writes: Option<Vec<FileWrite>>,
    output: Receipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Done,
}

/// Applies `input`, an [`EditRequest`] as JSON, to the vault folder `vault`. A repeated
/// operation returns its stored receipt, finishing it first when it was interrupted.
pub fn run(vault: &Path, input: &[u8], doing: &str) -> Result<Receipt> {
    check_doing(doing)?;
    let request: EditRequest = parse(input, "edit")?;
    check_request(&request)?;
    let fingerprint = hash(&serde_json::to_vec(&request)?);
    let journal = Journal::open(vault)?;
    if let Some(record) = journal.read(&request.operation_id)? {
        if record.request_sha256 != fingerprint {
            return Err(Refusal::new(
                Code::OperationReused,
                "this operation_id already applied a different request; use a new operation_id",
            )
            .with_details(json!({ "operation_id": request.operation_id }))
            .into());
        }
        return match record.status {
            Status::Done => Ok(record.output),
            Status::Pending => complete(vault, &journal, record),
        };
    }
    state::refuse_interrupted(&journal.state.dir)?;
    let model = vault::load_dir(vault.to_path_buf())?;
    check_target(&model, &request)?;
    let claim = request.claim.clone();
    let (record, affected) = plan(&model, request, doing, fingerprint)?;
    claims::check_edit(
        &journal.state.dir,
        &model,
        &record.output.writer,
        claim.as_deref(),
        &affected,
    )?;
    journal.write(&record)?;
    #[cfg(debug_assertions)]
    if std::env::var_os(STOP_AFTER_JOURNAL).is_some() {
        anyhow::bail!("stopped after writing the journal because {STOP_AFTER_JOURNAL} is set");
    }
    complete(vault, &journal, record)
}

/// Finishes every interrupted edit of the vault folder `vault` and returns their receipts.
pub fn recover(vault: &Path) -> Result<Vec<Receipt>> {
    let journal = Journal::open(vault)?;
    journal
        .pending()?
        .into_iter()
        .map(|record| complete(vault, &journal, record))
        .collect()
}

fn check_request(request: &EditRequest) -> Result<()> {
    let operation = &request.operation_id;
    let problem = if operation.is_empty()
        || operation.len() > OPERATION_MAX
        || !operation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        format!(
            "operation_id needs 1 to {OPERATION_MAX} characters from A-Z, a-z, 0-9, `.`, `_`, `-`"
        )
    } else if request.changes.is_empty() || request.changes.len() > CHANGES_MAX {
        format!("changes needs 1 to {CHANGES_MAX} entries")
    } else {
        return check_writer(&request.writer);
    };
    Err(invalid(problem))
}

fn check_target(model: &Model, request: &EditRequest) -> Result<()> {
    check_model(model, &request.target)?;
    if request.expected_revision != model.revision.digest {
        return Err(Refusal::new(
            Code::StaleRevision,
            "the model changed since the expected revision; read it again and rebase the changes",
        )
        .with_details(json!({ "current": model.revision.digest }))
        .into());
    }
    Ok(())
}

/// Applies the batch in memory, validates the result and plans its writes as a pending record,
/// returned with the subjects a claim must cover.
fn plan(
    model: &Model,
    request: EditRequest,
    doing: &str,
    fingerprint: String,
) -> Result<(Record, BTreeSet<String>)> {
    let EditRequest {
        operation_id,
        writer,
        changes,
        ..
    } = request;
    let mut batch = Batch::new(model);
    for (index, change) in changes.into_iter().enumerate() {
        batch.apply(index, change)?;
    }
    batch.finish()?;
    let diff = diff(model, &batch.modules, &batch.fresh);

    let mut manifest = model.manifest.clone();
    manifest.retired_ids.extend(diff.retired.iter().cloned());
    let mut problems = vault::problems(&manifest, &model.config, &batch.modules);
    if !problems.is_empty() {
        for problem in &mut problems {
            problem.change = batch.touched.get(&problem.subject).copied();
        }
        return Err(vault::invalid(&model.dir, problems));
    }

    let manifest = (!diff.retired.is_empty()).then_some(&manifest);
    let writes = vault::plan_writes(manifest, &diff.write, &diff.remove)?;
    let after = model.revision.updated(
        writes
            .iter()
            .map(|w| (w.path.as_str(), w.content.as_deref().map(str::as_bytes))),
    );
    let record = Record {
        request_sha256: fingerprint,
        status: Status::Pending,
        before: Some(model.revision.digest.clone()),
        after: Some(after.digest.clone()),
        writes: Some(writes),
        output: Receipt {
            operation_id,
            writer,
            doing: doing.to_owned(),
            before: model.revision.clone(),
            after,
            subjects: diff.subjects,
            allocated: batch.allocated,
        },
    };
    Ok((record, diff.affected))
}

/// Applies a pending record's writes, checks the vault reached its revision and marks it done.
/// Every failure here may follow a vault write, so it is an error rather than a refusal.
fn complete(vault: &Path, journal: &Journal, mut record: Record) -> Result<Receipt> {
    let operation = record.output.operation_id.clone();
    let finished = (|| -> Result<()> {
        vault::apply_writes(vault, record.writes.as_deref().unwrap_or_default())?;
        let digest = vault::load_dir(vault.to_path_buf())?.revision.digest;
        let planned = &record.output.after.digest;
        ensure!(
            digest == *planned,
            "the vault revision is {digest} after writing, not the planned {planned}"
        );
        record.status = Status::Done;
        record.before = None;
        record.after = None;
        record.writes = None;
        journal.write(&record)
    })();
    finished.map_err(|e| {
        anyhow!("{e:#}").context(format!(
            "operation {operation} is not complete; run `boundrail model recover` to finish it"
        ))
    })?;
    Ok(record.output)
}

/// What the batch changed against the loaded model.
struct Diff<'a> {
    /// Module files to write: added and changed modules.
    write: Vec<&'a Module>,
    /// Local IDs of module files to delete.
    remove: Vec<&'a str>,
    /// IDs leaving the model, allocated-then-removed ones included; they stay reserved.
    retired: Vec<String>,
    subjects: Subjects,
    /// The pre-existing subjects a claim must cover, and the model ID for the root level.
    affected: BTreeSet<String>,
}

fn diff<'a>(model: &'a Model, new: &'a [Module], fresh: &[String]) -> Diff<'a> {
    let (old_modules, new_modules) = (by_id(&model.modules), by_id(new));
    let (old_relationships, new_relationships) =
        (relationships(&model.modules), relationships(new));
    let live = |id: &str| new_modules.contains_key(id) || new_relationships.contains_key(id);
    let mut diff = Diff {
        write: Vec::new(),
        remove: Vec::new(),
        retired: Vec::new(),
        subjects: Subjects::default(),
        affected: BTreeSet::new(),
    };
    for (id, module) in &new_modules {
        match old_modules.get(id) {
            None => diff.subjects.added.push(model.qualify(id)),
            Some(before) if before != module => diff.subjects.changed.push(model.qualify(id)),
            Some(_) => continue,
        }
        diff.write.push(module);
    }
    for (id, relationship) in &new_relationships {
        match old_relationships.get(id) {
            None => diff.subjects.added.push(model.qualify(id)),
            Some(before) if before != relationship => diff.subjects.changed.push(model.qualify(id)),
            Some(_) => {}
        }
    }
    for id in old_modules.keys().chain(old_relationships.keys()) {
        if !live(id) {
            diff.subjects.removed.push(model.qualify(id));
            diff.retired.push((*id).to_owned());
        }
    }
    diff.remove = old_modules
        .keys()
        .copied()
        .filter(|id| !new_modules.contains_key(id))
        .collect();
    diff.retired
        .extend(fresh.iter().filter(|id| !live(id)).cloned());
    for list in [
        &mut diff.subjects.added,
        &mut diff.subjects.changed,
        &mut diff.subjects.removed,
    ] {
        list.sort();
    }

    // The subjects a claim must cover, `None` standing for the root level that the model ID
    // holds. A module or relationship is touched when it changes or leaves. So is a module
    // gaining or losing a part, or the root level for a root module, and the old and new source
    // and target of each relationship added, changed, moved or removed. So is every module whose
    // current or former name an added or renamed module takes, since the name would then stop
    // naming it alone. IDs this batch allocates are left out: nobody could have claimed them.
    let mut touched: Vec<Option<String>> = Vec::new();
    let module_ids: BTreeSet<&str> = old_modules
        .keys()
        .chain(new_modules.keys())
        .copied()
        .collect();
    for id in module_ids {
        let (before, after) = (old_modules.get(id), new_modules.get(id));
        if before == after {
            continue;
        }
        touched.push(Some(model.qualify(id)));
        let parent = |module: Option<&&Module>| module.map(|m| m.parent.clone());
        if parent(before) != parent(after) {
            touched.extend(parent(before).into_iter().chain(parent(after)));
        }
        if let Some(named) = after.filter(|a| before.is_none_or(|b| b.name != a.name)) {
            touched.extend(
                model
                    .modules
                    .iter()
                    .filter(|m| m.name == named.name || m.former_names.contains(&named.name))
                    .map(|m| Some(model.qualify(&m.id))),
            );
        }
    }
    let relationship_ids: BTreeSet<&str> = old_relationships
        .keys()
        .chain(new_relationships.keys())
        .copied()
        .collect();
    for id in relationship_ids {
        let (before, after) = (old_relationships.get(id), new_relationships.get(id));
        if before == after {
            continue;
        }
        touched.push(Some(model.qualify(id)));
        for (owner, relationship) in before.into_iter().chain(after) {
            touched.push(Some(model.qualify(owner)));
            touched.push(Some(relationship.to.clone()));
        }
    }
    let existed = |id: &str| {
        let local = ids::local(&model.manifest.id, id);
        old_modules.contains_key(local) || old_relationships.contains_key(local)
    };
    diff.affected = touched
        .into_iter()
        .filter_map(|subject| match subject {
            None => Some(model.manifest.id.clone()),
            Some(id) => existed(&id).then_some(id),
        })
        .collect();
    // A batch that only retires IDs, adding and removing a module, still rewrites the manifest.
    if diff.affected.is_empty() && !diff.retired.is_empty() {
        diff.affected.insert(model.manifest.id.clone());
    }
    diff
}

fn by_id(modules: &[Module]) -> BTreeMap<&str, &Module> {
    modules.iter().map(|m| (m.id.as_str(), m)).collect()
}

/// The model while a batch applies. Removed modules stay until `finish`, so a later change can
/// still move their relationships or reparent their children.
struct Batch<'a> {
    model: &'a str,
    modules: Vec<Module>,
    removed: BTreeSet<String>,
    /// Every ID the model holds or retired, and those allocated so far, in ASCII lowercase.
    taken: BTreeSet<String>,
    fresh: Vec<String>,
    allocated: Vec<Allocation>,
    edited: BTreeSet<String>,
    /// The last change that touched each module or relationship, by local ID.
    touched: BTreeMap<String, usize>,
}

impl<'a> Batch<'a> {
    fn new(model: &'a Model) -> Self {
        let manifest = &model.manifest;
        let taken = [&manifest.id, &manifest.current.id]
            .into_iter()
            .chain(&manifest.retired_ids)
            .chain(
                model
                    .modules
                    .iter()
                    .flat_map(|m| iter::once(&m.id).chain(m.outgoing.iter().map(|r| &r.id))),
            )
            .map(|id| id.to_ascii_lowercase())
            .collect();
        Self {
            model: &manifest.id,
            modules: model.modules.clone(),
            removed: BTreeSet::new(),
            taken,
            fresh: Vec::new(),
            allocated: Vec::new(),
            edited: BTreeSet::new(),
            touched: BTreeMap::new(),
        }
    }

    fn apply(&mut self, index: usize, change: Change) -> Result<()> {
        match change {
            Change::AddModule { module } => self.add_module(index, module),
            Change::EditModule {
                subject,
                set,
                clear,
            } => self.edit_module(index, &subject, set, &clear),
            Change::RemoveModule { subject } => self.remove_module(index, &subject),
            Change::AddRelationship {
                source,
                relationship,
            } => self.add_relationship(index, &source, relationship),
            Change::EditRelationship {
                subject,
                source,
                set,
                clear,
            } => self.edit_relationship(index, &subject, source.as_deref(), set, &clear),
            Change::RemoveRelationship { subject } => self.remove_relationship(index, &subject),
        }
    }

    fn qualify(&self, at: usize) -> String {
        ids::qualify(self.model, &self.modules[at].id)
    }

    fn allocate(&mut self) -> Result<String> {
        let id = ids::allocate(|candidate| self.taken.contains(candidate))?;
        self.taken.insert(id.to_ascii_lowercase());
        self.fresh.push(id.clone());
        Ok(id)
    }

    /// The position of the module `subject` names among those not removed.
    fn module(&self, index: usize, subject: &str) -> Result<usize> {
        let live = self
            .modules
            .iter()
            .filter(|m| !self.removed.contains(&m.id));
        let found = vault::find(self.model, live, subject).map_err(at(index))?;
        self.modules
            .iter()
            .position(|m| m.id == found.id)
            .with_context(|| format!("module {} is not in the batch", found.id))
    }

    /// The owning module's position and the relationship's position in its outgoing list.
    fn relationship(&self, index: usize, subject: &str) -> Result<(usize, usize)> {
        let local = ids::local(self.model, subject);
        self.modules
            .iter()
            .enumerate()
            .find_map(|(m, module)| {
                let r = module.outgoing.iter().position(|r| r.id == local)?;
                Some((m, r))
            })
            .ok_or_else(|| {
                at(index)(Refusal::new(
                    Code::SubjectNotFound,
                    format!("no relationship has the ID `{subject}`"),
                ))
                .into()
            })
    }

    /// Records the edit of `id`, refusing a second edit of the same subject.
    fn edit_once(&mut self, index: usize, id: &str) -> Result<()> {
        if !self.edited.insert(id.to_owned()) {
            return Err(request_invalid(
                index,
                format!(
                    "{} is already edited by an earlier change; combine the edits",
                    ids::qualify(self.model, id)
                ),
            ));
        }
        Ok(())
    }

    fn reference(&self, index: usize, subject: Option<&str>) -> Result<Option<String>> {
        subject
            .map(|s| self.module(index, s).map(|at| self.qualify(at)))
            .transpose()
    }

    fn add_module(&mut self, index: usize, new: NewModule) -> Result<()> {
        let parent = self.reference(index, new.parent.as_deref())?;
        let id = self.allocate()?;
        self.allocated.push(Allocation {
            name: new.name.clone(),
            id: ids::qualify(self.model, &id),
        });
        self.touched.insert(id.clone(), index);
        self.modules.push(Module {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            kind: new.kind,
            order: new.order,
            parent,
            responsibility: new.responsibility,
            description: new.description,
            groups: new.groups,
            binding: new.binding,
            former_names: Vec::new(),
            outgoing: Vec::new(),
        });
        Ok(())
    }

    fn edit_module(
        &mut self,
        index: usize,
        subject: &str,
        set: ModuleSet,
        clear: &[ModuleField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let at = self.module(index, subject)?;
        let id = self.modules[at].id.clone();
        self.edit_once(index, &id)?;
        let parent = self.reference(index, set.parent.as_deref())?;
        let module = &mut self.modules[at];
        if let Some(name) = set.name {
            rename(module, name);
        }
        if let Some(kind) = set.kind {
            module.kind = kind;
        }
        if let Some(order) = set.order {
            module.order = order;
        }
        if parent.is_some() {
            module.parent = parent;
        }
        if set.responsibility.is_some() {
            module.responsibility = set.responsibility;
        }
        if set.description.is_some() {
            module.description = set.description;
        }
        if let Some(groups) = set.groups {
            module.groups = groups;
        }
        if set.binding.is_some() {
            module.binding = set.binding;
        }
        for field in clear {
            match field {
                ModuleField::Parent => module.parent = None,
                ModuleField::Responsibility => module.responsibility = None,
                ModuleField::Description => module.description = None,
                ModuleField::Groups => module.groups.clear(),
                ModuleField::Binding => module.binding = None,
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    fn remove_module(&mut self, index: usize, subject: &str) -> Result<()> {
        let at = self.module(index, subject)?;
        let id = self.modules[at].id.clone();
        self.touched.insert(id.clone(), index);
        self.removed.insert(id);
        Ok(())
    }

    fn add_relationship(&mut self, index: usize, source: &str, new: NewRelationship) -> Result<()> {
        let from = self.module(index, source)?;
        let target = self.module(index, &new.to)?;
        let id = self.allocate()?;
        self.allocated.push(Allocation {
            name: format!(
                "relationship from {} to {}",
                self.modules[from].name, self.modules[target].name
            ),
            id: ids::qualify(self.model, &id),
        });
        self.touched.insert(id.clone(), index);
        let to = self.qualify(target);
        self.modules[from].outgoing.push(Relationship {
            id,
            to,
            kind: new.kind,
            order: new.order,
            label: new.label,
            description: new.description,
            emphasis: new.emphasis,
            anchor: new.anchor,
        });
        Ok(())
    }

    fn edit_relationship(
        &mut self,
        index: usize,
        subject: &str,
        source: Option<&str>,
        set: RelationshipSet,
        clear: &[RelationshipField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let (from, at) = self.relationship(index, subject)?;
        let id = self.modules[from].outgoing[at].id.clone();
        self.edit_once(index, &id)?;
        let owner = source
            .map(|s| self.module(index, s))
            .transpose()?
            .unwrap_or(from);
        let to = self.reference(index, set.to.as_deref())?;
        // Only a transfer moves the relationship, so an edit keeps its place in the file.
        let at = if owner == from {
            at
        } else {
            let moved = self.modules[from].outgoing.remove(at);
            self.modules[owner].outgoing.push(moved);
            self.modules[owner].outgoing.len() - 1
        };
        let relationship = &mut self.modules[owner].outgoing[at];
        if let Some(to) = to {
            relationship.to = to;
        }
        if let Some(kind) = set.kind {
            relationship.kind = kind;
        }
        if let Some(order) = set.order {
            relationship.order = order;
        }
        if set.label.is_some() {
            relationship.label = set.label;
        }
        if set.description.is_some() {
            relationship.description = set.description;
        }
        if set.emphasis.is_some() {
            relationship.emphasis = set.emphasis;
        }
        if set.anchor.is_some() {
            relationship.anchor = set.anchor;
        }
        for field in clear {
            match field {
                RelationshipField::Label => relationship.label = None,
                RelationshipField::Description => relationship.description = None,
                RelationshipField::Emphasis => relationship.emphasis = None,
                RelationshipField::Anchor => relationship.anchor = None,
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    fn remove_relationship(&mut self, index: usize, subject: &str) -> Result<()> {
        let (from, at) = self.relationship(index, subject)?;
        let relationship = self.modules[from].outgoing.remove(at);
        self.touched.insert(relationship.id, index);
        Ok(())
    }

    /// Drops removed modules and every relationship from or to them, refusing when a surviving
    /// module still names a removed parent.
    fn finish(&mut self) -> Result<()> {
        let gone: BTreeSet<String> = self
            .removed
            .iter()
            .map(|id| ids::qualify(self.model, id))
            .collect();
        let orphans: Vec<String> = self
            .modules
            .iter()
            .filter(|m| !self.removed.contains(&m.id))
            .filter(|m| m.parent.as_ref().is_some_and(|p| gone.contains(p)))
            .map(|m| ids::qualify(self.model, &m.id))
            .collect();
        if !orphans.is_empty() {
            return Err(Refusal::new(
                Code::ChildrenOrphaned,
                format!(
                    "{} module(s) would lose their parent; remove or reparent them in the same batch",
                    orphans.len()
                ),
            )
            .with_details(json!({ "children": orphans }))
            .into());
        }
        self.modules.retain(|m| !self.removed.contains(&m.id));
        for module in &mut self.modules {
            module.outgoing.retain(|r| !gone.contains(&r.to));
        }
        Ok(())
    }
}

/// A rename keeps the old name as a former name; renaming back to a former name drops it.
fn rename(module: &mut Module, name: String) {
    if name == module.name {
        return;
    }
    module.former_names.retain(|n| *n != name);
    let old = std::mem::replace(&mut module.name, name);
    if !module.former_names.contains(&old) {
        module.former_names.push(old);
    }
}

/// Names change `index` in a refusal's message and details.
fn at(index: usize) -> impl Fn(Refusal) -> Refusal {
    move |mut refusal| {
        refusal.message = format!("changes[{index}]: {}", refusal.message);
        let mut details = refusal.details.take().unwrap_or_else(|| json!({}));
        details["change"] = index.into();
        refusal.with_details(details)
    }
}

fn request_invalid(index: usize, message: String) -> anyhow::Error {
    at(index)(Refusal::new(Code::RequestInvalid, message)).into()
}

/// A change that sets and clears the same field, named as the request spells it.
fn both(index: usize, field: impl Serialize) -> anyhow::Error {
    let name = serde_json::to_value(field)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    request_invalid(index, format!("sets and clears `{name}`"))
}

/// The journal in `<git dir>/boundrail/operations`, read and written under the vault lock.
struct Journal {
    state: Locked,
}

impl Journal {
    fn open(vault: &Path) -> Result<Self> {
        let state = state::lock(vault)?;
        let dir = state.dir.join(OPERATIONS);
        refuse_symlink(&dir)?;
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { state })
    }

    fn path(&self, operation: &str) -> PathBuf {
        self.state
            .dir
            .join(OPERATIONS)
            .join(format!("{operation}.json"))
    }

    fn read(&self, operation: &str) -> Result<Option<Record>> {
        state::read(&self.path(operation))
    }

    /// Every pending record, in operation ID order.
    // ponytail: reads every record per edit; keep pending records apart if journals grow large.
    fn pending(&self) -> Result<Vec<Record>> {
        let mut pending = Vec::new();
        for path in state::operations(&self.state.dir)? {
            if let Some(record) = state::read::<Record>(&path)?
                && record.status == Status::Pending
            {
                pending.push(record);
            }
        }
        Ok(pending)
    }

    /// Fails, before any vault write for a pending record, when the record is too large to read.
    fn write(&self, record: &Record) -> Result<()> {
        state::write(&self.path(&record.output.operation_id), record)
    }
}
