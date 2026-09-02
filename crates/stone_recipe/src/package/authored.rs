//! Language-agnostic authored package model and its lowering.
//!
//! [`AuthoredPackage`] is the minimal authoring surface every configuration
//! language targets: identity, sources, a structural [`BuilderRequest`], typed
//! dependency lists, and *optional* outputs/options/profiles/tuning that default
//! in Rust. [`lower`] fills those defaults, lowers the builder request, and
//! produces the frozen [`PackageSpec`] domain the executor consumes.
//!
//! This is the shared "common" layer. Because the defaults and lowering live
//! here — not in a config-language module — configuration-language adapters
//! are interchangeable authoring syntaxes and any one of them can be removed
//! without losing the ability to author a complete package.

use crate::{NamedTuningSpec, OptionsSpec, PathSpec, UpstreamSpec};

use super::{
    BuilderRequest, DependencySpec, HooksSpec, MetaSpec, OutputRef, OutputSpec, PackageConversionError,
    PackageRef, PackageSpec, ProfileSpec, lower_builder,
};

/// The minimal, language-agnostic authored package. Optional fields are filled
/// by [`lower`] from the versioned package-ABI defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredPackage {
    pub meta: MetaSpec,
    pub builder: BuilderRequest,
    pub sources: Vec<UpstreamSpec>,
    pub native_build_inputs: Vec<DependencySpec>,
    pub build_inputs: Vec<DependencySpec>,
    pub check_inputs: Vec<DependencySpec>,
    /// `None` selects the deterministic default split-output set.
    pub outputs: Option<Vec<OutputSpec>>,
    /// `None` selects [`OptionsSpec::default`].
    pub options: Option<OptionsSpec>,
    pub profiles: Vec<ProfileSpec>,
    pub architectures: Vec<String>,
    pub tuning: Vec<NamedTuningSpec>,
    pub emul32: bool,
    pub mold: bool,
    pub hooks: HooksSpec,
}

/// Lower an authored package into the frozen [`PackageSpec`] domain.
///
/// Lowering validates: the authored domain can express values the frozen domain
/// forbids, so the transition is fallible. Validating here rather than in each
/// language adapter is what keeps the backends interchangeable — a backend
/// cannot decline to check what it just decoded.
pub fn lower(authored: AuthoredPackage) -> Result<PackageSpec, PackageConversionError> {
    let outputs = authored
        .outputs
        .unwrap_or_else(|| default_output_set(&authored.meta.pname));
    let spec = PackageSpec {
        builder: lower_builder(authored.builder),
        meta: authored.meta,
        hooks: authored.hooks,
        native_build_inputs: authored.native_build_inputs,
        build_inputs: authored.build_inputs,
        check_inputs: authored.check_inputs,
        outputs,
        options: authored.options.unwrap_or_default(),
        profiles: authored.profiles,
        sources: authored.sources,
        architectures: authored.architectures,
        tuning: authored.tuning,
        emul32: authored.emul32,
        mold: authored.mold,
    };
    spec.validate()?;
    Ok(spec)
}

fn any(path: &str) -> PathSpec {
    PathSpec::Any {
        path: path.to_owned(),
    }
}

fn local_output(pname: &str, output: &str) -> DependencySpec {
    DependencySpec::Output(OutputRef {
        package: PackageRef {
            name: pname.to_owned(),
        },
        output: output.to_owned(),
    })
}

fn output(name: &str) -> OutputSpec {
    OutputSpec {
        name: name.to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        runtime_exclude: Vec::new(),
        paths: Vec::new(),
        conflicts: Vec::new(),
    }
}

