// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Format-neutral recipe input types.
//!
//! These types deliberately contain only primitive values, options, vectors,
//! key-value arrays, and explicit variants. Parsers and embedded languages can
//! target this boundary without taking a dependency on YAML shapes or domain
//! parser types such as [`url::Url`] and [`std::path::PathBuf`].

use std::path::PathBuf;

use thiserror::Error;
use url::Url;

use crate::{
    Build, KeyValue, Options, Package, Path, PathKind, Recipe, Source, Tuning, ValidationError,
    tuning::Toolchain,
    upstream::{Props, Upstream},
};

/// A format-neutral package recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeSpec {
    pub source: SourceSpec,
    pub build: BuildSpec,
    pub package: PackageSpec,
    pub options: OptionsSpec,
    pub profiles: Vec<KeyValueSpec<BuildSpec>>,
    pub sub_packages: Vec<KeyValueSpec<PackageSpec>>,
    pub upstreams: Vec<UpstreamSpec>,
    pub architectures: Vec<String>,
    pub tuning: Vec<KeyValueSpec<TuningSpec>>,
    pub emul32: bool,
    pub mold: bool,
}

impl RecipeSpec {
    /// Construct a minimal recipe with the same defaults as the legacy loader.
    pub fn new(source: SourceSpec) -> Self {
        Self {
            source,
            build: BuildSpec::default(),
            package: PackageSpec::default(),
            options: OptionsSpec::default(),
            profiles: Vec::new(),
            sub_packages: Vec::new(),
            upstreams: Vec::new(),
            architectures: Vec::new(),
            tuning: Vec::new(),
            emul32: false,
            mold: false,
        }
    }
}

/// Source metadata required for every recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub name: String,
    pub version: String,
    pub release: u64,
    pub homepage: String,
    pub license: Vec<String>,
}

/// Build phases and their dependencies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildSpec {
    pub setup: Option<String>,
    pub build: Option<String>,
    pub install: Option<String>,
    pub check: Option<String>,
    pub workload: Option<String>,
    pub environment: Option<String>,
    pub build_deps: Vec<String>,
    pub check_deps: Vec<String>,
}

/// Recipe-wide build options.
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

/// Package metadata shared by the root package and subpackages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageSpec {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub run_deps: Vec<String>,
    pub run_deps_exclude: Vec<String>,
    pub paths: Vec<PathSpec>,
    pub conflicts: Vec<String>,
}

/// A dynamically named value represented without dynamic record fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueSpec<T> {
    pub key: String,
    pub value: T,
}

/// An upstream with its source kind encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSpec {
    Archive {
        url: String,
        hash: String,
        rename: Option<String>,
        strip_dirs: Option<u8>,
        unpack: bool,
        unpack_dir: Option<String>,
    },
    Git {
        url: String,
        git_ref: String,
        clone_dir: Option<String>,
    },
}

/// A package path with its matching behavior encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

/// A tuning setting with no scalar-or-map coercion.
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

/// Failure to convert a format-neutral recipe into the domain model.
#[derive(Debug, Error)]
pub enum RecipeConversionError {
    /// A URL field could not be parsed.
    #[error("{field}: invalid URL `{value}`")]
    InvalidUrl {
        field: String,
        value: String,
        #[source]
        source: url::ParseError,
    },
    /// The converted recipe violated a format-independent invariant.
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

impl RecipeConversionError {
    /// Return the stable field path associated with this error.
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidUrl { field, .. } => field,
            Self::Validation(error) => error.field(),
        }
    }
}

impl TryFrom<RecipeSpec> for Recipe {
    type Error = RecipeConversionError;

    fn try_from(spec: RecipeSpec) -> Result<Self, Self::Error> {
        let RecipeSpec {
            source,
            build,
            package,
            options,
            profiles,
            sub_packages,
            upstreams,
            architectures,
            tuning,
            emul32,
            mold,
        } = spec;

        let upstreams = upstreams
            .into_iter()
            .enumerate()
            .map(|(index, upstream)| upstream.try_into_domain(index))
            .collect::<Result<_, _>>()?;

        let recipe = Self {
            source: source.into(),
            build: build.into(),
            package: package.into(),
            options: options.into(),
            profiles: profiles.into_iter().map(Into::into).collect(),
            sub_packages: sub_packages.into_iter().map(Into::into).collect(),
            upstreams,
            architectures,
            tuning: tuning.into_iter().map(Into::into).collect(),
            emul32,
            mold,
        };

        recipe.validate()?;
        Ok(recipe)
    }
}

impl From<SourceSpec> for Source {
    fn from(spec: SourceSpec) -> Self {
        Self {
            name: spec.name,
            version: spec.version,
            release: spec.release,
            homepage: spec.homepage,
            license: spec.license,
        }
    }
}

