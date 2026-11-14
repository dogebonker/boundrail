//! The authored architecture model: modules and relationships that agents write in
//! `.boundrail/model/` and that Boundrail checks against the code.

pub mod ids;
pub mod revision;
pub mod types;
pub mod vault;

use std::fmt;

use serde::{Serialize, Serializer};
use serde_json::Value;

/// Why a model command declined to act. The CLI prints it on stdout and exits with code 2.
#[derive(Debug)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    NameInvalid,
    CheckoutNotFound,
    VaultNotFound,
    VaultExists,
    ModelInvalid,
    SubjectNotFound,
    SubjectAmbiguous,
    SchemaNotFound,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NameInvalid => "NAME_INVALID",
            Code::CheckoutNotFound => "CHECKOUT_NOT_FOUND",
            Code::VaultNotFound => "VAULT_NOT_FOUND",
            Code::VaultExists => "VAULT_EXISTS",
            Code::ModelInvalid => "MODEL_INVALID",
            Code::SubjectNotFound => "SUBJECT_NOT_FOUND",
            Code::SubjectAmbiguous => "SUBJECT_AMBIGUOUS",
            Code::SchemaNotFound => "SCHEMA_NOT_FOUND",
        }
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl Refusal {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}
