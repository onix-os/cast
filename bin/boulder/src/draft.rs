// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::Path, path::PathBuf};

use itertools::Itertools;
use licenses::match_licences;
use moss::util;
use stone::relation::{Dependency, Kind};
use stone_recipe::UpstreamSpec;
use thiserror::Error;
use tui::Styled;
use url::Url;

use crate::Env;

use self::metadata::Metadata;
use self::upstream::Upstream;

mod build;
mod licenses;
mod metadata;
pub mod upstream;

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
        let extract_root = temp_dir.as_ref();

        // Fetch and extract all upstreams
        let extracted = upstream::fetch_and_extract(&self.env, &self.upstreams, extract_root)?;

        // Build metadata from extracted upstreams
        let metadata = Metadata::new(extracted);

        // Enumerate all extracted files
        let files = util::enumerate_files(extract_root, |_| true)?
            .into_iter()
            .map(|path| File { path, extract_root })
            .collect::<Vec<_>>();

        // Analyze files to determine build system / collect deps
        let build = build::analyze(&files).map_err(Error::AnalyzeBuildSystem)?;

        let licences_dir = &self.env.data_dir.join("licenses");

        let licenses = format_licenses(match_licences(extract_root, licences_dir).unwrap_or_default());

        // Remove temp extract dir
        drop(temp_dir);

        let build_system = build.detected_system.unwrap_or_else(|| {
            println!(
                "{} | Unhandled build system! - Defaulting to autotools",
                "Warning".yellow()
            );
            build::System::Autotools
        });

        let stone = encode_package_v2(&metadata, build_system, build.dependencies, licenses);

        Ok(Draft { stone })
    }
}

fn encode_package_v2(
    metadata: &Metadata,
    build_system: build::System,
    dependencies: impl IntoIterator<Item = Dependency>,
    licenses: Vec<String>,
) -> String {
    use std::fmt::Write as _;

    let mut output = String::from("let b = import! boulder.package.v2\n");
    let builder_module = match build_system {
        build::System::Cmake => Some("boulder.builders.cmake.v1"),
        build::System::Meson => Some("boulder.builders.meson.v1"),
        build::System::Cargo => Some("boulder.builders.cargo.v1"),
        build::System::Autotools => Some("boulder.builders.autotools.v1"),
        _ => None,
    };
    if let Some(module) = builder_module {
        writeln!(output, "let builder = import! {module}").unwrap();
    }

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

    if builder_module.is_some() {
        let run_tests = matches!(build_system, build::System::Cargo);
        output.push_str("    builder = builder.builder {\n");
        writeln!(
            output,
            "        run_tests = b.boolean.{},",
            if run_tests { "true" } else { "false" }
        )
        .unwrap();
        output.push_str("        .. builder.defaults\n    },\n");
    } else {
        let phases = build_system.phases();
        output.push_str("    builder = b.builder.shell (b.scripts {\n");
        for (name, value) in [
            ("setup", phases.setup),
            ("build", phases.build),
            ("install", phases.install),
            ("check", phases.check),
            ("environment", build_system.environment()),
        ] {
            if let Some(value) = value {
                writeln!(output, "        {name} = b.optional.set {},", quoted(value)).unwrap();
            }
        }
        output.push_str("        .. b.defaults.scripts\n    }) [],\n");
    }

    if matches!(build_system, build::System::Cargo) {
        output.push_str("    hooks = {\n");
        output.push_str("        pre_setup = [\"%cargo_fetch\"],\n");
        output.push_str("        .. b.defaults.hooks\n    },\n");
    }
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
    if build_system.options().networking {
        output.push_str("    options = {\n");
        output.push_str("        networking = b.boolean.true,\n");
        output.push_str("        .. b.defaults.options\n    },\n");
    }
    output.push_str("    .. base\n}\n");
    output
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

pub struct File<'a> {
    pub path: PathBuf,
    pub extract_root: &'a Path,
}

impl File<'_> {
    // The depth of a file relative to it's extracted archive
    pub fn depth(&self) -> usize {
        let relative = self.path.strip_prefix(self.extract_root).unwrap_or(&self.path);

        // Subtract 2 so root of archive folder == depth 0
        relative.iter().count().saturating_sub(2)
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
    #[error("walkdir")]
    WalkDir(#[from] walkdir::Error),
}

#[cfg(test)]
mod test {
    use std::{collections::BTreeSet, path::Path};

    use gluon_config::Source as GluonSource;
    use stone_recipe::package::evaluate_gluon;

    use super::*;

    #[test]
    fn test_file_depth() {
        let extract_root = Path::new("/tmp/test");

        let file = File {
            path: PathBuf::from("/tmp/test/some_archive/meson.build"),
            extract_root,
        };

        assert_eq!(file.depth(), 0);
    }

    #[test]
    fn generated_draft_is_a_valid_standalone_gluon_recipe() {
        let metadata = Metadata::new(vec![Upstream {
            uri: Url::parse("https://example.com/example-1.2.3.tar.xz").unwrap(),
            hash: "0123456789abcdef".to_owned(),
        }]);
        let source = encode_package_v2(
            &metadata,
            build::System::Cargo,
            BTreeSet::<Dependency>::new(),
            vec!["MPL-2.0".to_owned()],
        );

        let evaluated = evaluate_gluon(&GluonSource::new("stone.glu", source.clone())).unwrap();

        assert!(source.contains("boulder.package.v2"));
        assert!(source.contains("boulder.builders.cargo.v1"));
        assert!(source.contains("UPDATE SUMMARY"));
        assert_eq!(evaluated.recipe.source.name, "example");
        assert_eq!(evaluated.recipe.source.version, "1.2.3");
        assert_eq!(evaluated.recipe.upstreams.len(), 1);
        assert!(evaluated.recipe.options.networking);
    }
}
