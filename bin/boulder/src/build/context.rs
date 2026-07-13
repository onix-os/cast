// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed lowering context for standard package-v2 build steps.
//!
//! This boundary deliberately accepts concrete values. It does not know about
//! legacy actions, definition names, or the script parser.

use std::collections::BTreeMap;

use stone_recipe::{
    ToolchainSpec,
    build_policy::{
        BuildPolicySpec, BuilderCommandSpec, CompilerFlagsSpec, CompilerToolsSpec, ContextValue,
        EnvironmentBindingSpec, EnvironmentCondition, TargetPolicySpec, TextSpec,
    },
    derivation::{StepPlan, StepPlan::Run},
    package::StepSpec,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    pub prefix: String,
    pub bindir: String,
    pub sbindir: String,
    pub includedir: String,
    pub libdir: String,
    pub libexecdir: String,
    pub datadir: String,
    pub mandir: String,
    pub infodir: String,
    pub localedir: String,
    pub sysconfdir: String,
    pub localstatedir: String,
    pub sharedstatedir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildContext {
    pub package_name: String,
    pub work_dir: String,
    pub build_subdir: String,
    pub install_root: String,
    pub target_triple: String,
    pub build_platform: String,
    pub host_platform: String,
    pub jobs: u32,
    pub layout: InstallLayout,
    pub environment: BTreeMap<String, String>,
}

impl BuildContext {
    /// Lower one standard builder step to an argv-preserving frozen step.
    ///
    /// `Shell` is deliberately not handled here. `CargoEnvironment` contributes
    /// to [`Self::environment`] and therefore has no executable step of its own.
    pub fn resolve_standard_step(&self, step: &StepSpec) -> Option<StepPlan> {
        let run = |program: &str, args: Vec<String>, environment: BTreeMap<String, String>| Run {
            program: program.to_owned(),
            args,
            environment: self
                .environment
                .iter()
                .chain(&environment)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            working_dir: self.work_dir.clone(),
        };
        let values = |items: &[&str]| -> Vec<String> { items.iter().map(|item| (*item).to_owned()).collect() };
        let jobs = self.jobs.to_string();

        Some(match step {
            StepSpec::Shell { .. } | StepSpec::CargoEnvironment => return None,
            StepSpec::CMakeConfigure { flags } => {
                let mut args = values(&[
                    "-G",
                    "Ninja",
                    "-B",
                    &self.build_subdir,
                    "-DCMAKE_VERBOSE_MAKEFILE=ON",
                    "-DCMAKE_C_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_CXX_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_Fortran_FLAGS_RELEASE=-DNDEBUG",
                    "-DCMAKE_BUILD_TYPE=Release",
                    "-DCMAKE_INSTALL_DO_STRIP=OFF",
                    "-DCMAKE_INSTALL_LIBDIR=lib",
                ]);
                args.extend([
                    format!("-DCMAKE_INSTALL_LIBEXECDIR={}", self.layout.libexecdir),
                    format!("-DCMAKE_INSTALL_PREFIX={}", self.layout.prefix),
                    format!(
                        "-DCMAKE_LIB_SUFFIX={}",
                        self.layout.libdir.strip_prefix("/usr/lib").unwrap_or_default()
                    ),
                ]);
                args.extend(flags.iter().cloned());
                run("cmake", args, BTreeMap::new())
            }
            StepSpec::CMakeBuild => run(
                "cmake",
                values(&["--build", &self.build_subdir, "--verbose", "--parallel", &jobs]),
                BTreeMap::new(),
            ),
            StepSpec::CMakeInstall => run(
                "cmake",
                values(&["--install", &self.build_subdir, "--verbose"]),
                BTreeMap::from([("DESTDIR".to_owned(), self.install_root.clone())]),
            ),
            StepSpec::CMakeTest => run(
                "ctest",
                values(&[
                    "--test-dir",
                    &self.build_subdir,
                    "--verbose",
                    "--parallel",
                    &jobs,
                    "--output-on-failure",
                    "--force-new-ctest-process",
                ]),
                BTreeMap::new(),
            ),
            StepSpec::MesonSetup { flags } => {
                let mut args = vec![
                    "setup".to_owned(),
                    "--buildtype=plain".to_owned(),
                    format!("--prefix={}", self.layout.prefix),
                    format!(
                        "--libdir={}",
                        self.layout.libdir.strip_prefix("/usr/").unwrap_or(&self.layout.libdir)
                    ),
                    format!("--bindir={}", self.layout.bindir),
                    format!("--sbindir={}", self.layout.sbindir),
                    format!(
                        "--libexecdir={}",
                        self.layout
                            .libexecdir
                            .strip_prefix("/usr/")
                            .unwrap_or(&self.layout.libexecdir)
                    ),
                    format!("--includedir={}", self.layout.includedir),
                    format!("--datadir={}", self.layout.datadir),
                    format!("--mandir={}", self.layout.mandir),
                    format!("--infodir={}", self.layout.infodir),
                    format!("--localedir={}", self.layout.localedir),
                    format!("--sysconfdir={}", self.layout.sysconfdir),
                    format!("--localstatedir={}", self.layout.localstatedir),
                    "--wrap-mode=nodownload".to_owned(),
                ];
                args.extend(flags.iter().cloned());
                args.push(self.build_subdir.clone());
                run("meson", args, BTreeMap::new())
            }
            StepSpec::MesonBuild => run(
                "meson",
                values(&["compile", "--verbose", "-j", &jobs, "-C", &self.build_subdir]),
                BTreeMap::new(),
            ),
            StepSpec::MesonInstall => run(
                "meson",
                values(&["install", "--no-rebuild", "-C", &self.build_subdir]),
                BTreeMap::from([("DESTDIR".to_owned(), self.install_root.clone())]),
            ),
            StepSpec::MesonTest => run(
                "meson",
                values(&[
                    "test",
                    "--no-rebuild",
                    "--print-errorlogs",
                    "--verbose",
                    "-j",
                    &jobs,
                    "-C",
                    &self.build_subdir,
                ]),
                BTreeMap::new(),
            ),
            StepSpec::CargoFetch => run("cargo", values(&["fetch", "-v", "--locked"]), BTreeMap::new()),
            StepSpec::CargoBuild { features } => {
                let mut args = values(&[
                    "build",
                    "-v",
                    "-j",
                    &jobs,
                    "--frozen",
                    "--target",
                    &self.target_triple,
                    "--release",
                ]);
                if !features.is_empty() {
                    args.extend(["--features".to_owned(), features.join(",")]);
                }
                run("cargo", args, BTreeMap::new())
            }
            StepSpec::CargoInstall { binaries } => {
                let binaries = if binaries.is_empty() {
                    vec![self.package_name.as_str()]
                } else {
                    binaries.iter().map(String::as_str).collect()
                };
                let mut args = values(&[
                    "-Dm00755",
                    "-t",
                    &format!("{}{}", self.install_root, self.layout.bindir),
                ]);
                args.extend(
                    binaries
                        .into_iter()
                        .map(|binary| format!("target/{}/release/{binary}", self.target_triple)),
                );
                run("install", args, BTreeMap::new())
            }
            StepSpec::CargoTest { features } => {
                let mut args = values(&[
                    "test",
                    "-v",
                    "-j",
                    &jobs,
                    "--frozen",
                    "--target",
                    &self.target_triple,
                    "--release",
                ]);
                if !features.is_empty() {
                    args.extend(["--features".to_owned(), features.join(",")]);
                }
                args.push("--workspace".to_owned());
                run("cargo", args, BTreeMap::new())
            }
            StepSpec::AutotoolsConfigure { flags } => {
                let mut args = vec![
                    "./configure".to_owned(),
                    format!("--prefix={}", self.layout.prefix),
                    format!("--bindir={}", self.layout.bindir),
                    format!("--sbindir={}", self.layout.sbindir),
                    format!("--build={}", self.build_platform),
                    format!("--host={}", self.host_platform),
                    format!("--libdir={}", self.layout.libdir),
                    format!("--mandir={}", self.layout.mandir),
                    format!("--infodir={}", self.layout.infodir),
                    format!("--datadir={}", self.layout.datadir),
                    format!("--sysconfdir={}", self.layout.sysconfdir),
                    format!("--localstatedir={}", self.layout.localstatedir),
                    format!("--sharedstatedir={}", self.layout.sharedstatedir),
                    format!("--libexecdir={}", self.layout.libexecdir),
                ];
                args.extend(flags.iter().cloned());
                run(
                    "/usr/bin/dash",
                    args,
                    BTreeMap::from([
                        ("CONFIG_SHELL".to_owned(), "/usr/bin/dash".to_owned()),
                        ("SHELL".to_owned(), "/usr/bin/dash".to_owned()),
                    ]),
                )
            }
            StepSpec::AutotoolsBuild => run("make", values(&["VERBOSE=1", "-j", &jobs]), BTreeMap::new()),
            StepSpec::AutotoolsInstall => run(
                "make",
                values(&["install", &format!("DESTDIR={}", self.install_root)]),
                BTreeMap::new(),
            ),
            StepSpec::AutotoolsTest => run("make", values(&["check"]), BTreeMap::new()),
        })
    }
}

/// Explicit planner inputs which are not repository policy.
///
/// The selected compiler flags already include the package's tuning and PGO
/// choices. [`TypedBuildContext`] adds the policy-owned Mold flags when
/// `mold_enabled` is true. Source preparation values are deliberately absent:
/// they are supplied to individual commands through [`TextContextOverlay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedContextInputs {
    pub package_name: String,
    pub package_version: String,
    pub package_release: i64,
    pub source_dir: String,
    pub install_root: String,
    pub build_root: String,
    pub work_dir: String,
    pub pgo_dir: String,
    pub jobs: u32,
    pub source_date_epoch: i64,
    pub pgo_stage: PgoContextStage,
    pub toolchain: ToolchainSpec,
    pub compiler_cache_enabled: bool,
    pub mold_enabled: bool,
    pub flags: CompilerFlagsSpec,
}

