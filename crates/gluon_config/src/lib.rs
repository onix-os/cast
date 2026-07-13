// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Restricted Gluon evaluation for repository-owned declarative configuration.
//!
//! This crate intentionally constructs an empty [`gluon::RootedThread`]. It
//! avoids convenience VM builders and ambient importers because those expose
//! host I/O primitives and process-wide import paths.

mod diagnostic;
mod evaluator;
mod fingerprint;
mod import;
mod limits;
mod source;

pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use evaluator::{Evaluation, Evaluator};
pub use fingerprint::{EvaluationFingerprint, EvaluationFingerprintValidationError, ModuleFingerprint};
pub use import::ImportPolicy;
pub use limits::Limits;
pub use source::{Source, SourceRoot};

/// The exact Gluon release which defines this evaluator's language behavior.
pub const GLUON_VERSION: &str = "0.18.3";

/// Version of the Rust/Gluon configuration boundary.
pub const CONFIGURATION_ABI_VERSION: u32 = 1;

/// Version of the evaluator's security and determinism policy.
pub const EVALUATOR_POLICY_VERSION: u32 = 1;
