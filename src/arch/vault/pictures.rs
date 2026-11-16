//! Pictures in the vault: their validation and how `model read` lists them. A picture belongs to
//! the current state and names a reading, as a walkthrough beat does, and the repository file
//! Boundrail draws it to.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{Contents, Model, Problems, STATE, is_repo_path, views};
use crate::arch::types::{BeatReading, Picture, VaultConfig};
use crate::arch::{Refusal, ids};

/// Every picture's own fields and reading, and that no two write the same file.
pub(super) fn check<'a>(
    model: Option<&str>,
    config: Option<&VaultConfig>,
    contents: Contents<'a>,
    problems: &mut Problems,
) {
    // Case-insensitive file systems would make two spellings one file.
    let mut outputs: BTreeMap<String, &str> = BTreeMap::new();
    let own = |file: &str, picture: &'a Picture, problems: &mut Problems| {
        let id = picture.id.as_str();
        for message in output_problems(&picture.output) {
            problems.push(file, id, message);
        }
        if let Some(other) = outputs.insert(picture.output.to_ascii_lowercase(), id) {
            let message = format!("output {} is also picture {other}'s", picture.output);
            problems.push(file, id, message);
        }
    };
    views::check(model, config, contents, contents.pictures, problems, own);
}

/// Why `output` is not a file a picture may write: a relative repository path with no empty or
/// `.` segment, ending in `.svg` or `.png`, outside `.boundrail` and any `.git`. Segments compare
/// as Windows resolves them: any case, trailing dots and spaces dropped, and `git~1` naming
/// `.git`.
pub(crate) fn output_problems(output: &str) -> Vec<String> {
    let mut out = Vec::new();
    let segments: Vec<&str> = output.split('/').collect();
    if !is_repo_path(output) || segments.iter().any(|s| s.is_empty() || *s == ".") {
        out.push(format!(
            "output {output} is not a relative path inside the repository"
        ));
    }
    if !(output.ends_with(".svg") || output.ends_with(".png")) {
        out.push(format!("output {output} does not end in .svg or .png"));
    }
    let bare = |s: &str| s.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    let git = |s: &&str| matches!(bare(s).as_str(), ".git" | "git~1");
    if segments.first().is_some_and(|s| bare(s) == STATE) || segments.iter().any(git) {
        out.push(format!(
            "output {output} is inside {STATE} or .git; write it elsewhere"
        ));
    }
    out
}

/// A picture as `model read` shows it, its ID qualified.
#[derive(Debug, Serialize)]
pub struct PictureEntry<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub output: &'a str,
    pub reading: &'a BeatReading,
}

impl Model {
    /// The pictures in order.
    pub fn picture_entries(&self) -> Vec<PictureEntry<'_>> {
        (self.pictures_in_order().into_iter())
            .map(|p| self.picture_entry(p))
            .collect()
    }

    /// The pictures by order, then name, then ID.
    pub fn pictures_in_order(&self) -> Vec<&Picture> {
        views::in_order(&self.pictures)
    }

    pub fn picture_entry<'a>(&self, picture: &'a Picture) -> PictureEntry<'a> {
        PictureEntry {
            id: ids::qualify(&self.manifest.id, &picture.id),
            name: &picture.name,
            order: picture.order,
            description: picture.description.as_deref(),
            output: &picture.output,
            reading: &picture.reading,
        }
    }

    /// The picture `subject` names, its qualified or local ID or its unique name.
    pub fn picture(&self, subject: &str) -> Result<&Picture, Refusal> {
        let at = views::position(&self.manifest.id, &self.pictures, subject)?;
        Ok(&self.pictures[at])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::arch::types::{Manifest, Picture, VaultConfig};

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "schema_version": 1, "id": "Model001", "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap()
    }

    fn picture(id: &str, output: &str, reading: Value) -> Picture {
        serde_json::from_value(json!({
            "schema_version": 1, "id": id, "name": id, "order": 0,
            "output": output, "reading": reading,
        }))
        .unwrap()
    }

    fn messages(pictures: &[Picture]) -> Vec<String> {
        let found = super::super::problems(
            &manifest(),
            &VaultConfig::default(),
            None,
            &[],
            &[],
            (&[], pictures, &[]),
        );
        found.into_iter().map(|p| p.message).collect()
    }

    #[test]
    fn outputs_are_svg_or_png_files_inside_the_repository() {
        for good in [
            "docs/shop.svg",
            "shop.png",
            "docs/.hidden/a.svg",
            "docs/git/a.png",
        ] {
            assert_eq!(super::output_problems(good), Vec::<String>::new(), "{good}");
        }
        let outside = "is not a relative path inside the repository";
        for (bad, expected) in [
            ("/abs/shop.svg", outside),
            ("../shop.svg", outside),
            ("docs/../../shop.svg", outside),
            ("docs//shop.svg", outside),
            ("./shop.svg", outside),
            ("c:/shop.svg", outside),
            ("docs\\shop.svg", outside),
            ("docs/shop.jpg", "does not end in .svg or .png"),
            ("docs/shop.SVG", "does not end in .svg or .png"),
            (".boundrail/shop.svg", "is inside .boundrail or .git"),
            (".git/shop.svg", "is inside .boundrail or .git"),
            (".git./shop.svg", "is inside .boundrail or .git"),
            (".git . /shop.svg", "is inside .boundrail or .git"),
            ("GIT~1/shop.svg", "is inside .boundrail or .git"),
            (".Boundrail./shop.svg", "is inside .boundrail or .git"),
            ("vendor/lib/.GIT/shop.svg", "is inside .boundrail or .git"),
        ] {
            let problems = super::output_problems(bad);
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{bad}: {problems:?}"
            );
        }
    }

    #[test]
    fn pictures_keep_the_reading_rules_and_write_distinct_files() {
        let architecture = json!({"view": "architecture"});
        let same = [
            picture("Pict0001", "docs/a.svg", architecture.clone()),
            picture("Pict0002", "docs/a.svg", json!({"view": "sequence"})),
        ];
        let mut upper = same.clone();
        upper[1].output = "DOCS/A.svg".to_owned();
        upper[1].reading = upper[0].reading.clone();
        assert_eq!(
            messages(&upper),
            ["output DOCS/A.svg is also picture Pict0001's"]
        );
        assert_eq!(
            messages(&same),
            [
                "output docs/a.svg is also picture Pict0001's",
                "the sequence view needs a flow",
            ]
        );
        let missing = [picture(
            "Pict0001",
            "docs/a.png",
            json!({"view": "architecture", "focus": ["Model001#Gone0001"]}),
        )];
        assert_eq!(
            messages(&missing),
            ["focus Model001#Gone0001 is not a module"]
        );
    }
}