/// Overlay an authored root output onto the default root, mirroring the former
/// `overlay_output` in `package.glu`: an authored optional wins when set, and the
/// authored list fields prepend the default ones.
fn overlay_root(authored: OutputSpec, default: OutputSpec) -> OutputSpec {
    let mut runtime_inputs = authored.runtime_inputs;
    runtime_inputs.extend(default.runtime_inputs);
    let mut runtime_exclude = authored.runtime_exclude;
    runtime_exclude.extend(default.runtime_exclude);
    let mut paths = authored.paths;
    paths.extend(default.paths);
    OutputSpec {
        name: authored.name,
        include_in_manifest: authored.include_in_manifest,
        summary: authored.summary.or(default.summary),
        description: authored.description.or(default.description),
        provides_exclude: authored.provides_exclude,
        runtime_inputs,
        runtime_exclude,
        paths,
        conflicts: authored.conflicts,
    }
}

/// The default split-output set with an authored root output overlaid onto the
/// default `out` output (mirrors the former `outputs.with_root` in
/// `package.glu`). The remaining eight defaults are unchanged.
pub fn default_output_set_with_root(pname: &str, root: OutputSpec) -> Vec<OutputSpec> {
    let mut outputs = default_output_set(pname);
    let default_root = outputs.remove(0);
    outputs.insert(0, overlay_root(root, default_root));
    outputs
}

