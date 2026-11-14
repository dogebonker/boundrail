//! The revision a write names: a SHA-256 per recognized vault file and one digest over them.

use serde::Serialize;

use crate::sync_rules::hash;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Revision {
    pub digest: String,
    pub files: Vec<FileDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDigest {
    pub path: String,
    pub sha256: String,
}

/// `files` pairs vault-relative paths with their contents; the order does not matter.
pub fn of<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Revision {
    let mut files: Vec<FileDigest> = files
        .into_iter()
        .map(|(path, bytes)| FileDigest {
            path: path.to_owned(),
            sha256: hash(bytes),
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let lines: String = files
        .iter()
        .map(|f| format!("{}\0{}\n", f.path, f.sha256))
        .collect();
    Revision {
        digest: hash(lines.as_bytes()),
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_covers_sorted_path_and_hash_lines() {
        let revision = of([
            ("modules/b.module.json", b"b".as_slice()),
            ("model.json", b"m".as_slice()),
        ]);
        let paths: Vec<_> = revision.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["model.json", "modules/b.module.json"]);
        let expected = format!(
            "model.json\0{}\nmodules/b.module.json\0{}\n",
            hash(b"m"),
            hash(b"b")
        );
        assert_eq!(revision.digest, hash(expected.as_bytes()));
    }

    #[test]
    fn digest_changes_with_content_and_path_but_not_order() {
        let base = of([("a", b"1".as_slice()), ("b", b"2".as_slice())]);
        assert_eq!(base, of([("b", b"2".as_slice()), ("a", b"1".as_slice())]));
        assert_ne!(
            base.digest,
            of([("a", b"1".as_slice()), ("b", b"3".as_slice())]).digest
        );
        assert_ne!(
            base.digest,
            of([("a", b"1".as_slice()), ("c", b"2".as_slice())]).digest
        );
    }
}
