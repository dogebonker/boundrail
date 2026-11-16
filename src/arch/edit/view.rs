//! The saved-view changes of a `model edit` batch, and what picture changes share with them: a
//! named, ordered reading. Both belong to the current state; a proposal edit refuses them. Their
//! readings resolve and validate as a walkthrough beat's do.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::walkthrough::{BeatField, NewReading, ReadingSet, Refused};
use super::{Batch, at, both};
use crate::arch::Code;
use crate::arch::types::{SCHEMA_VERSION, SavedView};
use crate::arch::vault::{self, Named};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewView {
    pub name: String,
    pub order: u32,
    pub description: Option<String>,
    pub reading: NewReading,
}

/// New values for a saved view and its reading; references resolve in the state it reads after
/// the change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSet {
    pub name: Option<String>,
    pub order: Option<u32>,
    pub description: Option<String>,
    #[serde(flatten)]
    pub reading: ReadingSet,
}

/// A saved-view or picture field `clear` empties; clearing `state` reads the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamedField {
    Description,
    State,
    Against,
    Scope,
    Depth,
    Flow,
    Focus,
    Selection,
    Filters,
    Groups,
    Visibility,
}

impl NamedField {
    /// The reading field it clears, `None` for the view's or picture's own.
    fn reading(self) -> Option<BeatField> {
        match self {
            Self::Description => None,
            Self::State => Some(BeatField::State),
            Self::Against => Some(BeatField::Against),
            Self::Scope => Some(BeatField::Scope),
            Self::Depth => Some(BeatField::Depth),
            Self::Flow => Some(BeatField::Flow),
            Self::Focus => Some(BeatField::Focus),
            Self::Selection => Some(BeatField::Selection),
            Self::Filters => Some(BeatField::Filters),
            Self::Groups => Some(BeatField::Groups),
            Self::Visibility => Some(BeatField::Visibility),
        }
    }

    fn is_set(self, set: &ViewSet) -> bool {
        match self.reading() {
            Some(field) => field.is_set(&set.reading),
            None => set.description.is_some(),
        }
    }
}

