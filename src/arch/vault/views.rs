//! Saved views in the vault, and what pictures share with them: a named, ordered reading of the
//! current state, found, listed and checked alike. A view writes no file; a picture also names
//! the file Boundrail draws it to.

use serde::Serialize;

use super::walkthroughs::{Known, reading_problems};
use super::{Contents, Model, Problems, check_file, check_text, pick};
use crate::arch::types::{
    BeatReading, DESCRIPTION_MAX, Picture, ReadingState, SavedView, VaultConfig,
};
use crate::arch::{Refusal, ids};

/// A named, ordered reading the current state holds: a saved view or a picture.
pub(crate) trait Named {
    /// What a refusal calls one.
    const KIND: &'static str;
    fn schema_version(&self) -> u32;
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn order(&self) -> u32;
    fn description(&self) -> Option<&str>;
    fn reading(&self) -> &BeatReading;
    /// Its name, order, description and reading, to edit.
    fn parts_mut(&mut self) -> (&mut String, &mut u32, &mut Option<String>, &mut BeatReading);
    /// Its vault-relative file.
    fn file(id: &str) -> String;
}

impl Named for SavedView {
    const KIND: &'static str = "view";
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn order(&self) -> u32 {
        self.order
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn reading(&self) -> &BeatReading {
        &self.reading
    }
    fn parts_mut(&mut self) -> (&mut String, &mut u32, &mut Option<String>, &mut BeatReading) {
        (
            &mut self.name,
            &mut self.order,
            &mut self.description,
            &mut self.reading,
        )
    }
    fn file(id: &str) -> String {
        super::view_file(id)
    }
}

impl Named for Picture {
    const KIND: &'static str = "picture";
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn order(&self) -> u32 {
        self.order
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn reading(&self) -> &BeatReading {
        &self.reading
    }
    fn parts_mut(&mut self) -> (&mut String, &mut u32, &mut Option<String>, &mut BeatReading) {
        (
            &mut self.name,
            &mut self.order,
            &mut self.description,
            &mut self.reading,
        )
    }
    fn file(id: &str) -> String {
        super::picture_file(id)
    }
}

/// Each of `items`' own fields, then what `own` checks of it, then its reading. References are
/// judged only with the model ID, and configured keys only with the config.
pub(super) fn check<'a, T: Named>(
    model: Option<&str>,
    config: Option<&VaultConfig>,
    contents: Contents<'_>,
    items: &[(&str, &'a T)],
    problems: &mut Problems,
    mut own: impl FnMut(&str, &'a T, &mut Problems),
) {
    let known = Known::new(model, ReadingState::Current, contents);
    for &(file, item) in items {
        let id = item.id();
        check_file(problems, file, id, item.schema_version(), item.name());
        let (description, max) = (item.description(), Some(DESCRIPTION_MAX));
        check_text(problems, file, id, "description", description, max, true);
        own(file, item, problems);
        for message in reading_problems(item.reading(), known.as_ref(), config) {
            problems.push(file, id, message);
        }
    }
}

/// The position among `items` of the one `subject` names: its qualified or local ID, else its
/// unique name.
pub(crate) fn position<T: Named>(
    model: &str,
    items: &[T],
    subject: &str,
) -> Result<usize, Refusal> {
    let found = (items.iter().enumerate()).map(|(i, x)| (i, x.id(), x.name(), &[][..]));
    pick(model, T::KIND, found, subject)
}

/// `items` by order, then name, then ID.
pub(super) fn in_order<T: Named>(items: &[T]) -> Vec<&T> {
    let mut items: Vec<&T> = items.iter().collect();
    items.sort_by_key(|x| (x.order(), x.name(), x.id()));
    items
}

/// A saved view as `model read` shows it, its ID qualified.
#[derive(Debug, Serialize)]
pub struct ViewEntry<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub reading: &'a BeatReading,
}

impl Model {
    /// The saved views in order.
    pub fn view_entries(&self) -> Vec<ViewEntry<'_>> {
        (in_order(&self.views).into_iter())
            .map(|v| self.view_entry(v))
            .collect()
    }

    pub fn view_entry<'a>(&self, view: &'a SavedView) -> ViewEntry<'a> {
        ViewEntry {
            id: ids::qualify(&self.manifest.id, &view.id),
            name: &view.name,
            order: view.order,
            description: view.description.as_deref(),
            reading: &view.reading,
        }
    }

    /// The saved view `subject` names, its qualified or local ID or its unique name.
    pub fn saved_view(&self, subject: &str) -> Result<&SavedView, Refusal> {
        let at = position(&self.manifest.id, &self.views, subject)?;
        Ok(&self.views[at])
    }
}