/// Finite value exported to package hooks as `PGO_STAGE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PgoContextStage {
    #[default]
    None,
    One,
    Two,
    Use,
}

impl PgoContextStage {
    fn as_environment_value(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::One => "ONE",
            Self::Two => "TWO",
            Self::Use => "USE",
        }
    }
}

/// Optional finite values used by one source-preparation command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextContextOverlay {
    pub source_path: Option<String>,
    pub source_destination: Option<String>,
    pub source_strip_components: Option<u32>,
}

/// Fully resolved install layout retained for hooks and structural builders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedInstallLayout {
    pub prefix: String,
    pub bindir: String,
    pub sbindir: String,
    pub includedir: String,
    pub libdir: String,
    pub libexecdir: String,
    pub datadir: String,
    pub vendordir: String,
    pub docdir: String,
    pub infodir: String,
    pub localedir: String,
    pub mandir: String,
    pub sysconfdir: String,
    pub localstatedir: String,
    pub sharedstatedir: String,
    pub runstatedir: String,
    pub sysusersdir: String,
    pub tmpfilesdir: String,
    pub udevrulesdir: String,
    pub bash_completions_dir: String,
    pub fish_completions_dir: String,
    pub elvish_completions_dir: String,
    pub zsh_completions_dir: String,
}

