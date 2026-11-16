//! The picture changes of a `model edit` batch. Pictures belong to the current state; a proposal
//! edit refuses them. A picture's reading resolves and validates as a walkthrough beat's does.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::walkthrough::{BeatField, BeatSet, CurrentOnly, NewReading, Refused};
use super::{Batch, at, both};
use crate::arch::types::{Picture, ReadingState, ReadingView, SCHEMA_VERSION};
use crate::arch::{Code, vault};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPicture {
    pub name: String,
    pub order: u32,
    pub description: Option<String>,
    /// The file to draw, relative to the repository root, ending in `.svg` or `.png`.
    pub output: String,
    pub reading: NewReading,
}

/// New values for a picture and its reading; references resolve in the state it reads after the
/// change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PictureSet {
    pub name: Option<String>,
    pub order: Option<u32>,
    pub description: Option<String>,
    pub output: Option<String>,
    pub view: Option<ReadingView>,
    pub state: Option<ReadingState>,
    pub against: Option<ReadingState>,
    pub scope: Option<String>,
    pub depth: Option<u32>,
    pub flow: Option<String>,
    pub focus: Option<Vec<String>>,
}

/// A picture field `clear` empties; clearing `state` reads the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PictureField {
    Description,
    State,
    Against,
    Scope,
    Depth,
    Flow,
    Focus,
}

impl PictureField {
    /// The reading field it clears, `None` for the picture's own.
    fn reading(self) -> Option<BeatField> {
        match self {
            Self::Description => None,
            Self::State => Some(BeatField::State),
            Self::Against => Some(BeatField::Against),
            Self::Scope => Some(BeatField::Scope),
            Self::Depth => Some(BeatField::Depth),
            Self::Flow => Some(BeatField::Flow),
            Self::Focus => Some(BeatField::Focus),
        }
    }
}

impl PictureSet {
    fn is_set(&self, field: PictureField) -> bool {
        match field.reading() {
            Some(field) => field.is_set(&self.reading()),
            None => self.description.is_some(),
        }
    }

    /// The reading fields it sets, as a beat edit names them.
    fn reading(&self) -> BeatSet {
        BeatSet {
            view: self.view,
            state: self.state.clone(),
            against: self.against.clone(),
            scope: self.scope.clone(),
            depth: self.depth,
            flow: self.flow.clone(),
            focus: self.focus.clone(),
            ..BeatSet::default()
        }
    }
}

impl CurrentOnly for Picture {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn ids(&self) -> Vec<&str> {
        vec![&self.id]
    }
    fn file(id: &str) -> String {
        vault::picture_file(id)
    }
}