/// The deterministic `cast.package.v3` default split-output set. Changing this
/// incompatibly requires a new package ABI version (mirrors the former
/// `default_outputs` in `package.glu`).
fn default_output_set(pname: &str) -> Vec<OutputSpec> {
    let root = OutputSpec {
        paths: vec![any("*")],
        ..output("out")
    };
    let docs = OutputSpec {
        summary: Some(format!("Documentation for {pname}")),
        description: Some(format!("Documentation files for the {pname} package")),
        paths: vec![any("/usr/share/gtk-doc")],
        ..output("docs")
    };
    let devel = OutputSpec {
        summary: Some(format!("Development files for {pname}")),
        description: Some(format!(
            "Install this package if you intend to build software against\nthe {pname} package."
        )),
        runtime_inputs: vec![local_output(pname, "out")],
        paths: [
            "/usr/include",
            "/usr/lib/*.a",
            "/usr/lib/cmake",
            "/usr/lib/lib*.so",
            "/usr/lib/pkgconfig",
            "/usr/share/aclocal",
            "/usr/share/cmake",
            "/usr/share/man/man2",
            "/usr/share/man/man3",
            "/usr/share/man/man9",
            "/usr/share/pkgconfig",
            "/usr/share/gir-1.0/*.gir",
            "/usr/share/vala/vapi/*.deps",
            "/usr/share/vala/vapi/*.vapi",
            "/usr/lib/*.prl",
            "/usr/lib/metatypes",
            "/usr/lib/qt*/metatypes/qt*.json",
            "/usr/lib/qt*/mkspecs",
            "/usr/lib/qt*/modules/*.json",
            "/usr/lib/qt*/sbom",
            "/usr/lib/qt*/plugins/designer/*.so",
            "/usr/share/doc/qt5/*.qch",
            "/usr/share/doc/qt5/*.tags",
            "/usr/share/doc/qt6/*.qch",
            "/usr/share/doc/qt6/*.tags",
        ]
        .into_iter()
        .map(any)
        .collect(),
        ..output("devel")
    };
    let dbginfo = OutputSpec {
        include_in_manifest: false,
        summary: Some(format!("Debugging symbols for {pname}")),
        description: Some(format!(
            "Install this package if you need debugging information + symbols\nfor the {pname} package."
        )),
        paths: vec![any("/usr/lib/debug")],
        ..output("dbginfo")
    };
    let libs = OutputSpec {
        summary: Some(format!("Library files for {pname}")),
        description: Some(format!(
            "Library files for {pname}, typically pulled in as a dependency of another package."
        )),
        ..output("libs")
    };
    let bit32 = OutputSpec {
        summary: Some(format!("Provides 32-bit runtime libraries for {pname}")),
        description: Some(format!(
            "Install this package if you need the 32-bit versions of the\n{pname} package libraries."
        )),
        runtime_inputs: vec![local_output(pname, "out")],
        paths: vec![any("/usr/lib32"), any("/usr/lib32/lib*.so.*")],
        ..output("32bit")
    };
    let bit32_devel = OutputSpec {
        summary: Some(format!("Provides development files for {pname}-32bit")),
        description: Some(format!(
            "Install this package if you need to build software against\nthe 32-bit version of {pname}, {pname}-32bit."
        )),
        runtime_inputs: vec![local_output(pname, "32bit"), local_output(pname, "devel")],
        paths: vec![
            any("/usr/lib32/*.a"),
            any("/usr/lib32/cmake"),
            any("/usr/lib32/lib*.so"),
            any("/usr/lib32/pkgconfig"),
        ],
        ..output("32bit-devel")
    };
    let bit32_dbginfo = OutputSpec {
        include_in_manifest: false,
        summary: Some(format!("Debugging symbols for {pname}-32bit")),
        description: Some(format!(
            "Install this package if you need debugging information + symbols\nfor the {pname}-32bit package."
        )),
        paths: vec![any("/usr/lib32/debug")],
        ..output("32bit-dbginfo")
    };
    let demos = OutputSpec {
        summary: Some(format!("Example files for {pname}")),
        description: Some(format!("Example files for the {pname} package")),
        paths: vec![any("/usr/lib/qt*/examples")],
        ..output("demos")
    };
    vec![
        root,
        docs,
        devel,
        dbginfo,
        libs,
        bit32,
        bit32_devel,
        bit32_dbginfo,
        demos,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ToolchainSpec;

    fn meta(pname: &str) -> MetaSpec {
        MetaSpec {
            pname: pname.to_owned(),
            version: "1.0.0".to_owned(),
            release: 1,
            homepage: "https://example.invalid".to_owned(),
            license: vec!["MIT".to_owned()],
        }
    }

    fn minimal(pname: &str) -> AuthoredPackage {
        AuthoredPackage {
            meta: meta(pname),
            builder: BuilderRequest::Cmake {
                flags: Vec::new(),
                run_tests: true,
            },
            sources: Vec::new(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
            outputs: None,
            options: None,
            profiles: Vec::new(),
            architectures: Vec::new(),
            tuning: Vec::new(),
            emul32: false,
            mold: false,
            hooks: HooksSpec::default(),
        }
    }

    #[test]
    fn default_output_set_matches_the_package_v3_abi_defaults() {
        let outputs = default_output_set("hello");
        let names: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "out",
                "docs",
                "devel",
                "dbginfo",
                "libs",
                "32bit",
                "32bit-devel",
                "32bit-dbginfo",
                "demos"
            ]
        );
        // The root output claims everything; dbginfo outputs are manifest-excluded.
        assert_eq!(outputs[0].paths, vec![any("*")]);
        assert!(!outputs[3].include_in_manifest);
        assert!(!outputs[7].include_in_manifest);
        // pname interpolation reaches summaries and runtime relations.
        assert_eq!(outputs[1].summary.as_deref(), Some("Documentation for hello"));
        assert_eq!(outputs[2].runtime_inputs, vec![local_output("hello", "out")]);
    }

    #[test]
    fn lower_fills_defaults_and_lowers_the_builder() {
        let spec = lower(minimal("hello")).expect("the minimal authored package is valid");
        assert_eq!(spec.meta.pname, "hello");
        // builder was a request; lowering produced typed cmake steps.
        assert_eq!(
            spec.builder.phases.setup.steps,
            vec![super::super::StepSpec::CMakeConfigure { flags: Vec::new() }]
        );
        // optional fields defaulted.
        assert_eq!(spec.options.toolchain, ToolchainSpec::Llvm);
        assert_eq!(spec.outputs.len(), 9);
    }

    #[test]
    fn authored_outputs_override_the_default_set() {
        let mut authored = minimal("hello");
        authored.outputs = Some(vec![output("out")]);
        let spec = lower(authored).expect("the overridden output set is valid");
        assert_eq!(spec.outputs, vec![output("out")]);
    }
}