/// Concrete compiler executables selected from repository policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCompilerTools {
    pub cc: String,
    pub cxx: String,
    pub objc: String,
    pub objcxx: String,
    pub cpp: String,
    pub objcpp: String,
    pub objcxxcpp: String,
    pub d: String,
    pub ar: String,
    pub ld: String,
    pub objcopy: String,
    pub nm: String,
    pub ranlib: String,
    pub strip: String,
}

/// Concrete, shell-environment representations of tokenized compiler flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCompilerFlags {
    pub c: String,
    pub cxx: String,
    pub f: String,
    pub d: String,
    pub rust: String,
    pub vala: String,
    pub go: String,
    pub ld: String,
}

/// Typed finite build context resolved from one Gluon policy value.
///
/// This exists alongside the legacy [`BuildContext`] only while phase planning
/// is migrated. New lowering must use this type: it has no definition map and
/// cannot interpret `%action` or `%(definition)` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedBuildContext {
    policy: BuildPolicySpec,
    target: TargetPolicySpec,
    inputs: TypedContextInputs,
    pub layout: TypedInstallLayout,
    pub tools: ResolvedCompilerTools,
    pub flags: ResolvedCompilerFlags,
    pub environment: BTreeMap<String, String>,
}

impl TypedBuildContext {
    /// Resolve every repository-owned value needed by normal build steps.
    pub fn resolve(
        policy: &BuildPolicySpec,
        target: &TargetPolicySpec,
        inputs: TypedContextInputs,
    ) -> Result<Self, TypedContextError> {
        let resolver = TextResolver::new(policy, target, &inputs, TextContextOverlay::default());
        let layout = resolver.resolve_layout()?;
        let tools = resolver.resolve_tools()?;
        let flags = resolver.resolve_flags_record()?;
        let environment = resolver.resolve_environment(&policy.environment, &target.environment)?;

        Ok(Self {
            policy: policy.clone(),
            target: target.clone(),
            inputs,
            layout,
            tools,
            flags,
            environment,
        })
    }

    /// Resolve arbitrary policy text against the closed context enum.
    pub fn resolve_text(&self, value: &TextSpec) -> Result<String, TypedContextError> {
        self.resolve_text_with(value, &TextContextOverlay::default())
    }

    /// Resolve arbitrary policy text with source-local command values.
    pub fn resolve_text_with(
        &self,
        value: &TextSpec,
        overlay: &TextContextOverlay,
    ) -> Result<String, TypedContextError> {
        TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone()).resolve(value)
    }

    /// Lower any repository policy command without shell parsing.
    pub fn resolve_command(
        &self,
        command: &BuilderCommandSpec,
        overlay: &TextContextOverlay,
    ) -> Result<StepPlan, TypedContextError> {
        self.resolve_command_with_environment(command, &[], overlay)
    }

    /// Lower one standard package-v2 builder step from policy command data.
    ///
    /// Package-authored flags, Cargo features and installed binaries remain
    /// structural argv entries. `Shell` and `CargoEnvironment` are markers and
    /// therefore do not produce an executable step here.
    pub fn resolve_standard_step(&self, step: &StepSpec) -> Result<Option<StepPlan>, TypedContextError> {
        let (builder, command) = match step {
            StepSpec::Shell { .. } | StepSpec::CargoEnvironment => return Ok(None),
            StepSpec::CMakeConfigure { .. } => (&self.policy.builders.cmake, &self.policy.builders.cmake.setup),
            StepSpec::CMakeBuild => (&self.policy.builders.cmake, &self.policy.builders.cmake.build),
            StepSpec::CMakeInstall => (&self.policy.builders.cmake, &self.policy.builders.cmake.install),
            StepSpec::CMakeTest => (&self.policy.builders.cmake, &self.policy.builders.cmake.check),
            StepSpec::MesonSetup { .. } => (&self.policy.builders.meson, &self.policy.builders.meson.setup),
            StepSpec::MesonBuild => (&self.policy.builders.meson, &self.policy.builders.meson.build),
            StepSpec::MesonInstall => (&self.policy.builders.meson, &self.policy.builders.meson.install),
            StepSpec::MesonTest => (&self.policy.builders.meson, &self.policy.builders.meson.check),
            StepSpec::CargoFetch => (&self.policy.builders.cargo, &self.policy.builders.cargo.setup),
            StepSpec::CargoBuild { .. } => (&self.policy.builders.cargo, &self.policy.builders.cargo.build),
            StepSpec::CargoInstall { .. } => (&self.policy.builders.cargo, &self.policy.builders.cargo.install),
            StepSpec::CargoTest { .. } => (&self.policy.builders.cargo, &self.policy.builders.cargo.check),
            StepSpec::AutotoolsConfigure { .. } => {
                (&self.policy.builders.autotools, &self.policy.builders.autotools.setup)
            }
            StepSpec::AutotoolsBuild => (&self.policy.builders.autotools, &self.policy.builders.autotools.build),
            StepSpec::AutotoolsInstall => (&self.policy.builders.autotools, &self.policy.builders.autotools.install),
            StepSpec::AutotoolsTest => (&self.policy.builders.autotools, &self.policy.builders.autotools.check),
        };
        let Run {
            program,
            mut args,
            environment,
            working_dir,
        } = self.resolve_command_with_environment(command, &builder.environment, &TextContextOverlay::default())?
        else {
            unreachable!("typed command lowering only produces Run steps")
        };

        match step {
            StepSpec::CMakeConfigure { flags } | StepSpec::AutotoolsConfigure { flags } => {
                args.extend(flags.iter().cloned());
            }
            StepSpec::MesonSetup { flags } => {
                let builder_dir = args
                    .pop()
                    .ok_or(TypedContextError::MissingBuilderDirectoryArgument { builder: "meson" })?;
                args.extend(flags.iter().cloned());
                args.push(builder_dir);
            }
            StepSpec::CargoBuild { features } => append_cargo_features(&mut args, features, None),
            StepSpec::CargoTest { features } => append_cargo_features(&mut args, features, Some("--workspace")),
            StepSpec::CargoInstall { binaries } => {
                let binaries = if binaries.is_empty() {
                    std::slice::from_ref(&self.inputs.package_name)
                } else {
                    binaries.as_slice()
                };
                args.extend(
                    binaries
                        .iter()
                        .map(|binary| format!("target/{}/release/{binary}", self.target.target_triple)),
                );
            }
            StepSpec::Shell { .. }
            | StepSpec::CargoEnvironment
            | StepSpec::CMakeBuild
            | StepSpec::CMakeInstall
            | StepSpec::CMakeTest
            | StepSpec::MesonBuild
            | StepSpec::MesonInstall
            | StepSpec::MesonTest
            | StepSpec::CargoFetch
            | StepSpec::AutotoolsBuild
            | StepSpec::AutotoolsInstall
            | StepSpec::AutotoolsTest => {}
        }

        Ok(Some(Run {
            program,
            args,
            environment,
            working_dir,
        }))
    }

    fn resolve_command_with_environment(
        &self,
        command: &BuilderCommandSpec,
        builder_environment: &[EnvironmentBindingSpec],
        overlay: &TextContextOverlay,
    ) -> Result<StepPlan, TypedContextError> {
        let resolver = TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone());
        let mut environment = self.environment.clone();
        environment.extend(resolver.resolve_environment(builder_environment, &command.environment)?);

        Ok(Run {
            program: resolver.resolve(&command.program)?,
            args: command
                .args
                .iter()
                .map(|argument| resolver.resolve(argument))
                .collect::<Result<_, _>>()?,
            environment,
            working_dir: resolver.resolve(&command.working_dir)?,
        })
    }
}

