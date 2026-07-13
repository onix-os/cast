// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed lowering context for standard package-v3 build steps.
//!
//! This boundary deliberately accepts concrete values. It does not know about
//! legacy actions, definition names, or the script parser.

use std::collections::BTreeMap;

use stone_recipe::{
    ToolchainSpec,
    build_policy::{
        BuildPolicySpec, BuildProgramSpec, BuilderCommandSpec, CompilerFlagsSpec, CompilerToolsSpec, ContextValue,
        EnvironmentBindingSpec, EnvironmentCondition, TargetPolicySpec, TextSpec,
    },
    derivation::{ExecutablePlan, RelationPlan, StepPlan, StepPlan::Run},
    package::StepSpec,
};
use thiserror::Error;

/// Explicit planner inputs which are not repository policy.
///
/// The selected compiler flags already include the package's tuning and PGO
/// choices. [`BuildContext`] adds the policy-owned Mold flags when
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
pub struct InstallLayout {
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
/// It has no definition map and cannot interpret `%action` or
/// `%(definition)` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildContext {
    policy: BuildPolicySpec,
    target: TargetPolicySpec,
    inputs: TypedContextInputs,
    pub layout: InstallLayout,
    pub tools: ResolvedCompilerTools,
    pub flags: ResolvedCompilerFlags,
    pub environment: BTreeMap<String, String>,
}

impl BuildContext {
    /// Resolve every repository-owned value needed by normal build steps.
    pub fn resolve(
        policy: &BuildPolicySpec,
        target: &TargetPolicySpec,
        inputs: TypedContextInputs,
    ) -> Result<Self, ContextError> {
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

    /// Add a typed environment layer selected by the package's structural
    /// environment markers.
    pub fn extend_environment(&mut self, bindings: &[EnvironmentBindingSpec]) -> Result<(), ContextError> {
        let resolver = TextResolver::new(&self.policy, &self.target, &self.inputs, TextContextOverlay::default());
        let environment = resolver.resolve_environment(bindings, &[])?;
        self.environment.extend(environment);
        Ok(())
    }

    /// Resolve arbitrary policy text against the closed context enum.
    pub fn resolve_text(&self, value: &TextSpec) -> Result<String, ContextError> {
        self.resolve_text_with(value, &TextContextOverlay::default())
    }

    /// Resolve arbitrary policy text with source-local command values.
    pub fn resolve_text_with(&self, value: &TextSpec, overlay: &TextContextOverlay) -> Result<String, ContextError> {
        TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone()).resolve(value)
    }

    /// Lower any repository policy command without shell parsing.
    pub fn resolve_command(
        &self,
        command: &BuilderCommandSpec,
        overlay: &TextContextOverlay,
    ) -> Result<StepPlan, ContextError> {
        self.resolve_command_with_environment(command, overlay)
    }

    /// Lower one standard package-v3 builder step from policy command data.
    ///
    /// Package-authored flags, Cargo features and installed binaries remain
    /// structural argv entries. Package-authored `Shell` steps are lowered by
    /// the phase planner and therefore do not produce an executable step here.
    pub fn resolve_standard_step(&self, step: &StepSpec) -> Result<Option<StepPlan>, ContextError> {
        let command = match step {
            StepSpec::Run { .. } | StepSpec::Shell { .. } => return Ok(None),
            StepSpec::CMakeConfigure { .. } => &self.policy.builders.cmake.setup,
            StepSpec::CMakeBuild => &self.policy.builders.cmake.build,
            StepSpec::CMakeInstall => &self.policy.builders.cmake.install,
            StepSpec::CMakeTest => &self.policy.builders.cmake.check,
            StepSpec::MesonSetup { .. } => &self.policy.builders.meson.setup,
            StepSpec::MesonBuild => &self.policy.builders.meson.build,
            StepSpec::MesonInstall => &self.policy.builders.meson.install,
            StepSpec::MesonTest => &self.policy.builders.meson.check,
            StepSpec::CargoFetch => &self.policy.builders.cargo.setup,
            StepSpec::CargoBuild { .. } => &self.policy.builders.cargo.build,
            StepSpec::CargoInstall { .. } => &self.policy.builders.cargo.install,
            StepSpec::CargoTest { .. } => &self.policy.builders.cargo.check,
            StepSpec::AutotoolsConfigure { .. } => &self.policy.builders.autotools.setup,
            StepSpec::AutotoolsBuild => &self.policy.builders.autotools.build,
            StepSpec::AutotoolsInstall => &self.policy.builders.autotools.install,
            StepSpec::AutotoolsTest => &self.policy.builders.autotools.check,
        };
        let Run {
            program,
            mut args,
            environment,
            working_dir,
        } = self.resolve_command_with_environment(command, &TextContextOverlay::default())?
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
                    .ok_or(ContextError::MissingBuilderDirectoryArgument { builder: "meson" })?;
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
            StepSpec::Run { .. }
            | StepSpec::Shell { .. }
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
        overlay: &TextContextOverlay,
    ) -> Result<StepPlan, ContextError> {
        let resolver = TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone());
        let mut environment = self.environment.clone();
        environment.extend(resolver.resolve_environment(&command.environment, &[])?);

