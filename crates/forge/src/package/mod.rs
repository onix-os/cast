// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::borrow::Borrow;

use astr::AStr;
use derive_more::{Debug, Display, From, Into};
use itertools::Itertools;

pub use self::meta::{Meta, MissingMetaFieldError, Name, RepositoryMetaError};

pub mod meta;
pub mod render;

/// Return whether one normalized, raw `/usr`-relative package target belongs
/// to Cast's system-metadata namespace.
///
/// Stone layout targets omit the leading `/usr/`. Packages must never own the
/// state or tree-identity markers, their atomic-publication temporaries, the
/// generated candidate metadata files, nor anything beneath those names if a
/// conflicting package layout tries to turn one into a directory. This
/// predicate deliberately reserves only the exact system-owned paths; similar
/// package names and the package-owned `lib/os-info.json` input remain
/// available.
pub fn is_reserved_usr_layout_target(target: &str) -> bool {
    [
        ".cast-state-id.tmp",
        ".cast-tree-id",
        ".cast-tree-id.tmp",
        ".stateID",
        "lib/os-release",
        "lib/system-model.glu",
    ]
    .into_iter()
    .any(|reserved| {
        target == reserved
            || target
                .strip_prefix(reserved)
                .is_some_and(|remainder| remainder.starts_with('/'))
    })
}

/// Unique ID of a [`Package`]
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, From, Into, Display)]
#[debug("{_0:?}")]
pub struct Id(AStr);

impl Id {
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Id> for meta::Id {
    fn from(id: Id) -> Self {
        meta::Id(id.0)
    }
}

impl From<meta::Id> for Id {
    fn from(id: meta::Id) -> Self {
        Self(id.0)
    }
}

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self(AStr::from(value))
    }
}

#[cfg(test)]
impl From<&'static str> for Id {
    fn from(value: &'static str) -> Self {
        Self(value.into())
    }
}

impl Borrow<str> for Id {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub id: Id,
    pub meta: Meta,
    pub flags: Flags,
}

impl PartialOrd for Package {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Package {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.meta
            .source_release
            .cmp(&other.meta.source_release)
            .reverse()
            .then_with(|| self.meta.build_release.cmp(&other.meta.build_release).reverse())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// Package is available for installation.
    pub available: bool,
    /// Package is already installed.
    pub installed: bool,
    /// Available as from-source build.
    pub source: bool,
    /// Package is explicitly installed (use with [`Flags::installed`]).
    pub explicit: bool,
}

impl Flags {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy of [`Flags`] with available set to true.
    pub fn with_available(&self) -> Self {
        Self {
            available: true,
            ..*self
        }
    }

    /// Returns a copy of [`Flags`] with installed set to true.
    pub fn with_installed(&self) -> Self {
        Self {
            installed: true,
            ..*self
        }
    }

    /// Returns a copy of [`Flags`] with source set to true.
    pub fn with_source(&self) -> Self {
        Self { source: true, ..*self }
    }

    /// Returns a copy of [`Flags`] with explicit set to true.
    pub fn with_explicit(&self) -> Self {
        Self {
            explicit: true,
            ..*self
        }
    }

    /// Returns whether this flag set contains another flag set.
    pub fn contains(&self, other: Self) -> bool {
        (self.bits() & other.bits()) == other.bits()
    }

    fn bits(&self) -> u32 {
        (self.available as u32)
            | ((self.installed as u32) << 1)
            | ((self.source as u32) << 2)
            | ((self.explicit as u32) << 3)
    }
}

/// Iterate packages in sorted order
pub struct Sorted<I>(I);

impl<I> Sorted<I> {
    pub fn new(iter: I) -> Self {
        Self(iter)
    }
}

/// Iterate in sorted order
impl<I, T> IntoIterator for Sorted<I>
where
    I: IntoIterator<Item = T>,
    T: Ord,
{
    type Item = T;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().sorted()
    }
}

/// A package being updated from `old` to `new`
pub struct Update<'a> {
    pub old: &'a Package,
    pub new: &'a Package,
}

#[cfg(test)]
mod tests {
    use super::is_reserved_usr_layout_target;

    #[test]
    fn system_metadata_reservation_is_exact_and_descendant_aware() {
        for target in [
            ".cast-state-id.tmp",
            ".cast-state-id.tmp/child",
            ".cast-tree-id",
            ".cast-tree-id/child",
            ".cast-tree-id/nested/child",
            ".cast-tree-id.tmp",
            ".cast-tree-id.tmp/child",
            ".stateID",
            ".stateID/child",
            ".stateID/nested/child",
            "lib/os-release",
            "lib/os-release/child",
            "lib/os-release/nested/child",
            "lib/system-model.glu",
            "lib/system-model.glu/child",
            "lib/system-model.glu/nested/child",
        ] {
            assert!(is_reserved_usr_layout_target(target), "did not reserve {target:?}");
        }

        for target in [
            ".cast-state-id.tmp-old",
            ".cast-state-id.tmp.old/child",
            ".cast-tree",
            ".cast-tree-id-old",
            ".cast-tree-id.old/child",
            ".cast-tree-id.tmp-old",
            ".cast-tree-id.tmp.old/child",
            ".state",
            ".stateID-old",
            ".stateID.old/child",
            "lib",
            "lib/os-info.json",
            "lib/os-release-old",
            "lib/os-release.local/child",
            "lib/system-model.glu-old",
            "lib/system-model.glu.local/child",
            "share/lib/os-release",
            "share/lib/system-model.glu",
            "share/.cast-tree-id",
            "share/.stateID",
        ] {
            assert!(!is_reserved_usr_layout_target(target), "over-reserved {target:?}");
        }
    }
}