fn append_cargo_features(args: &mut Vec<String>, features: &[String], before: Option<&str>) {
    if features.is_empty() {
        return;
    }
    let at = before
        .and_then(|marker| args.iter().position(|argument| argument == marker))
        .unwrap_or(args.len());
    args.splice(at..at, ["--features".to_owned(), features.join(",")]);
}

struct TextResolver<'a> {
    policy: &'a BuildPolicySpec,
    target: &'a TargetPolicySpec,
    inputs: &'a TypedContextInputs,
    overlay: TextContextOverlay,
}

impl<'a> TextResolver<'a> {
    fn new(
        policy: &'a BuildPolicySpec,
        target: &'a TargetPolicySpec,
        inputs: &'a TypedContextInputs,
        overlay: TextContextOverlay,
    ) -> Self {
        Self {
            policy,
            target,
            inputs,
            overlay,
        }
    }

    fn resolve(&self, value: &TextSpec) -> Result<String, TypedContextError> {
        self.resolve_inner(value, &mut Vec::new())
    }

    fn resolve_inner(&self, value: &TextSpec, resolving: &mut Vec<ContextValue>) -> Result<String, TypedContextError> {
        match value {
            TextSpec::Literal(value) => Ok(value.clone()),
            TextSpec::Context(value) => self.resolve_context(*value, resolving),
            TextSpec::Concat(parts) => parts
                .iter()
                .map(|part| self.resolve_inner(part, resolving))
                .collect::<Result<Vec<_>, _>>()
                .map(|parts| parts.concat()),
        }
    }

    fn resolve_context(
        &self,
        value: ContextValue,
        resolving: &mut Vec<ContextValue>,
    ) -> Result<String, TypedContextError> {
        if let Some(start) = resolving.iter().position(|candidate| *candidate == value) {
            let mut chain = resolving[start..].to_vec();
            chain.push(value);
            return Err(TypedContextError::RecursiveContext { chain });
        }
        resolving.push(value);
        let resolved = self.resolve_context_inner(value, resolving);
        resolving.pop();
        resolved
    }

