//! The picture changes of a `model edit` batch. A picture is edited as a saved view is, and also
//! names the file it draws. Pictures belong to the current state; a proposal edit refuses them.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Batch;
use super::view::{NamedField, ViewSet};
use super::walkthrough::{NewReading, ReadingSet, Refused};
use crate::arch::Code;
use crate::arch::types::{Picture, SCHEMA_VERSION};

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
    #[serde(flatten)]
    pub reading: ReadingSet,
}

impl Batch<'_> {
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
        clear: &[NamedField],
    ) -> Result<()> {
        let PictureSet {
            name,
            order,
            description,
            output,
            reading,
        } = set;
        let set = ViewSet {
            name,
            order,
            description,
            reading,
        };
        let p = self.edit_named(index, subject, set, clear, |b| &mut b.pictures)?;
        if let Some(output) = output {
            self.pictures[p].output = output;
        }
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
            views: Vec::new(),
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
