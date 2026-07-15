use stone::relation::{Dependency, Kind as StoneRelationKind, Provider};

use super::CanonicalEncoder;

/// A typed package relation carried across the derivation freeze boundary.
///
/// The kind and target are stored separately so execution never has to parse
/// authored `kind(target)` syntax. The same canonical value can be lowered
/// infallibly to either Stone relation role after
/// [`DerivationPlan::validate`](super::DerivationPlan::validate).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelationPlan {
    pub kind: RelationKind,
    pub name: String,
}

impl RelationPlan {
    pub fn to_dependency(&self) -> Dependency {
        Dependency {
            kind: self.kind.into(),
            name: self.name.clone(),
        }
    }

    pub fn to_provider(&self) -> Provider {
        Provider {
            kind: self.kind.into(),
            name: self.name.clone(),
        }
    }

    pub fn canonical_name(&self) -> String {
        self.to_dependency().to_name()
    }

    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.variant(self.kind as u8);
        encoder.string(&self.name);
    }
}

impl From<&Dependency> for RelationPlan {
    fn from(relation: &Dependency) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name.clone(),
        }
    }
}

impl From<Dependency> for RelationPlan {
    fn from(relation: Dependency) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name,
        }
    }
}

impl From<&Provider> for RelationPlan {
    fn from(relation: &Provider) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name.clone(),
        }
    }
}

impl From<Provider> for RelationPlan {
    fn from(relation: Provider) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name,
        }
    }
}

/// Capability namespace retained explicitly in a frozen relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum RelationKind {
    PackageName = 0,
    SharedLibrary = 1,
    PkgConfig = 2,
    Interpreter = 3,
    CMake = 4,
    Python = 5,
    Binary = 6,
    SystemBinary = 7,
    PkgConfig32 = 8,
}

impl From<StoneRelationKind> for RelationKind {
    fn from(kind: StoneRelationKind) -> Self {
        match kind {
            StoneRelationKind::PackageName => Self::PackageName,
            StoneRelationKind::SharedLibrary => Self::SharedLibrary,
            StoneRelationKind::PkgConfig => Self::PkgConfig,
            StoneRelationKind::Interpreter => Self::Interpreter,
            StoneRelationKind::CMake => Self::CMake,
            StoneRelationKind::Python => Self::Python,
            StoneRelationKind::Binary => Self::Binary,
            StoneRelationKind::SystemBinary => Self::SystemBinary,
            StoneRelationKind::PkgConfig32 => Self::PkgConfig32,
        }
    }
}

impl From<RelationKind> for StoneRelationKind {
    fn from(kind: RelationKind) -> Self {
        match kind {
            RelationKind::PackageName => Self::PackageName,
            RelationKind::SharedLibrary => Self::SharedLibrary,
            RelationKind::PkgConfig => Self::PkgConfig,
            RelationKind::Interpreter => Self::Interpreter,
            RelationKind::CMake => Self::CMake,
            RelationKind::Python => Self::Python,
            RelationKind::Binary => Self::Binary,
            RelationKind::SystemBinary => Self::SystemBinary,
            RelationKind::PkgConfig32 => Self::PkgConfig32,
        }
    }
}