    fn resolve_context_inner(
        &self,
        value: ContextValue,
        resolving: &mut Vec<ContextValue>,
    ) -> Result<String, TypedContextError> {
        let input = self.inputs;
        let cache = &self.policy.build_root.compiler_cache;
        let tools = self.selected_tools();
        let layout = &self.policy.layout;
        let text = |value: &TextSpec, resolving: &mut Vec<ContextValue>| self.resolve_inner(value, resolving);
        let flags = |values: &[TextSpec], mold: &[TextSpec], resolving: &mut Vec<ContextValue>| {
            self.resolve_flag_values(values, mold, resolving)
        };

        match value {
            ContextValue::PackageName => Ok(input.package_name.clone()),
            ContextValue::PackageVersion => Ok(input.package_version.clone()),
            ContextValue::PackageRelease => Ok(input.package_release.to_string()),
            ContextValue::SourceDir => Ok(input.source_dir.clone()),
            ContextValue::InstallRoot => Ok(input.install_root.clone()),
            ContextValue::BuildRoot => Ok(input.build_root.clone()),
            ContextValue::WorkDir => Ok(input.work_dir.clone()),
            ContextValue::BuilderDir => Ok(self.policy.build_subdir.clone()),
            ContextValue::PgoDir => Ok(input.pgo_dir.clone()),
            ContextValue::Jobs => Ok(input.jobs.to_string()),
            ContextValue::SourceDateEpoch => Ok(input.source_date_epoch.to_string()),
            ContextValue::PgoStage => Ok(input.pgo_stage.as_environment_value().to_owned()),
            ContextValue::TargetTriple => Ok(self.target.target_triple.clone()),
            ContextValue::BuildPlatform => Ok(self.target.build_triple.clone()),
            ContextValue::HostPlatform => Ok(self.target.host_triple.clone()),
            ContextValue::LibSuffix => Ok(self.target.lib_suffix.clone()),
            ContextValue::Prefix => text(&layout.prefix, resolving),
            ContextValue::BinDir => text(&layout.bindir, resolving),
            ContextValue::SbinDir => text(&layout.sbindir, resolving),
            ContextValue::IncludeDir => text(&layout.includedir, resolving),
            ContextValue::LibDir => text(&layout.libdir, resolving),
            ContextValue::LibexecDir => text(&layout.libexecdir, resolving),
            ContextValue::DataDir => text(&layout.datadir, resolving),
            ContextValue::VendorDir => text(&layout.vendordir, resolving),
            ContextValue::DocDir => text(&layout.docdir, resolving),
            ContextValue::InfoDir => text(&layout.infodir, resolving),
            ContextValue::LocaleDir => text(&layout.localedir, resolving),
            ContextValue::ManDir => text(&layout.mandir, resolving),
            ContextValue::SysconfDir => text(&layout.sysconfdir, resolving),
            ContextValue::LocalStateDir => text(&layout.localstatedir, resolving),
            ContextValue::SharedStateDir => text(&layout.sharedstatedir, resolving),
            ContextValue::RunStateDir => text(&layout.runstatedir, resolving),
            ContextValue::CFlags => flags(&input.flags.c, &self.mold_flags().c, resolving),
            ContextValue::CxxFlags => flags(&input.flags.cxx, &self.mold_flags().cxx, resolving),
            ContextValue::FFlags => flags(&input.flags.f, &self.mold_flags().f, resolving),
            ContextValue::DFlags => flags(&input.flags.d, &self.mold_flags().d, resolving),
            ContextValue::RustFlags => flags(&input.flags.rust, &self.mold_flags().rust, resolving),
            ContextValue::ValaFlags => flags(&input.flags.vala, &self.mold_flags().vala, resolving),
            ContextValue::GoFlags => flags(&input.flags.go, &self.mold_flags().go, resolving),
            ContextValue::LdFlags => flags(&input.flags.ld, &self.mold_flags().ld, resolving),
            ContextValue::Cc => text(&tools.cc, resolving),
            ContextValue::Cxx => text(&tools.cxx, resolving),
            ContextValue::Objc => text(&tools.objc, resolving),
            ContextValue::Objcxx => text(&tools.objcxx, resolving),
            ContextValue::Cpp => text(&tools.cpp, resolving),
            ContextValue::Objcpp => text(&tools.objcpp, resolving),
            ContextValue::Objcxxcpp => text(&tools.objcxxcpp, resolving),
            ContextValue::D => text(&tools.d, resolving),
            ContextValue::Ar => text(&tools.ar, resolving),
            ContextValue::Ld if input.mold_enabled => text(&self.policy.build_root.mold.linker, resolving),
            ContextValue::Ld => text(&tools.ld, resolving),
            ContextValue::Objcopy => text(&tools.objcopy, resolving),
            ContextValue::Nm => text(&tools.nm, resolving),
            ContextValue::Ranlib => text(&tools.ranlib, resolving),
            ContextValue::Strip => text(&tools.strip, resolving),
            ContextValue::CompilerPath if input.compiler_cache_enabled => Ok(cache.compiler_path.clone()),
            ContextValue::CompilerPath => Ok(cache.default_path.clone()),
            ContextValue::CcacheDir => Ok(cache.ccache_dir.clone()),
            ContextValue::SccacheDir => Ok(cache.sccache_dir.clone()),
            ContextValue::GoCacheDir => Ok(cache.go_cache_dir.clone()),
            ContextValue::GoModCacheDir => Ok(cache.go_mod_cache_dir.clone()),
            ContextValue::CargoCacheDir => Ok(cache.cargo_cache_dir.clone()),
            ContextValue::ZigCacheDir => Ok(cache.zig_cache_dir.clone()),
            ContextValue::RustcWrapper => Ok(cache.rustc_wrapper.clone()),
            ContextValue::SourcePath => self
                .overlay
                .source_path
                .clone()
                .ok_or(TypedContextError::MissingContext { value }),
            ContextValue::SourceDestination => self
                .overlay
                .source_destination
                .clone()
                .ok_or(TypedContextError::MissingContext { value }),
            ContextValue::SourceStripComponents => self
                .overlay
                .source_strip_components
                .map(|value| value.to_string())
                .ok_or(TypedContextError::MissingContext { value }),
        }
    }

    fn selected_tools(&self) -> &CompilerToolsSpec {
        match self.inputs.toolchain {
            ToolchainSpec::Llvm => &self.policy.toolchains.llvm,
            ToolchainSpec::Gnu => &self.policy.toolchains.gnu,
        }
    }

    fn mold_flags(&self) -> &CompilerFlagsSpec {
        if self.inputs.mold_enabled {
            &self.policy.build_root.mold.flags
        } else {
            static EMPTY: std::sync::LazyLock<CompilerFlagsSpec> = std::sync::LazyLock::new(CompilerFlagsSpec::default);
            &EMPTY
        }
    }

