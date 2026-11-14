//! The modules of one model indexed by qualified ID, shared by the semantic and code checks.

use std::collections::BTreeMap;

use super::ids;
use super::types::{Anchor, Binding, Module};

/// One relationship, with the qualified IDs of itself and its source.
pub(super) struct Line<'a> {
    pub(super) id: String,
    pub(super) from: String,
    pub(super) to: &'a str,
    pub(super) kind: &'a str,
    pub(super) anchor: Option<&'a Anchor>,
}

/// The modules of one model by qualified ID, with their containment and relationships.
pub(super) struct Index<'a> {
    pub(super) by_id: BTreeMap<String, &'a Module>,
    pub(super) roots: Vec<(String, &'a Module)>,
    /// Modules keyed by their parent's qualified ID.
    parts: BTreeMap<&'a str, Vec<(String, &'a Module)>>,
    pub(super) lines: Vec<Line<'a>>,
}

impl<'a> Index<'a> {
    pub(super) fn new(model: &str, modules: &'a [Module]) -> Self {
        let mut index = Self {
            by_id: BTreeMap::new(),
            roots: Vec::new(),
            parts: BTreeMap::new(),
            lines: Vec::new(),
        };
        for module in modules {
            let id = ids::qualify(model, &module.id);
            match module.parent.as_deref() {
                Some(parent) => index.parts.entry(parent).or_default(),
                None => &mut index.roots,
            }
            .push((id.clone(), module));
            index.lines.extend(module.outgoing.iter().map(|r| Line {
                id: ids::qualify(model, &r.id),
                from: id.clone(),
                to: &r.to,
                kind: &r.kind,
                anchor: r.anchor.as_ref(),
            }));
            index.by_id.insert(id, module);
        }
        index
    }

    pub(super) fn name<'s>(&'s self, id: &'s str) -> &'s str {
        self.by_id.get(id).map_or(id, |m| m.name.as_str())
    }

    pub(super) fn parts_of(&self, id: &str) -> &[(String, &'a Module)] {
        self.parts.get(id).map_or(&[], Vec::as_slice)
    }

    pub(super) fn is_container(&self, id: &str) -> bool {
        !self.parts_of(id).is_empty()
    }

    /// Every module below `id`, at any depth.
    pub(super) fn descendants(&self, id: &str) -> Vec<(&str, &'a Module)> {
        let mut found = Vec::new();
        let mut stack: Vec<&(String, &'a Module)> = self.parts_of(id).iter().collect();
        while let Some((part, module)) = stack.pop() {
            stack.extend(self.parts_of(part));
            found.push((part.as_str(), *module));
        }
        found
    }

    /// The closest module above `id` that has a binding, with that binding.
    pub(super) fn bound_ancestor(&self, id: &str) -> Option<(&str, &'a Module, &'a Binding)> {
        let module: &'a Module = self.by_id.get(id)?;
        let mut at = module.parent.as_deref();
        while let Some(parent) = at {
            let (key, &above) = self.by_id.get_key_value(parent)?;
            if let Some(binding) = &above.binding {
                return Some((key, above, binding));
            }
            at = above.parent.as_deref();
        }
        None
    }

    /// Whether `ancestor` contains `id`, at any depth.
    pub(super) fn is_below(&self, id: &str, ancestor: &str) -> bool {
        let mut at = self.by_id.get(id).and_then(|m| m.parent.as_deref());
        while let Some(parent) = at {
            if parent == ancestor {
                return true;
            }
            at = self.by_id.get(parent).and_then(|m| m.parent.as_deref());
        }
        false
    }

    /// `id` and every module above it, innermost first.
    pub(super) fn chain<'s>(&'s self, id: &'s str) -> Vec<&'s str> {
        let mut chain = vec![id];
        let mut at = self.by_id.get(id).and_then(|m| m.parent.as_deref());
        while let Some(parent) = at {
            chain.push(parent);
            at = self.by_id.get(parent).and_then(|m| m.parent.as_deref());
        }
        chain
    }
}
