// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Canonical package dependency and provider relations shared by Stone,
//! Mason, Forge, and declarative package evaluation.

use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::StonePayloadMetaDependency;

/// The capability namespace of a dependency or provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum Kind {
    #[strum(serialize = "name")]
    PackageName,
    #[strum(serialize = "soname")]
    SharedLibrary,
    PkgConfig,
    Interpreter,
    CMake,
    Python,
    Binary,
    #[strum(serialize = "sysbinary")]
    SystemBinary,
    PkgConfig32,
}

impl Kind {
    pub fn from_stone_dependency(dependency: StonePayloadMetaDependency) -> Option<Self> {
        Some(match dependency {
            StonePayloadMetaDependency::PackageName => Self::PackageName,
            StonePayloadMetaDependency::SharedLibrary => Self::SharedLibrary,
            StonePayloadMetaDependency::PkgConfig => Self::PkgConfig,
            StonePayloadMetaDependency::Interpreter => Self::Interpreter,
            StonePayloadMetaDependency::CMake => Self::CMake,
            StonePayloadMetaDependency::Python => Self::Python,
            StonePayloadMetaDependency::Binary => Self::Binary,
            StonePayloadMetaDependency::SystemBinary => Self::SystemBinary,
            StonePayloadMetaDependency::PkgConfig32 => Self::PkgConfig32,
            StonePayloadMetaDependency::Unknown => return None,
        })
    }
}

impl From<Kind> for StonePayloadMetaDependency {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::PackageName => Self::PackageName,
            Kind::SharedLibrary => Self::SharedLibrary,
            Kind::PkgConfig => Self::PkgConfig,
            Kind::Interpreter => Self::Interpreter,
            Kind::CMake => Self::CMake,
            Kind::Python => Self::Python,
            Kind::Binary => Self::Binary,
            Kind::SystemBinary => Self::SystemBinary,
            Kind::PkgConfig32 => Self::PkgConfig32,
        }
    }
}

macro_rules! relation_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            pub kind: Kind,
            pub name: String,
        }

        impl $name {
            /// Construct a validated typed relation.
            pub fn new(kind: Kind, name: impl Into<String>) -> Result<Self, ParseError> {
                let name = name.into();
                validate_target(&name, &format!("{kind}({name})"))?;
                Ok(Self { kind, name })
            }

            /// Construct the internal package-name form.
            ///
            /// External strings should enter through [`Self::from_name`] so
            /// malformed or empty values are rejected.
            pub fn package_name(name: impl Into<String>) -> Self {
                Self {
                    kind: Kind::PackageName,
                    name: name.into(),
                }
            }

            /// Parse either a bare package name or a typed `kind(target)` relation.
            pub fn from_name(value: &str) -> Result<Self, ParseError> {
                if value.contains('(') {
                    Self::from_str(value)
                } else {
                    validate_bare(value)?;
                    Ok(Self::package_name(value))
                }
            }

            /// Emit the canonical authored form. Package-name relations remain bare.
            pub fn to_name(&self) -> String {
                if self.kind == Kind::PackageName {
                    self.name.clone()
                } else {
                    self.to_string()
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", self.kind, self.name)
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.to_string().cmp(&other.to_string())
            }
        }

        impl FromStr for $name {
            type Err = ParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let (kind, name) = parse(value)?;
                Ok(Self { kind, name })
            }
        }

        impl TryFrom<String> for $name {
            type Error = ParseError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::from_name(&value)
            }
        }
    };
}

relation_type!(Dependency);
relation_type!(Provider);

fn validate_bare(value: &str) -> Result<(), ParseError> {
    if value.is_empty() {
        Err(ParseError::EmptyTarget {
            value: value.to_owned(),
        })
    } else if value.contains(')') {
        Err(ParseError::Malformed {
            value: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_target(target: &str, value: &str) -> Result<(), ParseError> {
    if target.is_empty() {
        return Err(ParseError::EmptyTarget {
            value: value.to_owned(),
        });
    }

    let mut depth = 0_u32;
    for character in target.chars() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => {
                return Err(ParseError::Malformed {
                    value: value.to_owned(),
                });
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    if depth != 0 {
        return Err(ParseError::Malformed {
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn parse(value: &str) -> Result<(Kind, String), ParseError> {
    let (kind, rest) = value.split_once('(').ok_or_else(|| ParseError::Malformed {
        value: value.to_owned(),
    })?;
    if !rest.ends_with(')') {
        return Err(ParseError::Malformed {
            value: value.to_owned(),
        });
    }

    let kind = kind
        .parse::<Kind>()
        .map_err(|_| ParseError::UnsupportedKind { kind: kind.to_owned() })?;
    let name = &rest[..rest.len() - 1];
    validate_target(name, value)?;
    Ok((kind, name.to_owned()))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("invalid package relation `{value}`: target must not be empty")]
    EmptyTarget { value: String },
    #[error("invalid package relation `{value}`: expected a package name or supported kind(target)")]
    Malformed { value: String },
    #[error("invalid package relation kind `{kind}`")]
    UnsupportedKind { kind: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_typed_and_nested_targets_canonically() {
        assert_eq!(Dependency::from_name("zlib").unwrap().to_name(), "zlib");
        assert_eq!(
            Provider::from_name("pkgconfig(zlib)").unwrap().to_name(),
            "pkgconfig(zlib)"
        );
        assert_eq!(
            Dependency::from_name("soname(libz.so.1(x86_64))").unwrap().name,
            "libz.so.1(x86_64)"
        );
    }

    #[test]
    fn rejects_empty_unknown_and_unbalanced_relations() {
        assert!(matches!(Dependency::from_name(""), Err(ParseError::EmptyTarget { .. })));
        assert!(matches!(
            Dependency::from_name("binary()"),
            Err(ParseError::EmptyTarget { .. })
        ));
        assert!(matches!(
            Dependency::from_name("unknown(value)"),
            Err(ParseError::UnsupportedKind { .. })
        ));
        assert!(matches!(
            Dependency::from_name("soname(lib.so)extra)"),
            Err(ParseError::Malformed { .. })
        ));
    }

    #[test]
    fn stone_kind_mapping_round_trips_all_supported_values() {
        for kind in [
            Kind::PackageName,
            Kind::SharedLibrary,
            Kind::PkgConfig,
            Kind::Interpreter,
            Kind::CMake,
            Kind::Python,
            Kind::Binary,
            Kind::SystemBinary,
            Kind::PkgConfig32,
        ] {
            assert_eq!(Kind::from_stone_dependency(kind.into()), Some(kind));
        }
        assert_eq!(Kind::from_stone_dependency(StonePayloadMetaDependency::Unknown), None);
    }
}
