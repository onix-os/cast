// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Moss compatibility surface for the shared Stone relation model.
//!
//! New code should import these types from [`stone::relation`] directly. This
//! module remains while Moss internals and downstream users migrate without
//! carrying a second parser or representation.

pub use stone::relation::{Dependency, Kind, ParseError, Provider};
