//! Cast surface for the shared Stone relation model.
//!
//! New code should import these types from [`stone::relation`] directly. This
//! module remains while Cast internals and downstream users migrate without
//! carrying a second parser or representation.

pub use stone::relation::{Dependency, Kind, ParseError, Provider};
