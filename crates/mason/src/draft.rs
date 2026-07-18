// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{io, path::PathBuf};

use gluon_config::Source as GluonSource;
use itertools::Itertools;
use licenses::match_licences;
use stone::relation::{Dependency, Kind};
use stone_recipe::{UpstreamSpec, package::evaluate_gluon};
use thiserror::Error;
use url::Url;

use crate::Env;

use self::metadata::Metadata;
use self::upstream::Upstream;

mod build;
mod licenses;
mod metadata;
pub mod upstream;

const MAX_DRAFT_FILES: usize = 10_000;

pub struct Drafter {
    env: Env,
    upstreams: Vec<Url>,
}

pub struct Draft {
    pub stone: String,
}

impl Drafter {
    pub fn new(env: Env, upstreams: Vec<Url>) -> Self {
        Self { env, upstreams }
    }

    pub fn run(&self) -> Result<Draft, Error> {
        let temp_dir = tempfile::tempdir()?;
        let download_root = temp_dir.path().join("downloads");
        let extract_root = temp_dir.path().join("extracted");
        std::fs::create_dir(&download_root)?;
        std::fs::create_dir(&extract_root)?;

        // Fetch and extract all upstreams
        let extracted = upstream::fetch_and_extract(&self.env, &self.upstreams, &download_root, &extract_root)?;
        require_draft_file_limit(extracted.files.len())?;

        // Build metadata from extracted upstreams
        let metadata = Metadata::new(extracted.upstreams);

        // Analyze files to determine build system / collect deps
        let build = build::analyze(&extracted.files).map_err(Error::AnalyzeBuildSystem)?;

        let licences_dir = &self.env.data_dir.join("licenses");

        let licenses = format_licenses(match_licences(&extracted.files, licences_dir)?);

        // Remove temp extract dir
        drop(temp_dir);

        let build_system = require_detected_build_system(build.detected_system)?;

        let stone = encode_package_v3(&metadata, build_system, build.dependencies, licenses)?;
        evaluate_gluon(&GluonSource::new("stone.glu", stone.clone())).map_err(Error::GeneratedDraft)?;

        Ok(Draft { stone })
    }
}

fn require_detected_build_system(system: Option<build::System>) -> Result<build::System, Error> {
    system.ok_or(Error::UndetectedBuildSystem)
}

fn require_draft_file_limit(actual: usize) -> Result<(), Error> {
    if actual <= MAX_DRAFT_FILES {
        Ok(())
    } else {
        Err(Error::TooManyDraftFiles {
            actual,
            limit: MAX_DRAFT_FILES,
        })
    }
}

fn encode_package_v3(
    metadata: &Metadata,
    build_system: build::System,
    dependencies: impl IntoIterator<Item = Dependency>,
    licenses: Vec<String>,
) -> Result<String, Error> {
    use std::fmt::Write as _;

    let mut output = String::from("let b = import! cast.package.v3\n");
    let builder_module = match build_system {
        build::System::Cmake => "cast.builders.cmake.v2",
        build::System::Meson => "cast.builders.meson.v2",
        build::System::Cargo => "cast.builders.cargo.v2",
        build::System::Autotools => "cast.builders.autotools.v2",
        unsupported => {
            return Err(Error::UnsupportedDraftSystem {
                system: unsupported.to_string(),
            });
        }
    };
    writeln!(output, "let builder = import! {builder_module}").unwrap();

    output.push_str("let base = b.mk_package (b.meta {\n");
    for (field, value) in [
        ("pname", placeholder(&metadata.source.name, "UPDATE-NAME")),
        ("version", placeholder(&metadata.source.version, "0.0.0")),
        (
            "homepage",
            placeholder(&metadata.source.homepage, "https://example.invalid/UPDATE-HOMEPAGE"),
        ),
    ] {
        writeln!(output, "    {field} = {},", quoted(&value)).unwrap();
        if field == "version" {
            output.push_str("    release = 1,\n");
        }
    }
    writeln!(output, "    license = {},", string_array(&licenses)).unwrap();
    output.push_str("})\n");
    output.push_str("let root = {\n");
    output.push_str("    summary = b.optional.set \"UPDATE SUMMARY\",\n");
    output.push_str("    description = b.optional.set \"UPDATE DESCRIPTION\",\n");
    output.push_str("    .. b.output \"out\"\n}\n");
    output.push_str("{\n");

    let run_tests = matches!(build_system, build::System::Cargo);
    output.push_str("    builder = builder.builder {\n");
    writeln!(
        output,
        "        run_tests = b.boolean.{},",
        if run_tests { "true" } else { "false" }
    )
    .unwrap();
    output.push_str("        .. builder.defaults\n    },\n");

    let dependencies = dependencies.into_iter().sorted().collect::<Vec<_>>();
    writeln!(
        output,
        "    build_inputs = [{}],",
        dependencies
            .iter()
            .map(encode_dependency)
            .collect::<Vec<_>>()
            .join(", ")
    )
    .unwrap();
    output.push_str("    sources = [\n");
    for source in metadata.upstream_specs() {
        match source {
            UpstreamSpec::Archive { url, hash, .. } => {
                writeln!(output, "        b.source.archive {} {},", quoted(&url), quoted(&hash)).unwrap();
            }
            UpstreamSpec::Git { url, git_ref, .. } => {
                writeln!(output, "        b.source.git {} {},", quoted(&url), quoted(&git_ref)).unwrap();
            }
        }
    }
    output.push_str("    ],\n    outputs = [root],\n");
    output.push_str("    .. base\n}\n");
    Ok(output)
}

