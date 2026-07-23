use std::fmt;

use crate::package::DependencySpec;

/// The authored package field in which a dependency is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyRole {
    /// An executable capability required by the selected builder.
    BuilderTool,
    /// A capability required for the build platform.
    NativeBuild,
    /// A capability required for the target platform while building.
    Build,
    /// A capability required while checking the package.
    Check,
    /// A capability required by one installed output.
    Runtime,
    /// A provider capability which conflicts with one installed output.
    Conflict,
}

impl DependencyRole {
    pub(super) fn accepts(self, kind: DependencyKind) -> bool {
        match self {
            Self::BuilderTool => matches!(
                kind,
                DependencyKind::Package
                    | DependencyKind::Output
                    | DependencyKind::Binary
                    | DependencyKind::SystemBinary
            ),
            Self::Runtime => !matches!(
                kind,
                DependencyKind::CMake | DependencyKind::PkgConfig | DependencyKind::PkgConfig32
            ),
            Self::NativeBuild | Self::Build | Self::Check | Self::Conflict => true,
        }
    }

    pub(super) fn is_provider(self) -> bool {
        matches!(self, Self::Conflict)
    }
}

impl fmt::Display for DependencyRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BuilderTool => "builder-tool",
            Self::NativeBuild => "native-build",
            Self::Build => "build",
            Self::Check => "check",
            Self::Runtime => "runtime",
            Self::Conflict => "conflict",
        })
    }
}

/// The authored capability kind carried by a typed dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Package,
    Output,
    Binary,
    SystemBinary,
    PkgConfig,
    PkgConfig32,
    Soname,
    CMake,
    Python,
    Interpreter,
}

impl DependencyKind {
    pub(super) fn of(dependency: &DependencySpec) -> Self {
        match dependency {
            DependencySpec::Package(_) => Self::Package,
            DependencySpec::Output(_) => Self::Output,
            DependencySpec::Binary(_) => Self::Binary,
            DependencySpec::SystemBinary(_) => Self::SystemBinary,
            DependencySpec::PkgConfig(_) => Self::PkgConfig,
            DependencySpec::PkgConfig32(_) => Self::PkgConfig32,
            DependencySpec::Soname(_) => Self::Soname,
            DependencySpec::CMake(_) => Self::CMake,
            DependencySpec::Python(_) => Self::Python,
            DependencySpec::Interpreter(_) => Self::Interpreter,
        }
    }
}

impl fmt::Display for DependencyKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Package => "package",
            Self::Output => "output",
            Self::Binary => "binary",
            Self::SystemBinary => "system-binary",
            Self::PkgConfig => "pkgconfig",
            Self::PkgConfig32 => "pkgconfig32",
            Self::Soname => "soname",
            Self::CMake => "cmake",
            Self::Python => "python",
            Self::Interpreter => "interpreter",
        })
    }
}