    fn resolve_flag_values(
        &self,
        selected: &[TextSpec],
        mold: &[TextSpec],
        resolving: &mut Vec<ContextValue>,
    ) -> Result<String, TypedContextError> {
        selected
            .iter()
            .chain(mold)
            .map(|value| self.resolve_inner(value, resolving))
            .collect::<Result<Vec<_>, _>>()
            .map(|values| {
                values
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    }

    fn resolve_environment(
        &self,
        first: &[EnvironmentBindingSpec],
        second: &[EnvironmentBindingSpec],
    ) -> Result<BTreeMap<String, String>, TypedContextError> {
        first
            .iter()
            .chain(second)
            .filter(|binding| match binding.condition {
                EnvironmentCondition::Always => true,
                EnvironmentCondition::CompilerCacheEnabled => self.inputs.compiler_cache_enabled,
                EnvironmentCondition::CompilerCacheDisabled => !self.inputs.compiler_cache_enabled,
            })
            .map(|binding| Ok((binding.name.clone(), self.resolve(&binding.value)?)))
            .collect()
    }

    fn resolve_layout(&self) -> Result<TypedInstallLayout, TypedContextError> {
        let layout = &self.policy.layout;
        Ok(TypedInstallLayout {
            prefix: self.resolve(&layout.prefix)?,
            bindir: self.resolve(&layout.bindir)?,
            sbindir: self.resolve(&layout.sbindir)?,
            includedir: self.resolve(&layout.includedir)?,
            libdir: self.resolve(&layout.libdir)?,
            libexecdir: self.resolve(&layout.libexecdir)?,
            datadir: self.resolve(&layout.datadir)?,
            vendordir: self.resolve(&layout.vendordir)?,
            docdir: self.resolve(&layout.docdir)?,
            infodir: self.resolve(&layout.infodir)?,
            localedir: self.resolve(&layout.localedir)?,
            mandir: self.resolve(&layout.mandir)?,
            sysconfdir: self.resolve(&layout.sysconfdir)?,
            localstatedir: self.resolve(&layout.localstatedir)?,
            sharedstatedir: self.resolve(&layout.sharedstatedir)?,
            runstatedir: self.resolve(&layout.runstatedir)?,
            sysusersdir: self.resolve(&layout.sysusersdir)?,
            tmpfilesdir: self.resolve(&layout.tmpfilesdir)?,
            udevrulesdir: self.resolve(&layout.udevrulesdir)?,
            bash_completions_dir: self.resolve(&layout.bash_completions_dir)?,
            fish_completions_dir: self.resolve(&layout.fish_completions_dir)?,
            elvish_completions_dir: self.resolve(&layout.elvish_completions_dir)?,
            zsh_completions_dir: self.resolve(&layout.zsh_completions_dir)?,
        })
    }

    fn resolve_tools(&self) -> Result<ResolvedCompilerTools, TypedContextError> {
        let value = |context| self.resolve(&TextSpec::Context(context));
        Ok(ResolvedCompilerTools {
            cc: value(ContextValue::Cc)?,
            cxx: value(ContextValue::Cxx)?,
            objc: value(ContextValue::Objc)?,
            objcxx: value(ContextValue::Objcxx)?,
            cpp: value(ContextValue::Cpp)?,
            objcpp: value(ContextValue::Objcpp)?,
            objcxxcpp: value(ContextValue::Objcxxcpp)?,
            d: value(ContextValue::D)?,
            ar: value(ContextValue::Ar)?,
            ld: value(ContextValue::Ld)?,
            objcopy: value(ContextValue::Objcopy)?,
            nm: value(ContextValue::Nm)?,
            ranlib: value(ContextValue::Ranlib)?,
            strip: value(ContextValue::Strip)?,
        })
    }

    fn resolve_flags_record(&self) -> Result<ResolvedCompilerFlags, TypedContextError> {
        let value = |context| self.resolve(&TextSpec::Context(context));
        Ok(ResolvedCompilerFlags {
            c: value(ContextValue::CFlags)?,
            cxx: value(ContextValue::CxxFlags)?,
            f: value(ContextValue::FFlags)?,
            d: value(ContextValue::DFlags)?,
            rust: value(ContextValue::RustFlags)?,
            vala: value(ContextValue::ValaFlags)?,
            go: value(ContextValue::GoFlags)?,
            ld: value(ContextValue::LdFlags)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypedContextError {
    #[error("policy text requires missing finite context value {value:?}")]
    MissingContext { value: ContextValue },
    #[error("policy text contains a recursive context reference: {chain:?}")]
    RecursiveContext { chain: Vec<ContextValue> },
    #[error("policy builders.{builder}.setup has no final builder-directory argument")]
    MissingBuilderDirectoryArgument { builder: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuildPolicy;

    fn context() -> BuildContext {
        let prefix = "/usr".to_owned();
        BuildContext {
            package_name: "example".to_owned(),
            work_dir: "/mason/build/x86_64/source".to_owned(),
            build_subdir: "aerynos-builddir".to_owned(),
            install_root: "/mason/install".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            build_platform: "x86_64-aerynos-linux".to_owned(),
            host_platform: "x86_64-aerynos-linux".to_owned(),
            jobs: 8,
            layout: InstallLayout {
                bindir: "/usr/bin".to_owned(),
                sbindir: "/usr/sbin".to_owned(),
                includedir: "/usr/include".to_owned(),
                libdir: "/usr/lib".to_owned(),
                libexecdir: "/usr/lib/example".to_owned(),
                datadir: "/usr/share".to_owned(),
                mandir: "/usr/share/man".to_owned(),
                infodir: "/usr/share/info".to_owned(),
                localedir: "/usr/share/locale".to_owned(),
                sysconfdir: "/etc".to_owned(),
                localstatedir: "/var".to_owned(),
                sharedstatedir: "/var/lib".to_owned(),
                prefix,
            },
            environment: BTreeMap::from([("SOURCE_DATE_EPOCH".to_owned(), "1700000000".to_owned())]),
        }
    }

    fn typed_context(target_name: &str, compiler_cache_enabled: bool, mold_enabled: bool) -> TypedBuildContext {
        let policy = BuildPolicy::repository_for_tests();
        let target = policy.target(target_name).unwrap();
        TypedBuildContext::resolve(
            &policy.spec,
            target,
            TypedContextInputs {
                package_name: "example".to_owned(),
                package_version: "1.2.3".to_owned(),
                package_release: 4,
                source_dir: "/mason/sourcedir".to_owned(),
                install_root: "/mason/install".to_owned(),
                build_root: format!("/mason/build/{target_name}"),
                work_dir: format!("/mason/build/{target_name}/source"),
                pgo_dir: format!("/mason/build/{target_name}-pgo"),
                jobs: 8,
                source_date_epoch: 1_700_000_000,
                pgo_stage: PgoContextStage::Two,
                toolchain: ToolchainSpec::Llvm,
                compiler_cache_enabled,
                mold_enabled,
                flags: CompilerFlagsSpec {
                    c: vec![
                        TextSpec::Literal("-O2".to_owned()),
                        TextSpec::Concat(vec![
                            TextSpec::Literal("-flto=".to_owned()),
                            TextSpec::Context(ContextValue::Jobs),
                        ]),
                    ],
                    rust: vec![TextSpec::Concat(vec![
                        TextSpec::Literal("-Cprofile-use=".to_owned()),
                        TextSpec::Context(ContextValue::PgoDir),
                    ])],
                    ..CompilerFlagsSpec::default()
                },
            },
        )
        .unwrap()
    }

    #[test]
    fn cmake_and_meson_are_argv_preserving_run_steps() {
        let context = context();
        let Run {
            program,
            args,
            working_dir,
            ..
        } = context
            .resolve_standard_step(&StepSpec::CMakeConfigure {
                flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
            })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "cmake");
        assert_eq!(working_dir, context.work_dir);
        assert!(args.contains(&"-DBUILD_TESTS=OFF".to_owned()));
        assert!(args.windows(2).any(|values| values == ["-B", "aerynos-builddir"]));

        let Run { program, args, .. } = context.resolve_standard_step(&StepSpec::MesonBuild).unwrap() else {
            panic!("expected run")
        };
        assert_eq!(program, "meson");
        assert_eq!(args, ["compile", "--verbose", "-j", "8", "-C", "aerynos-builddir"]);
    }

    #[test]
    fn cargo_and_autotools_resolve_context_without_templates() {
        let context = context();
        let Run { program, args, .. } = context
            .resolve_standard_step(&StepSpec::CargoBuild {
                features: vec!["cli".to_owned(), "tls".to_owned()],
            })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "cargo");
        assert!(
            args.windows(2)
                .any(|values| values == ["--target", "x86_64-unknown-linux-gnu"])
        );
        assert!(args.windows(2).any(|values| values == ["--features", "cli,tls"]));

        let Run {
            program,
            args,
            environment,
            ..
        } = context
            .resolve_standard_step(&StepSpec::AutotoolsConfigure { flags: Vec::new() })
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "/usr/bin/dash");
        assert!(args.contains(&"--build=x86_64-aerynos-linux".to_owned()));
        assert_eq!(environment["CONFIG_SHELL"], "/usr/bin/dash");
    }

    #[test]
    fn shell_and_environment_markers_never_enter_standard_lowering() {
        let context = context();
        assert!(
            context
                .resolve_standard_step(&StepSpec::Shell {
                    script: "%literal".to_owned()
                })
                .is_none()
        );
        assert!(context.resolve_standard_step(&StepSpec::CargoEnvironment).is_none());
    }

    #[test]
    fn typed_context_resolves_policy_layout_tools_flags_and_cache_conditions() {
        let context = typed_context("x86_64", false, true);

        assert_eq!(context.layout.libdir, "/usr/lib");
        assert_eq!(context.layout.libexecdir, "/usr/lib/example");
        assert_eq!(context.tools.cc, "clang");
        assert_eq!(context.tools.ld, "ld.mold");
        assert_eq!(context.flags.c, "-O2 -flto=8 -fuse-ld=mold");
        assert_eq!(
            context.flags.rust,
            "-Cprofile-use=/mason/build/x86_64-pgo -Clink-arg=-fuse-ld=mold"
        );
        assert_eq!(context.environment["PATH"], "/usr/bin:/bin");
        assert_eq!(context.environment["CFLAGS"], context.flags.c);
        assert_eq!(context.environment["PGO_STAGE"], "TWO");
        assert_eq!(context.environment["GOAMD64"], "v2");
        assert_eq!(
            context.environment["PKG_CONFIG_PATH"],
            "/usr/lib/pkgconfig:/usr/share/pkgconfig"
        );
        assert!(!context.environment.contains_key("CCACHE_DIR"));

        let cached = typed_context("x86_64", true, false);
        assert_eq!(cached.environment["PATH"], "/usr/lib/ccache/bin:/usr/bin:/bin");
        assert_eq!(cached.environment["CCACHE_DIR"], "/mason/ccache");
        assert_eq!(cached.tools.ld, "ld.lld");
        assert!(!cached.flags.c.contains("mold"));
    }

    #[test]
    fn compiler_flag_tokens_preserve_policy_order_and_multiplicity() {
        let policy = BuildPolicy::repository_for_tests();
        let target = policy.target("x86_64").unwrap();
        let mut inputs = typed_context("x86_64", false, false).inputs;
        inputs.flags.rust = vec![
            TextSpec::Literal("-C".to_owned()),
            TextSpec::Literal("opt-level=3".to_owned()),
            TextSpec::Literal("-C".to_owned()),
            TextSpec::Literal("codegen-units=1".to_owned()),
        ];

        let context = TypedBuildContext::resolve(&policy.spec, target, inputs).unwrap();
        assert_eq!(context.flags.rust, "-C opt-level=3 -C codegen-units=1");
        assert_eq!(context.environment["RUSTFLAGS"], context.flags.rust);
    }

    #[test]
    fn target_environment_overrides_global_tool_values() {
        let context = typed_context("emul32/x86_64", false, false);

        assert_eq!(context.environment["CC"], "clang -m32");
        assert_eq!(context.environment["CXX"], "clang++ -m32");
        assert_eq!(context.environment["CPP"], "clang-cpp -m32");
        assert_eq!(
            context.environment["PKG_CONFIG_PATH"],
            "/usr/lib32/pkgconfig:/usr/share/pkgconfig:/usr/lib/pkgconfig"
        );
    }

    #[test]
    fn standard_builder_commands_come_from_policy_and_keep_package_arguments() {
        let context = typed_context("x86_64", false, false);
        let Run {
            program,
            args,
            working_dir,
            ..
        } = context
            .resolve_standard_step(&StepSpec::CMakeConfigure {
                flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
            })
            .unwrap()
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "cmake");
        assert_eq!(working_dir, "/mason/build/x86_64/source");
        assert_eq!(&args[..4], ["-G", "Ninja", "-B", "aerynos-builddir"]);
        assert_eq!(args.last().unwrap(), "-DBUILD_TESTS=OFF");

        let Run { args, .. } = context
            .resolve_standard_step(&StepSpec::MesonSetup {
                flags: vec!["-Ddocs=false".to_owned()],
            })
            .unwrap()
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(&args[args.len() - 2..], ["-Ddocs=false", "aerynos-builddir"]);

        let Run { args, environment, .. } = context
            .resolve_standard_step(&StepSpec::CargoTest {
                features: vec!["cli".to_owned(), "tls".to_owned()],
            })
            .unwrap()
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(&args[args.len() - 3..], ["--features", "cli,tls", "--workspace"]);
        assert_eq!(environment["CARGO_BUILD_DEP_INFO_BASEDIR"], context.inputs.work_dir);

        let Run { args, .. } = context
            .resolve_standard_step(&StepSpec::CargoInstall {
                binaries: vec!["one".to_owned(), "two".to_owned()],
            })
            .unwrap()
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(
            &args[args.len() - 2..],
            [
                "target/x86_64-unknown-linux-gnu/release/one",
                "target/x86_64-unknown-linux-gnu/release/two",
            ]
        );

        let Run { args, .. } = context
            .resolve_standard_step(&StepSpec::AutotoolsConfigure { flags: Vec::new() })
            .unwrap()
            .unwrap()
        else {
            panic!("expected run")
        };
        assert!(args.contains(&"--build=x86_64-aerynos-linux".to_owned()));
        assert!(args.contains(&"--host=x86_64-aerynos-linux".to_owned()));
    }

    #[test]
    fn changing_policy_command_data_changes_frozen_argv() {
        let mut policy = BuildPolicy::repository_for_tests();
        let target = policy.target("x86_64").unwrap().clone();
        policy.spec.builders.cmake.build.program = TextSpec::Literal("policy-cmake".to_owned());
        policy.spec.builders.cmake.build.args = vec![
            TextSpec::Literal("--policy-build".to_owned()),
            TextSpec::Context(ContextValue::BuilderDir),
        ];
        let inputs = typed_context("x86_64", false, false).inputs;
        let context = TypedBuildContext::resolve(&policy.spec, &target, inputs).unwrap();

        let Run { program, args, .. } = context.resolve_standard_step(&StepSpec::CMakeBuild).unwrap().unwrap() else {
            panic!("expected run")
        };
        assert_eq!(program, "policy-cmake");
        assert_eq!(args, ["--policy-build", "aerynos-builddir"]);
    }

    #[test]
    fn source_context_is_command_local_and_missing_values_are_actionable() {
        let context = typed_context("x86_64", false, false);
        assert_eq!(
            context.resolve_text(&TextSpec::Context(ContextValue::SourcePath)),
            Err(TypedContextError::MissingContext {
                value: ContextValue::SourcePath,
            })
        );

        let overlay = TextContextOverlay {
            source_path: Some("/mason/sourcedir/source archive.tar.xz".to_owned()),
            source_destination: Some("source tree".to_owned()),
            source_strip_components: Some(2),
        };
        let Run {
            program,
            args,
            working_dir,
            ..
        } = context
            .resolve_command(&context.policy.sources.archive.unpack, &overlay)
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program, "bsdtar-static");
        assert_eq!(working_dir, "/mason/build/x86_64");
        assert_eq!(
            args,
            [
                "xf",
                "/mason/sourcedir/source archive.tar.xz",
                "-C",
                "source tree",
                "--strip-components=2",
                "--no-same-owner",
            ]
        );
    }

    #[test]
    fn recursive_policy_context_is_rejected_without_interpolation() {
        let mut policy = BuildPolicy::repository_for_tests();
        policy.spec.layout.prefix = TextSpec::Context(ContextValue::Prefix);
        let target = policy.target("x86_64").unwrap().clone();
        let mut inputs = typed_context("x86_64", false, false).inputs;
        inputs.flags = CompilerFlagsSpec::default();

        assert_eq!(
            TypedBuildContext::resolve(&policy.spec, &target, inputs),
            Err(TypedContextError::RecursiveContext {
                chain: vec![ContextValue::Prefix, ContextValue::Prefix],
            })
        );
    }
}
