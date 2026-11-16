//! The import statements a native collector reads from source.
use crate::model::EdgeKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedName {
    pub imported: String,
    pub local: String,
    pub type_only: bool,
}

#[derive(Debug, Clone)]
pub struct ImportStatement {
    pub specifier: Option<String>,
    pub line: usize,
    pub kind: EdgeKind,
    pub names: Vec<ImportedName>,
    pub reexport: bool,
    pub wildcard: bool,
    pub side_effect: bool,
    pub require: bool,
}
