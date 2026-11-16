//! The package an import specifier names.

/// The package a bare specifier names (`@scope/name` or `name`), or `None` for a relative,
/// absolute, subpath-import or empty one.
pub(crate) fn package_name(specifier: &str) -> Option<String> {
    if specifier.starts_with(['.', '/', '#', '~']) || specifier.is_empty() {
        return None;
    }
    let mut segments = specifier.split('/');
    let first = segments.next()?;
    if first.starts_with('@') {
        Some(format!("{first}/{}", segments.next()?))
    } else {
        Some(first.into())
    }
}