        Ok(Run {
            program: freeze_policy_program(&command.program),
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

pub(crate) fn freeze_policy_program(program: &BuildProgramSpec) -> ExecutablePlan {
    let dependency = program
        .requirement
        .dependency()
        .expect("validated build policy program requirement");
    ExecutablePlan {
        path: program.path.clone(),
        requirement: RelationPlan::from(dependency),
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

    fn resolve(&self, value: &TextSpec) -> Result<String, ContextError> {
        self.resolve_inner(value, &mut Vec::new())
    }

    fn resolve_inner(&self, value: &TextSpec, resolving: &mut Vec<ContextValue>) -> Result<String, ContextError> {
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

    fn resolve_context(&self, value: ContextValue, resolving: &mut Vec<ContextValue>) -> Result<String, ContextError> {
        if let Some(start) = resolving.iter().position(|candidate| *candidate == value) {
            let mut chain = resolving[start..].to_vec();
            chain.push(value);
            return Err(ContextError::RecursiveContext { chain });
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
    ) -> Result<String, ContextError> {
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
                .ok_or(ContextError::MissingContext { value }),
            ContextValue::SourceDestination => self
                .overlay
                .source_destination
                .clone()
                .ok_or(ContextError::MissingContext { value }),
            ContextValue::SourceStripComponents => self
                .overlay
                .source_strip_components
                .map(|value| value.to_string())
                .ok_or(ContextError::MissingContext { value }),
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
    ) -> Result<String, ContextError> {
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
    ) -> Result<BTreeMap<String, String>, ContextError> {
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

    fn resolve_layout(&self) -> Result<InstallLayout, ContextError> {
        let layout = &self.policy.layout;
        Ok(InstallLayout {
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

    fn resolve_tools(&self) -> Result<ResolvedCompilerTools, ContextError> {
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

    fn resolve_flags_record(&self) -> Result<ResolvedCompilerFlags, ContextError> {
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
pub enum ContextError {
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
    use stone_recipe::build_policy::BuildToolSpec;

    fn fixture_context(target_name: &str, compiler_cache_enabled: bool, mold_enabled: bool) -> BuildContext {
        let policy = BuildPolicy::repository_for_tests();
        let target = policy.target(target_name).unwrap();
        BuildContext::resolve(
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
    fn typed_context_resolves_policy_layout_tools_flags_and_cache_conditions() {
        let context = fixture_context("x86_64", false, true);

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

        let cached = fixture_context("x86_64", true, false);
        assert_eq!(cached.environment["PATH"], "/usr/lib/ccache/bin:/usr/bin:/bin");
        assert_eq!(cached.environment["CCACHE_DIR"], "/mason/ccache");
        assert_eq!(cached.tools.ld, "ld.lld");
        assert!(!cached.flags.c.contains("mold"));
    }

    #[test]
    fn compiler_flag_tokens_preserve_policy_order_and_multiplicity() {
        let policy = BuildPolicy::repository_for_tests();
        let target = policy.target("x86_64").unwrap();
        let mut inputs = fixture_context("x86_64", false, false).inputs;
        inputs.flags.rust = vec![
            TextSpec::Literal("-C".to_owned()),
            TextSpec::Literal("opt-level=3".to_owned()),
            TextSpec::Literal("-C".to_owned()),
            TextSpec::Literal("codegen-units=1".to_owned()),
        ];

        let context = BuildContext::resolve(&policy.spec, target, inputs).unwrap();
        assert_eq!(context.flags.rust, "-C opt-level=3 -C codegen-units=1");
        assert_eq!(context.environment["RUSTFLAGS"], context.flags.rust);
    }

    #[test]
    fn target_environment_overrides_global_tool_values() {
        let context = fixture_context("emul32/x86_64", false, false);

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
        let mut context = fixture_context("x86_64", false, false);
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
        assert_eq!(program.path, "/usr/bin/cmake");
        assert_eq!(program.requirement.canonical_name(), "binary(cmake)");
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

        let Run { environment, .. } = context.resolve_standard_step(&StepSpec::CargoFetch).unwrap().unwrap() else {
            panic!("expected run")
        };
        assert!(!environment.contains_key("CARGO_BUILD_DEP_INFO_BASEDIR"));

        let cargo_environment = context.policy.builders.cargo.environment.clone();
        context.extend_environment(&cargo_environment).unwrap();
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
        policy.spec.builders.cmake.build.program.path = "/usr/bin/policy-cmake".to_owned();
        policy.spec.builders.cmake.build.program.requirement = BuildToolSpec::Binary("policy-cmake".to_owned());
        policy.spec.builders.cmake.build.args = vec![
            TextSpec::Literal("--policy-build".to_owned()),
            TextSpec::Context(ContextValue::BuilderDir),
        ];
        let inputs = fixture_context("x86_64", false, false).inputs;
        let context = BuildContext::resolve(&policy.spec, &target, inputs).unwrap();

        let Run { program, args, .. } = context.resolve_standard_step(&StepSpec::CMakeBuild).unwrap().unwrap() else {
            panic!("expected run")
        };
        assert_eq!(program.path, "/usr/bin/policy-cmake");
        assert_eq!(program.requirement.canonical_name(), "binary(policy-cmake)");
        assert_eq!(args, ["--policy-build", "aerynos-builddir"]);
    }

    #[test]
    fn source_context_is_command_local_and_missing_values_are_actionable() {
        let context = fixture_context("x86_64", false, false);
        assert_eq!(
            context.resolve_text(&TextSpec::Context(ContextValue::SourcePath)),
            Err(ContextError::MissingContext {
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
        assert_eq!(program.path, "/usr/bin/bsdtar-static");
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
        let mut inputs = fixture_context("x86_64", false, false).inputs;
        inputs.flags = CompilerFlagsSpec::default();

        assert_eq!(
            BuildContext::resolve(&policy.spec, &target, inputs),
            Err(ContextError::RecursiveContext {
                chain: vec![ContextValue::Prefix, ContextValue::Prefix],
            })
        );
    }
}