impl From<BuildSpec> for Build {
    fn from(spec: BuildSpec) -> Self {
        Self {
            setup: spec.setup,
            build: spec.build,
            install: spec.install,
            check: spec.check,
            workload: spec.workload,
            environment: spec.environment,
            build_deps: spec.build_deps,
            check_deps: spec.check_deps,
        }
    }
}

impl From<OptionsSpec> for Options {
    fn from(spec: OptionsSpec) -> Self {
        Self {
            toolchain: spec.toolchain.into(),
            cspgo: spec.cspgo,
            samplepgo: spec.samplepgo,
            debug: spec.debug,
            strip: spec.strip,
            networking: spec.networking,
            compressman: spec.compressman,
            lastrip: spec.lastrip,
        }
    }
}

impl From<PackageSpec> for Package {
    fn from(spec: PackageSpec) -> Self {
        Self {
            summary: spec.summary,
            description: spec.description,
            provides_exclude: spec.provides_exclude,
            run_deps: spec.run_deps,
            run_deps_exclude: spec.run_deps_exclude,
            paths: spec.paths.into_iter().map(Into::into).collect(),
            conflicts: spec.conflicts,
        }
    }
}

impl<T, U> From<KeyValueSpec<T>> for KeyValue<U>
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

impl UpstreamSpec {
    fn try_into_domain(self, index: usize) -> Result<Upstream, RecipeConversionError> {
        let parse_url = |value: String| {
            Url::parse(&value).map_err(|source| RecipeConversionError::InvalidUrl {
                field: format!("upstreams[{index}].url"),
                value,
                source,
            })
        };

        match self {
            Self::Archive {
                url,
                hash,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
            } => Ok(Upstream {
                url: parse_url(url)?,
                props: Props::Plain {
                    hash,
                    rename,
                    strip_dirs,
                    unpack,
                    unpack_dir: unpack_dir.map(PathBuf::from),
                },
            }),
            Self::Git {
                url,
                git_ref,
                clone_dir,
            } => Ok(Upstream {
                url: parse_url(url)?,
                props: Props::Git {
                    git_ref,
                    clone_dir: clone_dir.map(PathBuf::from),
                },
            }),
        }
    }
}

impl From<PathSpec> for Path {
    fn from(spec: PathSpec) -> Self {
        let (path, kind) = match spec {
            PathSpec::Any { path } => (path, PathKind::Any),
            PathSpec::Exe { path } => (path, PathKind::Exe),
            PathSpec::Symlink { path } => (path, PathKind::Symlink),
            PathSpec::Special { path } => (path, PathKind::Special),
        };

        Self { path, kind }
    }
}

impl From<TuningSpec> for Tuning {
    fn from(spec: TuningSpec) -> Self {
        match spec {
            TuningSpec::Enable => Self::Enable,
            TuningSpec::Disable => Self::Disable,
            TuningSpec::Config { value } => Self::Config(value),
        }
    }
}

