//! The diagnostics `model check` reports, shared by the semantic check and the code check.

use serde::{Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticCode {
    ContainerUnbound,
    ContainerPartsOutside,
    ContainerPassThrough,
    RootUnconnected,
    BindingShared,
    RelationshipRepeated,
    ContainerLineToOwnPart,
    RelationshipOnContainer,
    FlowSummaryMissing,
    MessageUndrawn,
    WalkthroughTargetMissing,
    WalkthroughEmpty,
    PictureTargetMissing,
    PictureStale,
    PictureMissing,
    PictureUnrenderable,
    BindingPathMissing,
    BindingSymbolMissing,
    AnchorPathMissing,
    AnchorSymbolMissing,
    RelationshipUndrawn,
    RelationshipUnsupported,
    RelationshipBackwards,
    EntryPointUnexposed,
    ScanFailed,
}

impl DiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContainerUnbound => "CONTAINER_UNBOUND",
            Self::ContainerPartsOutside => "CONTAINER_PARTS_OUTSIDE",
            Self::ContainerPassThrough => "CONTAINER_PASS_THROUGH",
            Self::RootUnconnected => "ROOT_UNCONNECTED",
            Self::BindingShared => "BINDING_SHARED",
            Self::RelationshipRepeated => "RELATIONSHIP_REPEATED",
            Self::ContainerLineToOwnPart => "CONTAINER_LINE_TO_OWN_PART",
            Self::RelationshipOnContainer => "RELATIONSHIP_ON_CONTAINER",
            Self::FlowSummaryMissing => "FLOW_SUMMARY_MISSING",
            Self::MessageUndrawn => "MESSAGE_UNDRAWN",
            Self::WalkthroughTargetMissing => "WALKTHROUGH_TARGET_MISSING",
            Self::WalkthroughEmpty => "WALKTHROUGH_EMPTY",
            Self::PictureTargetMissing => "PICTURE_TARGET_MISSING",
            Self::PictureStale => "PICTURE_STALE",
            Self::PictureMissing => "PICTURE_MISSING",
            Self::PictureUnrenderable => "PICTURE_UNRENDERABLE",
            Self::BindingPathMissing => "BINDING_PATH_MISSING",
            Self::BindingSymbolMissing => "BINDING_SYMBOL_MISSING",
            Self::AnchorPathMissing => "ANCHOR_PATH_MISSING",
            Self::AnchorSymbolMissing => "ANCHOR_SYMBOL_MISSING",
            Self::RelationshipUndrawn => "RELATIONSHIP_UNDRAWN",
            Self::RelationshipUnsupported => "RELATIONSHIP_UNSUPPORTED",
            Self::RelationshipBackwards => "RELATIONSHIP_BACKWARDS",
            Self::EntryPointUnexposed => "ENTRY_POINT_UNEXPOSED",
            Self::ScanFailed => "SCAN_FAILED",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::ContainerPassThrough
            | Self::RelationshipOnContainer
            | Self::FlowSummaryMissing
            | Self::MessageUndrawn
            | Self::WalkthroughEmpty
            | Self::EntryPointUnexposed => Severity::Information,
            Self::ContainerUnbound
            | Self::ContainerPartsOutside
            | Self::RootUnconnected
            | Self::BindingShared
            | Self::RelationshipRepeated
            | Self::ContainerLineToOwnPart
            | Self::WalkthroughTargetMissing
            | Self::PictureTargetMissing
            | Self::PictureStale
            | Self::PictureMissing
            | Self::PictureUnrenderable
            | Self::BindingPathMissing
            | Self::BindingSymbolMissing
            | Self::AnchorPathMissing
            | Self::AnchorSymbolMissing
            | Self::RelationshipUndrawn
            | Self::RelationshipUnsupported
            | Self::RelationshipBackwards
            | Self::ScanFailed => Severity::Warning,
        }
    }
}

impl Serialize for DiagnosticCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A warning fails the check; information never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Information,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    /// Qualified IDs of the modules, relationships, flows, steps, walkthroughs, beats or pictures
    /// the diagnostic concerns.
    pub subjects: Vec<String>,
    pub message: String,
    /// For `CONTAINER_UNBOUND`: the directory its bound parts share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_binding: Option<String>,
    /// The repository file the evidence points at: a binding or anchor path, or the file an
    /// undrawn import, call or configured address is written in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The 1-based line in `file`, when the evidence has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

impl Diagnostic {
    pub fn new(code: DiagnosticCode, subjects: Vec<String>, message: String) -> Self {
        Self {
            code,
            severity: code.severity(),
            subjects,
            message,
            suggested_binding: None,
            file: None,
            line: None,
        }
    }

    /// The diagnostic pointing at `file`, and at `line` in it when known.
    pub fn at(mut self, file: &str, line: Option<usize>) -> Self {
        self.file = Some(file.to_owned());
        self.line = line;
        self
    }
}