fn placeholder(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn encode_dependency(dependency: &Dependency) -> String {
    let constructor = match dependency.kind {
        Kind::PackageName => "package",
        Kind::SharedLibrary => "soname",
        Kind::PkgConfig => "pkgconfig",
        Kind::Interpreter => "interpreter",
        Kind::CMake => "cmake",
        Kind::Python => "python",
        Kind::Binary => "binary",
        Kind::SystemBinary => "system_binary",
        Kind::PkgConfig32 => "pkgconfig32",
    };
    format!("b.dep.{constructor} {}", quoted(&dependency.name))
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string is infallible")
}

fn string_array(values: &[String]) -> String {
    format!("[{}]", values.iter().map(|value| quoted(value)).join(", "))
}

fn format_licenses(licenses: Vec<String>) -> Vec<String> {
    let mut formatted = licenses
        .into_iter()
        .sorted_by(|a, b| {
            // HACK: Ensure -or-later for GNU licenses comes before -only
            //       to match 90% of cases. We need to read the standard license
            //       header to figure out the actual variant.
            if a.contains("-only") {
                std::cmp::Ordering::Greater
            } else if b.contains("-only") {
                std::cmp::Ordering::Less
            } else {
                a.cmp(b)
            }
        })
        .collect::<Vec<_>>();
    if formatted.is_empty() {
        formatted.push("UPDATE LICENSE".to_owned());
    }
    formatted
}

pub struct File {
    pub path: PathBuf,
    depth: usize,
}

impl File {
    pub fn new(path: PathBuf, depth: usize) -> Self {
        Self { path, depth }
    }

    // The depth of a file relative to it's extracted archive
    pub fn depth(&self) -> usize {
        self.depth
    }

    pub fn file_name(&self) -> &str {
        self.path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("analyzing build system")]
    AnalyzeBuildSystem(#[source] build::Error),
    #[error("upstream")]
    Upstream(#[from] upstream::Error),
    #[error("licensing")]
    Licenses(#[from] licenses::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("generated draft failed its bounded Gluon evaluation")]
    GeneratedDraft(#[source] stone_recipe::package::PackageEvaluationError),
    #[error("draft manifest contains {actual} regular files; limit is {limit}")]
    TooManyDraftFiles { actual: usize, limit: usize },
    #[error("detected build system {system} has no typed draft builder")]
    UnsupportedDraftSystem { system: String },
    #[error("could not detect a supported build system from the admitted source manifest")]
    UndetectedBuildSystem,
}

#[cfg(test)]
mod test {
    use std::collections::BTreeSet;

    use gluon_config::Source as GluonSource;
    use stone_recipe::package::evaluate_gluon;

    use super::*;

    #[test]
    fn test_file_depth() {
        let file = File::new(PathBuf::from("/tmp/test/some_archive/meson.build"), 0);

        assert_eq!(file.depth(), 0);
    }

    #[test]
    fn draft_manifest_limit_accepts_n_and_rejects_n_plus_one_before_analysis() {
        assert!(require_draft_file_limit(MAX_DRAFT_FILES).is_ok());
        assert!(matches!(
            require_draft_file_limit(MAX_DRAFT_FILES + 1),
            Err(Error::TooManyDraftFiles {
                actual,
                limit: MAX_DRAFT_FILES,
            }) if actual == MAX_DRAFT_FILES + 1
        ));
    }

    #[test]
    fn generated_draft_is_a_valid_standalone_gluon_recipe() {
        let metadata = Metadata::new(vec![Upstream {
            uri: Url::parse("https://example.com/example-1.2.3.tar.xz").unwrap(),
            hash: "0123456789abcdef".repeat(4),
        }]);
        let source = encode_package_v3(
            &metadata,
            build::System::Cargo,
            BTreeSet::<Dependency>::new(),
            vec!["MPL-2.0".to_owned()],
        )
        .unwrap();

        let evaluated = evaluate_gluon(&GluonSource::new("stone.glu", source.clone())).unwrap();

        assert!(source.contains("cast.package.v3"));
        assert!(source.contains("cast.builders.cargo.v2"));
        assert!(source.contains("UPDATE SUMMARY"));
        assert!(!source.contains("cargo_fetch"));
        assert_eq!(evaluated.package.meta.pname, "example");
        assert_eq!(evaluated.package.meta.version, "1.2.3");
        assert_eq!(evaluated.package.sources.len(), 1);
        assert!(!source.contains("networking ="));
        assert!(!evaluated.package.options.networking);
    }

    #[test]
    fn untyped_python_ruby_and_perl_drafts_fail_closed() {
        let metadata = Metadata::new(vec![Upstream {
            uri: Url::parse("https://example.com/example-1.2.3.tar.xz").unwrap(),
            hash: "a".repeat(64),
        }]);
        for system in [
            build::System::PythonPep517,
            build::System::PythonSetupTools,
            build::System::RubyGem,
            build::System::RubyTarball,
            build::System::PerlExtutilsMakefile,
            build::System::PerlModuleBuild,
        ] {
            assert!(matches!(
                encode_package_v3(&metadata, system, BTreeSet::<Dependency>::new(), vec![]),
                Err(Error::UnsupportedDraftSystem { .. })
            ));
        }
    }

    #[test]
    fn missing_build_metadata_never_defaults_to_invented_autotools_semantics() {
        assert!(matches!(
            require_detected_build_system(None),
            Err(Error::UndetectedBuildSystem)
        ));
        assert_eq!(
            require_detected_build_system(Some(build::System::Cmake)).unwrap(),
            build::System::Cmake
        );
    }
}