impl From<ToolchainSpec> for Toolchain {
    fn from(spec: ToolchainSpec) -> Self {
        match spec {
            ToolchainSpec::Llvm => Self::Llvm,
            ToolchainSpec::Gnu => Self::Gnu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceSpec {
        SourceSpec {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            release: 1,
            homepage: "https://example.com".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        }
    }

    #[test]
    fn minimal_recipe_uses_legacy_defaults() {
        let recipe = Recipe::try_from(RecipeSpec::new(source())).unwrap();

        assert!(recipe.build.setup.is_none());
        assert!(recipe.build.build_deps.is_empty());
        assert!(recipe.package.paths.is_empty());
        assert!(matches!(recipe.options.toolchain, Toolchain::Llvm));
        assert!(recipe.options.debug);
        assert!(recipe.options.strip);
        assert!(recipe.options.lastrip);
        assert!(!recipe.options.cspgo);
        assert!(!recipe.options.samplepgo);
        assert!(!recipe.options.networking);
        assert!(!recipe.options.compressman);
        assert!(recipe.profiles.is_empty());
        assert!(recipe.sub_packages.is_empty());
        assert!(recipe.upstreams.is_empty());
        assert!(recipe.architectures.is_empty());
        assert!(recipe.tuning.is_empty());
        assert!(!recipe.emul32);
        assert!(!recipe.mold);
    }

    #[test]
    fn all_fields_and_explicit_variants_convert() {
        let mut spec = RecipeSpec::new(source());
        spec.build = BuildSpec {
            setup: Some("setup".to_owned()),
            build: Some("build".to_owned()),
            install: Some("install".to_owned()),
            check: Some("check".to_owned()),
            workload: Some("workload".to_owned()),
            environment: Some("environment".to_owned()),
            build_deps: vec!["build-dependency".to_owned()],
            check_deps: vec!["check-dependency".to_owned()],
        };
        spec.package = PackageSpec {
            summary: Some("summary".to_owned()),
            description: Some("description".to_owned()),
            provides_exclude: vec!["provided(*)".to_owned()],
            run_deps: vec!["runtime-dependency".to_owned()],
            run_deps_exclude: vec!["excluded-runtime(*)".to_owned()],
            paths: vec![
                PathSpec::Any {
                    path: "/usr/share/example".to_owned(),
                },
                PathSpec::Exe {
                    path: "/usr/bin/example".to_owned(),
                },
                PathSpec::Symlink {
                    path: "/usr/bin/example-link".to_owned(),
                },
                PathSpec::Special {
                    path: "/usr/lib/example.special".to_owned(),
                },
            ],
            conflicts: vec!["other-package".to_owned()],
        };
        spec.options = OptionsSpec {
            toolchain: ToolchainSpec::Gnu,
            cspgo: true,
            samplepgo: true,
            debug: false,
            strip: false,
            networking: true,
            compressman: true,
            lastrip: false,
        };
        spec.profiles = vec![KeyValueSpec {
            key: "x86_64".to_owned(),
            value: BuildSpec {
                build: Some("profile build".to_owned()),
                ..BuildSpec::default()
            },
        }];
        spec.sub_packages = vec![KeyValueSpec {
            key: "example-devel".to_owned(),
            value: PackageSpec {
                summary: Some("development files".to_owned()),
                ..PackageSpec::default()
            },
        }];
        spec.upstreams = vec![
            UpstreamSpec::Archive {
                url: "https://example.com/source.tar.xz".to_owned(),
                hash: "archive-hash".to_owned(),
                rename: Some("source.tar.xz".to_owned()),
                strip_dirs: Some(1),
                unpack: false,
                unpack_dir: Some("archive".to_owned()),
            },
            UpstreamSpec::Git {
                url: "https://example.com/source.git".to_owned(),
                git_ref: "v1.2.3".to_owned(),
                clone_dir: Some("git".to_owned()),
            },
        ];
        spec.architectures = vec!["x86_64".to_owned()];
        spec.tuning = vec![
            KeyValueSpec {
                key: "harden".to_owned(),
                value: TuningSpec::Enable,
            },
            KeyValueSpec {
                key: "lto".to_owned(),
                value: TuningSpec::Disable,
            },
            KeyValueSpec {
                key: "optimize".to_owned(),
                value: TuningSpec::Config {
                    value: "speed".to_owned(),
                },
            },
        ];
        spec.emul32 = true;
        spec.mold = true;

        let recipe = Recipe::try_from(spec).unwrap();

        assert_eq!(recipe.build.setup.as_deref(), Some("setup"));
        assert_eq!(recipe.build.check_deps, ["check-dependency"]);
        assert_eq!(recipe.package.provides_exclude, ["provided(*)"]);
        assert_eq!(recipe.package.paths[0].kind, PathKind::Any);
        assert_eq!(recipe.package.paths[1].kind, PathKind::Exe);
        assert_eq!(recipe.package.paths[2].kind, PathKind::Symlink);
        assert_eq!(recipe.package.paths[3].kind, PathKind::Special);
        assert!(matches!(recipe.options.toolchain, Toolchain::Gnu));
        assert_eq!(recipe.profiles[0].key, "x86_64");
        assert_eq!(recipe.sub_packages[0].key, "example-devel");
        assert!(matches!(recipe.upstreams[0].props, Props::Plain { .. }));
        assert!(matches!(recipe.upstreams[1].props, Props::Git { .. }));
        assert_eq!(recipe.architectures, ["x86_64"]);
        assert!(matches!(recipe.tuning[0].value, Tuning::Enable));
        assert!(matches!(recipe.tuning[1].value, Tuning::Disable));
        assert!(matches!(recipe.tuning[2].value, Tuning::Config(ref value) if value == "speed"));
        assert!(recipe.emul32);
        assert!(recipe.mold);
    }

    #[test]
    fn invalid_upstream_url_reports_indexed_field_path() {
        let mut spec = RecipeSpec::new(source());
        spec.upstreams.push(UpstreamSpec::Git {
            url: "not a URL".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: None,
        });

        let error = Recipe::try_from(spec).unwrap_err();

        assert_eq!(error.field(), "upstreams[0].url");
        assert!(error.to_string().starts_with("upstreams[0].url: invalid URL"));
    }

    #[test]
    fn version_invariant_is_reusable_and_has_a_field_path() {
        let mut spec = RecipeSpec::new(source());
        spec.source.version = "v1.2.3".to_owned();

        let error = Recipe::try_from(spec).unwrap_err();

        assert_eq!(error.field(), "source.version");
        assert!(matches!(
            error,
            RecipeConversionError::Validation(ValidationError::VersionMustStartWithDigit { .. })
        ));
    }

    #[test]
    fn release_invariant_is_reusable_and_has_a_field_path() {
        let mut spec = RecipeSpec::new(source());
        spec.source.release = 0;

        let error = Recipe::try_from(spec).unwrap_err();

        assert_eq!(error.field(), "source.release");
        assert!(matches!(
            error,
            RecipeConversionError::Validation(ValidationError::ReleaseMustBePositive { release: 0 })
        ));
    }
}
