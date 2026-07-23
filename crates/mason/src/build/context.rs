//! Typed lowering context for standard package-v3 build steps.
//!
//! This boundary deliberately accepts concrete values. It does not know about
//! legacy actions, definition names, or the script parser.

use std::{cell::Cell, collections::BTreeMap};

use stone_recipe::{
    ToolchainSpec,
    build_policy::{
        BuildCommandSpec, BuildPolicyConversionError, BuildPolicySpec, BuildPolicyValidationLimits, BuildProgramSpec,
        BuilderCommandSpec, CompilerFlagsSpec, CompilerToolsSpec, ContextValue, EnvironmentBindingSpec,
        EnvironmentCondition, TargetPolicySpec, TextSpec, validate_environment_bindings_with_limits,
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

impl InstallLayout {
    const RESOLVED_ITEMS: usize = 23;
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
    pub ar: String,
    pub ld: String,
    pub objcopy: String,
    pub nm: String,
    pub ranlib: String,
    pub strip: String,
}

impl ResolvedCompilerTools {
    const RESOLVED_ITEMS: usize = 13;
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

impl ResolvedCompilerFlags {
    const RESOLVED_ITEMS: usize = 8;
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
    limits: BuildPolicyValidationLimits,
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
        Self::resolve_with_limits(policy, target, inputs, BuildPolicyValidationLimits::default())
    }

    /// Resolve with the same finite ceilings used to accept repository policy.
    pub fn resolve_with_limits(
        policy: &BuildPolicySpec,
        target: &TargetPolicySpec,
        inputs: TypedContextInputs,
        limits: BuildPolicyValidationLimits,
    ) -> Result<Self, ContextError> {
        policy.validate_with_limits(limits)?;
        if !policy.targets.iter().any(|candidate| std::ptr::eq(candidate, target)) {
            return Err(ContextError::TargetNotInPolicy);
        }
        let resolver = TextResolver::new(policy, target, &inputs, TextContextOverlay::default(), limits);
        resolver.ensure_item_capacity(
            InstallLayout::RESOLVED_ITEMS
                .saturating_add(ResolvedCompilerTools::RESOLVED_ITEMS)
                .saturating_add(ResolvedCompilerFlags::RESOLVED_ITEMS)
                .saturating_add(resolver.active_environment_count(&policy.environment, &target.environment)),
        )?;
        let layout = resolver.resolve_layout()?;
        let tools = resolver.resolve_tools()?;
        let flags = resolver.resolve_flags_record()?;
        let environment = resolver.resolve_environment(&policy.environment, &target.environment)?;

        Ok(Self {
            policy: policy.clone(),
            target: target.clone(),
            inputs,
            limits,
            layout,
            tools,
            flags,
            environment,
        })
    }

    /// Add a typed environment layer selected by the package's structural
    /// environment markers.
    pub fn extend_environment(&mut self, bindings: &[EnvironmentBindingSpec]) -> Result<(), ContextError> {
        validate_environment_bindings_with_limits(bindings, self.limits)?;
        let resolver = TextResolver::new(
            &self.policy,
            &self.target,
            &self.inputs,
            TextContextOverlay::default(),
            self.limits,
        );
        resolver.ensure_item_capacity(
            self.environment
                .len()
                .saturating_add(resolver.active_environment_count(bindings, &[])),
        )?;
        resolver.claim_existing_environment(&self.environment)?;
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
        TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone(), self.limits).resolve(value)
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
            StepSpec::Run { .. } | StepSpec::RunBuilt { .. } | StepSpec::Shell { .. } => return Ok(None),
            StepSpec::CMakeConfigure { .. } => &self.policy.builders.cmake.setup,
            StepSpec::CMakeBuild => &self.policy.builders.cmake.build,
            StepSpec::CMakeInstall => &self.policy.builders.cmake.install,
            StepSpec::CMakeTest => &self.policy.builders.cmake.check,
            StepSpec::MesonSetup { .. } => &self.policy.builders.meson.setup,
            StepSpec::MesonBuild => &self.policy.builders.meson.build,
            StepSpec::MesonInstall => &self.policy.builders.meson.install,
            StepSpec::MesonTest => &self.policy.builders.meson.check,
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
            | StepSpec::RunBuilt { .. }
            | StepSpec::Shell { .. }
            | StepSpec::CMakeBuild
            | StepSpec::CMakeInstall
            | StepSpec::CMakeTest
            | StepSpec::MesonBuild
            | StepSpec::MesonInstall
            | StepSpec::MesonTest
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
        command.validate_with_limits(self.limits)?;
        let resolver = TextResolver::new(&self.policy, &self.target, &self.inputs, overlay.clone(), self.limits);
        let active_environment = resolver.active_environment_count(&command.environment, &[]);
        resolver.ensure_item_capacity(
            1usize
                .saturating_add(self.environment.len())
                .saturating_add(active_environment)
                .saturating_add(command.args.len())
                .saturating_add(1),
        )?;
        resolver.claim_existing_environment(&self.environment)?;
        resolver.claim_static_output(command.program.path.len())?;
        let mut environment = self.environment.clone();
        environment.extend(resolver.resolve_environment(&command.environment, &[])?);

        let mut args = Vec::new();
        args.try_reserve_exact(command.args.len())
            .map_err(|_| ContextError::TextCapacity {
                requested: command.args.len(),
            })?;
        for argument in &command.args {
            args.push(resolver.resolve(argument)?);
        }

        Ok(Run {
            program: freeze_policy_program(&command.program),
            args,
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

/// Render a structurally tokenized command for build-system environment
/// variables such as `CC` and `CPP`.
///
/// The executable path remains absolute and every argument is quoted as one
/// POSIX-shell word. A compiler-cache wrapper is an explicit first executable,
/// never a `PATH` mutation or basename lookup.
fn render_build_command(command: &BuildCommandSpec, wrapper: Option<&BuildProgramSpec>) -> String {
    let mut rendered = String::new();
    if let Some(wrapper) = wrapper {
        push_shell_word(&mut rendered, &wrapper.path);
    }
    push_shell_word(&mut rendered, &command.program.path);
    for argument in &command.args {
        push_shell_word(&mut rendered, argument);
    }
    rendered
}

fn push_shell_word(rendered: &mut String, word: &str) {
    if !rendered.is_empty() {
        rendered.push(' ');
    }
    if !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._+,-:=@%".contains(&byte))
    {
        rendered.push_str(word);
        return;
    }

    rendered.push('\'');
    for character in word.chars() {
        if character == '\'' {
            rendered.push_str("'\"'\"'");
        } else {
            rendered.push(character);
        }
    }
    rendered.push('\'');
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

include!("../build_context/resolution_state.rs");
include!("../build_context/text_resolution.rs");
include!("../build_context/checked_append.rs");

#[cfg(test)]
#[path = "../build_context/tests.rs"]
mod tests;
