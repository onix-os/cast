// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Primitive, format-neutral values shared by the package and policy ABIs.

/// Recipe-wide build options selected by a concrete package declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsSpec {
    pub toolchain: ToolchainSpec,
    pub cspgo: bool,
    pub samplepgo: bool,
    pub debug: bool,
    pub strip: bool,
    pub networking: bool,
    pub compressman: bool,
    pub lastrip: bool,
}

impl Default for OptionsSpec {
    fn default() -> Self {
        Self {
            toolchain: ToolchainSpec::Llvm,
            cspgo: false,
            samplepgo: false,
            debug: true,
            strip: true,
            networking: false,
            compressman: false,
            lastrip: true,
        }
    }
}

/// A dynamically named value represented without dynamic record fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueSpec<T> {
    pub key: String,
    pub value: T,
}

impl<T, U> From<KeyValueSpec<T>> for crate::KeyValue<U>
where
    U: From<T>,
{
    fn from(spec: KeyValueSpec<T>) -> Self {
        Self {
            key: spec.key,
            value: spec.value.into(),
        }
    }
}

/// An authored source request with its kind encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSpec {
    Archive {
        url: String,
        hash: String,
        rename: Option<String>,
        strip_dirs: Option<i64>,
        unpack: bool,
        unpack_dir: Option<String>,
    },
    Git {
        url: String,
        git_ref: String,
        clone_dir: Option<String>,
    },
}

/// A package output path with its matching behavior encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

/// One explicit package tuning selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuningSpec {
    Enable,
    Disable,
    Config { value: String },
}

/// A supported compiler toolchain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolchainSpec {
    #[default]
    Llvm,
    Gnu,
}

impl From<ToolchainSpec> for crate::tuning::Toolchain {
    fn from(spec: ToolchainSpec) -> Self {
        match spec {
            ToolchainSpec::Llvm => Self::Llvm,
            ToolchainSpec::Gnu => Self::Gnu,
        }
    }
}

impl From<crate::tuning::Toolchain> for ToolchainSpec {
    fn from(toolchain: crate::tuning::Toolchain) -> Self {
        match toolchain {
            crate::tuning::Toolchain::Llvm => Self::Llvm,
            crate::tuning::Toolchain::Gnu => Self::Gnu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_defaults_are_explicit_and_stable() {
        let options = OptionsSpec::default();
        assert_eq!(options.toolchain, ToolchainSpec::Llvm);
        assert!(options.debug);
        assert!(options.strip);
        assert!(options.lastrip);
        assert!(!options.networking);
    }
}