impl Batch<'_> {
    fn picture_at(&self, index: usize, subject: &str) -> Result<usize> {
        let found = vault::picture_position(self.model, &self.pictures, subject);
        Ok(found.map_err(at(index))?)
    }

    pub(super) fn add_picture(&mut self, index: usize, new: NewPicture) -> Result<()> {
        let id = self.new_id(index, new.name.clone())?;
        let reading = self.build_reading(index, new.reading)?;
        self.pictures.push(Picture {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            order: new.order,
            description: new.description,
            output: new.output,
            reading,
        });
        Ok(())
    }

    pub(super) fn edit_picture(
        &mut self,
        index: usize,
        subject: &str,
        set: PictureSet,
        clear: &[PictureField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| set.is_set(**f)) {
            return Err(both(index, field));
        }
        let p = self.picture_at(index, subject)?;
        let id = self.pictures[p].id.clone();
        self.edit_once(index, &id)?;
        let cleared: Vec<BeatField> = clear.iter().filter_map(|f| f.reading()).collect();
        let mut reading = self.pictures[p].reading.clone();
        self.apply_reading(index, &mut reading, &set.reading(), &cleared)?;
        let picture = &mut self.pictures[p];
        picture.reading = reading;
        if let Some(name) = set.name {
            picture.name = name;
        }
        if let Some(order) = set.order {
            picture.order = order;
        }
        if let Some(output) = set.output {
            picture.output = output;
        }
        if set.description.is_some() {
            picture.description = set.description;
        }
        if clear.contains(&PictureField::Description) {
            picture.description = None;
        }
        self.touched.insert(id, index);
        Ok(())
    }

    pub(super) fn remove_picture(&mut self, index: usize, subject: &str) -> Result<()> {
        let p = self.picture_at(index, subject)?;
        let removed = self.pictures.remove(p);
        self.touched.insert(removed.id, index);
        Ok(())
    }

    /// Refuses when a current-state picture the batch did not edit reads a module or flow the
    /// batch removed; an edited one is judged by validation instead.
    pub(super) fn refuse_picture_references(&self) -> Result<()> {
        let readings = self.pictures.iter().map(|p| (p.id.as_str(), &p.reading));
        let refused = Refused {
            code: Code::ReferencedByPicture,
            what: "picture(s)",
            key: "pictures",
        };
        self.refuse_reading_references(readings, &refused)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::{Record, plan};
    use crate::arch::types::{Manifest, Picture};
    use crate::arch::vault::Model;
    use crate::arch::{Code, Refusal, revision};

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    /// Web and Api; the picture Shape draws the architecture with Api in focus.
    fn model() -> Model {
        let module = |id: &str, name: &str| {
            serde_json::from_value(
                json!({"schema_version": 1, "id": id, "name": name, "kind": "service", "order": 0}),
            )
            .unwrap()
        };
        let pictures: Vec<Picture> = serde_json::from_value(json!([
            {"schema_version": 1, "id": "Pict0001", "name": "Shape", "order": 0,
             "output": "docs/shape.svg", "reading": {"view": "architecture", "focus": [q("ApiSrv01")]}},
        ]))
        .unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "schema_version": 1, "id": M, "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap();
        Model {
            dir: PathBuf::from("vault"),
            manifest,
            config: Default::default(),
            modules: vec![module("WebApp01", "Web"), module("ApiSrv01", "Api")],
            collections: Vec::new(),
            walkthroughs: Vec::new(),
            pictures,
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
        plan(&model(), request, "testing pictures", String::new()).map(|(r, _, _)| r)
    }

    fn written(record: &Record, local: &str) -> Option<Value> {
        let path = format!("pictures/{local}.picture.json");
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
    fn pictures_are_added_edited_and_removed() {
        let record = plan_of(json!([{"type": "add-picture", "picture": {
            "name": "Web", "order": 1, "output": "docs/web.png",
            "reading": {"view": "architecture", "scope": "Web", "focus": ["Api", "Api"]}}}]))
        .unwrap();
        let id = record.output.allocated[0].id.clone();
        let local = id.split_once('#').unwrap().1;
        let picture = written(&record, local).unwrap();
        assert_eq!(picture["output"], "docs/web.png");
        assert_eq!(
            picture["reading"],
            json!({"view": "architecture", "scope": q("WebApp01"), "focus": [q("ApiSrv01")]})
        );
        assert_eq!(record.output.subjects.added, [id]);

        let record = plan_of(json!([{"type": "edit-picture", "subject": "Shape",
            "set": {"output": "docs/shape.png", "depth": 1}, "clear": ["focus"]}]))
        .unwrap();
        let picture = written(&record, "Pict0001").unwrap();
        assert_eq!(picture["output"], "docs/shape.png");
        assert_eq!(
            picture["reading"],
            json!({"view": "architecture", "depth": 1})
        );
        assert_eq!(record.output.subjects.changed, [q("Pict0001")]);

        let record =
            plan_of(json!([{"type": "remove-picture", "subject": q("Pict0001")}])).unwrap();
        let writes = record.writes.as_deref().unwrap();
        assert!(
            writes
                .iter()
                .any(|w| w.path == "pictures/Pict0001.picture.json" && w.content.is_none())
        );

        let both = json!([{"type": "edit-picture", "subject": "Shape", "set": {"scope": "Web"}, "clear": ["scope"]}]);
        assert_eq!(refusal(both).0, Code::RequestInvalid);
        let bad =
            json!([{"type": "edit-picture", "subject": "Shape", "set": {"output": "../x.svg"}}]);
        assert_eq!(refusal(bad).0, Code::ModelInvalid);
    }

    #[test]
    fn removing_what_a_picture_reads_refuses() {
        let (code, details) = refusal(json!([{"type": "remove-module", "subject": "Api"}]));
        assert_eq!(code, Code::ReferencedByPicture);
        assert_eq!(details["pictures"], json!([q("Pict0001")]));
        assert_eq!(details["change"], 0);
        plan_of(json!([
            {"type": "edit-picture", "subject": "Shape", "clear": ["focus"]},
            {"type": "remove-module", "subject": "Api"},
        ]))
        .unwrap();
    }
}