impl Batch<'_> {
    pub(super) fn add_view(&mut self, index: usize, new: NewView) -> Result<()> {
        let id = self.new_id(index, new.name.clone())?;
        let reading = self.build_reading(index, new.reading)?;
        self.views.push(SavedView {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            order: new.order,
            description: new.description,
            reading,
        });
        Ok(())
    }

    /// The position among `list(self)`, saved views or pictures, of the one `subject` names.
    fn named_at<T: Named>(
        &mut self,
        index: usize,
        subject: &str,
        list: fn(&mut Self) -> &mut Vec<T>,
    ) -> Result<usize> {
        let model = self.model;
        Ok(vault::position(model, list(self), subject).map_err(at(index))?)
    }

    /// Sets and clears the name, order, description and reading of the saved view or picture
    /// `subject` names among `list(self)`, and returns its position there.
    pub(super) fn edit_named<T: Named>(
        &mut self,
        index: usize,
        subject: &str,
        set: ViewSet,
        clear: &[NamedField],
        list: fn(&mut Self) -> &mut Vec<T>,
    ) -> Result<usize> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let at = self.named_at(index, subject, list)?;
        let id = list(self)[at].id().to_owned();
        self.edit_once(index, &id)?;
        let cleared: Vec<BeatField> = clear.iter().filter_map(|f| f.reading()).collect();
        let mut reading = list(self)[at].reading().clone();
        self.apply_reading(index, &mut reading, &set.reading, &cleared)?;
        let (name, order, description, stored) = list(self)[at].parts_mut();
        *stored = reading;
        if let Some(new) = set.name {
            *name = new;
        }
        if let Some(new) = set.order {
            *order = new;
        }
        if set.description.is_some() {
            *description = set.description;
        }
        if clear.contains(&NamedField::Description) {
            *description = None;
        }
        self.touched.insert(id, index);
        Ok(at)
    }

    /// Removes the saved view or picture `subject` names from `list(self)`.
    pub(super) fn remove_named<T: Named>(
        &mut self,
        index: usize,
        subject: &str,
        list: fn(&mut Self) -> &mut Vec<T>,
    ) -> Result<()> {
        let at = self.named_at(index, subject, list)?;
        let removed = list(self).remove(at);
        self.touched.insert(removed.id().to_owned(), index);
        Ok(())
    }

    /// Refuses when a current-state view the batch did not edit reads a module or flow the batch
    /// removed; an edited one is judged by validation instead.
    pub(super) fn refuse_view_references(&self) -> Result<()> {
        let readings = self.views.iter().map(|v| (v.id.as_str(), &v.reading));
        let refused = Refused {
            code: Code::ReferencedByView,
            what: "view(s)",
            key: "views",
        };
        self.refuse_reading_references(readings, &refused)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::{Record, plan};
    use crate::arch::types::{Manifest, SavedView};
    use crate::arch::vault::Model;
    use crate::arch::{Code, Refusal, revision};

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    /// Web and Api, both in the group `core`, and the group `idle` with no module; the view Shape
    /// draws the architecture with Api in focus.
    fn model() -> Model {
        let module = |id: &str, name: &str| {
            serde_json::from_value(json!({"schema_version": 1, "id": id, "name": name,
                "kind": "service", "order": 0, "groups": ["core"]}))
            .unwrap()
        };
        let views: Vec<SavedView> = serde_json::from_value(json!([
            {"schema_version": 1, "id": "View0001", "name": "Shape", "order": 0,
             "reading": {"view": "architecture", "focus": [q("ApiSrv01")]}},
        ]))
        .unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "schema_version": 1, "id": M, "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap();
        let mut config = crate::arch::types::VaultConfig::default();
        let core = serde_json::from_value(json!({"name": "Core"})).unwrap();
        config.groups.insert("core".to_owned(), core);
        let idle = serde_json::from_value(json!({"name": "Idle"})).unwrap();
        config.groups.insert("idle".to_owned(), idle);
        Model {
            dir: PathBuf::from("vault"),
            manifest,
            config,
            modules: vec![module("WebApp01", "Web"), module("ApiSrv01", "Api")],
            collections: Vec::new(),
            walkthroughs: Vec::new(),
            pictures: Vec::new(),
            views,
            revision: revision::of(std::iter::empty()),
            proposal: None,
        }
    }

    fn plan_of(changes: Value) -> anyhow::Result<Record> {
        let request = serde_json::from_value(json!({
            "target": {"model": M, "state": "State001"}, "expected_revision": "unused",
            "operation_id": "op-1", "writer": "tester", "changes": changes,
        }))
        .unwrap();
        plan(&model(), request, "testing views", String::new()).map(|(r, _, _)| r)
    }

    fn written(record: &Record, local: &str) -> Option<Value> {
        let path = format!("views/{local}.view.json");
        let writes = record.writes.as_deref().unwrap();
        let write = writes.iter().find(|w| w.path == path)?;
        write
            .content
            .as_deref()
            .map(|c| serde_json::from_str(c).unwrap())
    }

    fn refusal(changes: Value) -> (Code, Value) {
        let Err(error) = plan_of(changes) else {
            panic!("the plan succeeded");
        };
        let refusal = error.downcast_ref::<Refusal>().unwrap();
        (refusal.code, refusal.details.clone().unwrap_or_default())
    }

    #[test]
    fn views_are_added_edited_and_removed() {
        let record = plan_of(json!([{"type": "add-view", "view": {
            "name": "Core", "order": 1,
            "reading": {"view": "architecture", "focus": ["Api"], "visibility": 1,
                "selection": ["Web", "Web"], "groups": ["core", "core"],
                "filters": {"relationships": ["call", "call"], "groups": ["core"]}}}}]))
        .unwrap();
        let id = record.output.allocated[0].id.clone();
        let local = id.split_once('#').unwrap().1;
        let view = written(&record, local).unwrap();
        assert_eq!(
            view,
            json!({"schema_version": 1, "id": local, "name": "Core", "order": 1,
                "reading": {"view": "architecture", "focus": [q("ApiSrv01")],
                    "selection": [q("WebApp01")],
                    "filters": {"relationships": ["call"], "groups": ["core"]},
                    "groups": ["core"], "visibility": 1}})
        );
        assert_eq!(record.output.subjects.added, [id]);

        let record = plan_of(json!([{"type": "edit-view", "subject": "Shape",
            "set": {"name": "Outline", "groups": "all", "selection": ["Web"]},
            "clear": ["focus"]}]))
        .unwrap();
        let view = written(&record, "View0001").unwrap();
        assert_eq!(view["name"], "Outline");
        assert_eq!(
            view["reading"],
            json!({"view": "architecture", "selection": [q("WebApp01")], "groups": "all"})
        );
        assert_eq!(record.output.subjects.changed, [q("View0001")]);

        let record = plan_of(json!([{"type": "remove-view", "subject": q("View0001")}])).unwrap();
        let writes = record.writes.as_deref().unwrap();
        assert!(
            writes
                .iter()
                .any(|w| w.path == "views/View0001.view.json" && w.content.is_none())
        );

        let both = json!([{"type": "edit-view", "subject": "Shape",
            "set": {"visibility": 1}, "clear": ["visibility"]}]);
        assert_eq!(refusal(both).0, Code::RequestInvalid);
        let unfocused = json!([{"type": "edit-view", "subject": "Shape", "clear": ["focus"], "set": {"visibility": 2}}]);
        assert_eq!(refusal(unfocused).0, Code::ModelInvalid);

        // A group filter naming a group no module is in would draw nothing of it.
        let idle = json!({"groups": ["idle"]});
        let added = json!([{"type": "add-view", "view": {"name": "Idle", "order": 2,
            "reading": {"view": "architecture", "filters": idle}}}]);
        let edited = json!([{"type": "edit-view", "subject": "Shape", "set": {"filters": idle}}]);
        for changes in [added, edited] {
            let (code, details) = refusal(changes);
            assert_eq!(code, Code::RequestInvalid);
            assert_eq!(details["change"], 0);
        }
    }

    #[test]
    fn set_fields_beside_the_reading_still_refuse_unknown_keys() {
        let set: super::ViewSet =
            serde_json::from_value(json!({"name": "Outline", "depth": 1})).unwrap();
        assert_eq!(
            (set.name.as_deref(), set.reading.depth),
            (Some("Outline"), Some(1))
        );
        let unknown = json!({"name": "Outline", "depth": 1, "bogus": true});
        assert!(serde_json::from_value::<super::ViewSet>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<super::super::PictureSet>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<super::super::BeatSet>(unknown).is_err());
    }

    #[test]
    fn removing_what_a_view_reads_refuses() {
        let (code, details) = refusal(json!([{"type": "remove-module", "subject": "Api"}]));
        assert_eq!(code, Code::ReferencedByView);
        assert_eq!(details["views"], json!([q("View0001")]));
        assert_eq!(details["change"], 0);
        plan_of(json!([
            {"type": "edit-view", "subject": "Shape", "clear": ["focus"]},
            {"type": "remove-module", "subject": "Api"},
        ]))
        .unwrap();
    }
}
