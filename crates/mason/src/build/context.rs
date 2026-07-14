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
            StepSpec::Run { .. } | StepSpec::Shell { .. } => return Ok(None),
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

#[derive(Default)]
struct ResolutionBudget {
    items: Cell<usize>,
    text_nodes: Cell<usize>,
    output_bytes: Cell<usize>,
    steps: Cell<usize>,
}

impl ResolutionBudget {
    fn ensure_items(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let count = self.items.get().saturating_add(additional);
        if count > limit {
            Err(ContextError::ResolvedItemLimit { count, limit })
        } else {
            Ok(())
        }
    }

    fn claim_items(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        self.ensure_items(additional, limit)?;
        self.items.set(self.items.get().saturating_add(additional));
        Ok(())
    }

    fn ensure_text_nodes(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let nodes = self.text_nodes.get().saturating_add(additional);
        if nodes > limit {
            Err(ContextError::TotalTextNodeLimit { nodes, limit })
        } else {
            Ok(())
        }
    }

    fn claim_text_node(&self, limit: usize) -> Result<(), ContextError> {
        self.ensure_text_nodes(1, limit)?;
        self.text_nodes.set(self.text_nodes.get().saturating_add(1));
        Ok(())
    }

    fn claim_output_bytes(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let bytes = self.output_bytes.get().saturating_add(additional);
        if bytes > limit {
            return Err(ContextError::TotalResolvedTextBytesLimit { bytes, limit });
        }
        self.output_bytes.set(bytes);
        Ok(())
    }

    fn ensure_steps(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let steps = self.steps.get().saturating_add(additional);
        if steps > limit {
            Err(ContextError::ResolverStepLimit { steps, limit })
        } else {
            Ok(())
        }
    }

    fn claim_step(&self, limit: usize) -> Result<(), ContextError> {
        self.ensure_steps(1, limit)?;
        self.steps.set(self.steps.get().saturating_add(1));
        Ok(())
    }
}

struct TextResolver<'a> {
    policy: &'a BuildPolicySpec,
    target: &'a TargetPolicySpec,
    inputs: &'a TypedContextInputs,
    overlay: TextContextOverlay,
    limits: BuildPolicyValidationLimits,
    budget: ResolutionBudget,
}

enum ResolveAction<'a> {
    Text {
        value: &'a TextSpec,
        depth: usize,
        output: usize,
    },
    Context {
        value: ContextValue,
        depth: usize,
        output: usize,
    },
    LeaveContext(ContextValue),
    Append {
        value: &'a str,
        output: usize,
    },
    AppendOwned {
        value: String,
        output: usize,
    },
    Flags {
        selected: &'a [TextSpec],
        mold: &'a [TextSpec],
        index: usize,
        output: usize,
        emitted: bool,
        depth: usize,
    },
    FinishFlag {
        selected: &'a [TextSpec],
        mold: &'a [TextSpec],
        next_index: usize,
        output: usize,
        child: usize,
        emitted: bool,
        depth: usize,
    },
}

enum ContextExpansion<'a> {
    Text(&'a TextSpec),
    Flags(&'a [TextSpec], &'a [TextSpec]),
    Borrowed(&'a str),
    Owned(String),
}

impl<'a> TextResolver<'a> {
    fn new(
        policy: &'a BuildPolicySpec,
        target: &'a TargetPolicySpec,
        inputs: &'a TypedContextInputs,
        overlay: TextContextOverlay,
        limits: BuildPolicyValidationLimits,
    ) -> Self {
        Self {
            policy,
            target,
            inputs,
            overlay,
            limits,
            budget: ResolutionBudget::default(),
        }
    }

    fn ensure_item_capacity(&self, additional: usize) -> Result<(), ContextError> {
        self.budget.ensure_items(additional, self.limits.max_resolved_items)
    }

    fn claim_static_output(&self, bytes: usize) -> Result<(), ContextError> {
        self.budget.claim_items(1, self.limits.max_resolved_items)?;
        self.budget
            .claim_output_bytes(bytes, self.limits.max_total_resolved_text_bytes)
    }

    fn claim_existing_environment(&self, environment: &BTreeMap<String, String>) -> Result<(), ContextError> {
        self.budget
            .claim_items(environment.len(), self.limits.max_resolved_items)?;
        let bytes = environment.iter().fold(0usize, |total, (name, value)| {
            total.saturating_add(name.len()).saturating_add(value.len())
        });
        self.budget
            .claim_output_bytes(bytes, self.limits.max_total_resolved_text_bytes)
    }

    fn active_environment_count(&self, first: &[EnvironmentBindingSpec], second: &[EnvironmentBindingSpec]) -> usize {
        first
            .iter()
            .chain(second)
            .filter(|binding| self.binding_is_active(binding))
            .count()
    }

    fn binding_is_active(&self, binding: &EnvironmentBindingSpec) -> bool {
        match binding.condition {
            EnvironmentCondition::Always => true,
            EnvironmentCondition::CompilerCacheEnabled => self.inputs.compiler_cache_enabled,
            EnvironmentCondition::CompilerCacheDisabled => !self.inputs.compiler_cache_enabled,
        }
    }

    fn push_action<'b>(
        &self,
        actions: &mut Vec<ResolveAction<'b>>,
        action: ResolveAction<'b>,
    ) -> Result<(), ContextError> {
        let requested = actions.len().saturating_add(1);
        self.budget.ensure_steps(requested, self.limits.max_resolver_steps)?;
        actions
            .try_reserve(1)
            .map_err(|_| ContextError::TextCapacity { requested })?;
        actions.push(action);
        Ok(())
    }

    fn resolve(&self, value: &TextSpec) -> Result<String, ContextError> {
        self.budget.claim_items(1, self.limits.max_resolved_items)?;
        self.budget
            .ensure_text_nodes(1, self.limits.max_total_resolved_text_nodes)?;
        self.budget.ensure_steps(1, self.limits.max_resolver_steps)?;
        let mut actions = Vec::new();
        actions
            .try_reserve(1)
            .map_err(|_| ContextError::TextCapacity { requested: 1 })?;
        actions.push(ResolveAction::Text {
            value,
            depth: 1,
            output: 0,
        });
        let mut outputs = vec![Some(String::new())];
        let mut resolving = Vec::new();
        let mut pending_text_nodes = 1usize;
        let mut text_nodes = 0usize;
        let mut literal_bytes = 0usize;

        while let Some(action) = actions.pop() {
            self.budget.claim_step(self.limits.max_resolver_steps)?;

            match action {
                ResolveAction::Text { value, depth, output } => {
                    pending_text_nodes -= 1;
                    text_nodes = text_nodes.saturating_add(1);
                    if text_nodes > self.limits.max_text_nodes {
                        return Err(ContextError::TextNodeLimit {
                            nodes: text_nodes,
                            limit: self.limits.max_text_nodes,
                        });
                    }
                    self.budget.claim_text_node(self.limits.max_total_resolved_text_nodes)?;
                    if depth > self.limits.max_text_depth {
                        return Err(ContextError::TextDepthLimit {
                            depth,
                            limit: self.limits.max_text_depth,
                        });
                    }
                    match value {
                        TextSpec::Literal(value) => {
                            let bytes = value.len();
                            if bytes > self.limits.max_text_literal_bytes {
                                return Err(ContextError::TextLiteralBytesLimit {
                                    bytes,
                                    limit: self.limits.max_text_literal_bytes,
                                });
                            }
                            literal_bytes = literal_bytes.saturating_add(bytes);
                            if literal_bytes > self.limits.max_text_total_literal_bytes {
                                return Err(ContextError::TextTotalLiteralBytesLimit {
                                    bytes: literal_bytes,
                                    limit: self.limits.max_text_total_literal_bytes,
                                });
                            }
                            append_checked(
                                &mut outputs,
                                output,
                                value,
                                self.limits.max_resolved_text_bytes,
                                &self.budget,
                                self.limits.max_total_resolved_text_bytes,
                            )?;
                        }
                        TextSpec::Context(value) => {
                            self.push_action(
                                &mut actions,
                                ResolveAction::Context {
                                    value: *value,
                                    depth,
                                    output,
                                },
                            )?;
                        }
                        TextSpec::Concat(parts) => {
                            let projected = text_nodes
                                .saturating_add(pending_text_nodes)
                                .saturating_add(parts.len());
                            if projected > self.limits.max_text_nodes {
                                return Err(ContextError::TextNodeLimit {
                                    nodes: projected,
                                    limit: self.limits.max_text_nodes,
                                });
                            }
                            self.budget.ensure_text_nodes(
                                pending_text_nodes.saturating_add(parts.len()),
                                self.limits.max_total_resolved_text_nodes,
                            )?;
                            self.budget.ensure_steps(
                                actions.len().saturating_add(parts.len()),
                                self.limits.max_resolver_steps,
                            )?;
                            actions
                                .try_reserve(parts.len())
                                .map_err(|_| ContextError::TextCapacity { requested: projected })?;
                            pending_text_nodes = pending_text_nodes.saturating_add(parts.len());
                            let child_depth = depth.saturating_add(1);
                            for part in parts.iter().rev() {
                                actions.push(ResolveAction::Text {
                                    value: part,
                                    depth: child_depth,
                                    output,
                                });
                            }
                        }
                    }
                }
                ResolveAction::Context { value, depth, output } => {
                    if let Some(start) = resolving.iter().position(|candidate| *candidate == value) {
                        let mut chain = resolving[start..].to_vec();
                        chain.push(value);
                        return Err(ContextError::RecursiveContext { chain });
                    }
                    resolving.push(value);
                    self.push_action(&mut actions, ResolveAction::LeaveContext(value))?;
                    match self.context_expansion(value)? {
                        ContextExpansion::Text(value) => {
                            let projected = text_nodes.saturating_add(pending_text_nodes).saturating_add(1);
                            if projected > self.limits.max_text_nodes {
                                return Err(ContextError::TextNodeLimit {
                                    nodes: projected,
                                    limit: self.limits.max_text_nodes,
                                });
                            }
                            self.budget.ensure_text_nodes(
                                pending_text_nodes.saturating_add(1),
                                self.limits.max_total_resolved_text_nodes,
                            )?;
                            pending_text_nodes += 1;
                            self.push_action(
                                &mut actions,
                                ResolveAction::Text {
                                    value,
                                    depth: depth.saturating_add(1),
                                    output,
                                },
                            )?;
                        }
                        ContextExpansion::Flags(selected, mold) => {
                            let count = selected.len().saturating_add(mold.len());
                            if count > self.limits.max_compiler_flags {
                                return Err(ContextError::FlagCollectionLimit {
                                    count,
                                    limit: self.limits.max_compiler_flags,
                                });
                            }
                            self.push_action(
                                &mut actions,
                                ResolveAction::Flags {
                                    selected,
                                    mold,
                                    index: 0,
                                    output,
                                    emitted: false,
                                    depth: depth.saturating_add(1),
                                },
                            )?;
                        }
                        ContextExpansion::Borrowed(value) => {
                            self.push_action(&mut actions, ResolveAction::Append { value, output })?;
                        }
                        ContextExpansion::Owned(value) => {
                            self.push_action(&mut actions, ResolveAction::AppendOwned { value, output })?;
                        }
                    }
                }
                ResolveAction::LeaveContext(value) => {
                    debug_assert_eq!(resolving.pop(), Some(value));
                }
                ResolveAction::Append { value, output } => {
                    append_checked(
                        &mut outputs,
                        output,
                        value,
                        self.limits.max_resolved_text_bytes,
                        &self.budget,
                        self.limits.max_total_resolved_text_bytes,
                    )?;
                }
                ResolveAction::AppendOwned { value, output } => {
                    append_checked(
                        &mut outputs,
                        output,
                        &value,
                        self.limits.max_resolved_text_bytes,
                        &self.budget,
                        self.limits.max_total_resolved_text_bytes,
                    )?;
                }
                ResolveAction::Flags {
                    selected,
                    mold,
                    index,
                    output,
                    emitted,
                    depth,
                } => {
                    let count = selected.len() + mold.len();
                    if index == count {
                        continue;
                    }
                    let value = if index < selected.len() {
                        &selected[index]
                    } else {
                        &mold[index - selected.len()]
                    };
                    let projected = text_nodes.saturating_add(pending_text_nodes).saturating_add(1);
                    if projected > self.limits.max_text_nodes {
                        return Err(ContextError::TextNodeLimit {
                            nodes: projected,
                            limit: self.limits.max_text_nodes,
                        });
                    }
                    self.budget.ensure_text_nodes(
                        pending_text_nodes.saturating_add(1),
                        self.limits.max_total_resolved_text_nodes,
                    )?;
                    let requested = outputs.len().saturating_add(1);
                    outputs
                        .try_reserve(1)
                        .map_err(|_| ContextError::TextCapacity { requested })?;
                    let child = outputs.len();
                    outputs.push(Some(String::new()));
                    pending_text_nodes += 1;
                    self.push_action(
                        &mut actions,
                        ResolveAction::FinishFlag {
                            selected,
                            mold,
                            next_index: index + 1,
                            output,
                            child,
                            emitted,
                            depth,
                        },
                    )?;
                    self.push_action(
                        &mut actions,
                        ResolveAction::Text {
                            value,
                            depth,
                            output: child,
                        },
                    )?;
                }
                ResolveAction::FinishFlag {
                    selected,
                    mold,
                    next_index,
                    output,
                    child,
                    emitted,
                    depth,
                } => {
                    let value = outputs[child].take().expect("flag output buffer is live");
                    let nonempty = !value.is_empty();
                    if nonempty {
                        append_joined_checked(
                            &mut outputs,
                            output,
                            &value,
                            emitted,
                            self.limits.max_resolved_text_bytes,
                            &self.budget,
                            self.limits.max_total_resolved_text_bytes,
                        )?;
                    }
                    self.push_action(
                        &mut actions,
                        ResolveAction::Flags {
                            selected,
                            mold,
                            index: next_index,
                            output,
                            emitted: emitted || nonempty,
                            depth,
                        },
                    )?;
                }
            }
        }

        Ok(outputs[0].take().expect("root output buffer is live"))
    }

    fn context_expansion(&self, value: ContextValue) -> Result<ContextExpansion<'_>, ContextError> {
        let input = self.inputs;
        let cache = &self.policy.build_root.compiler_cache;
        let tools = self.selected_tools();
        let layout = &self.policy.layout;
        let mold = self.mold_flags();

        Ok(match value {
            ContextValue::PackageName => ContextExpansion::Borrowed(&input.package_name),
            ContextValue::PackageVersion => ContextExpansion::Borrowed(&input.package_version),
            ContextValue::PackageRelease => ContextExpansion::Owned(input.package_release.to_string()),
            ContextValue::SourceDir => ContextExpansion::Borrowed(&input.source_dir),
            ContextValue::InstallRoot => ContextExpansion::Borrowed(&input.install_root),
            ContextValue::BuildRoot => ContextExpansion::Borrowed(&input.build_root),
            ContextValue::WorkDir => ContextExpansion::Borrowed(&input.work_dir),
            ContextValue::BuilderDir => ContextExpansion::Borrowed(&self.policy.build_subdir),
            ContextValue::PgoDir => ContextExpansion::Borrowed(&input.pgo_dir),
            ContextValue::Jobs => ContextExpansion::Owned(input.jobs.to_string()),
            ContextValue::SourceDateEpoch => ContextExpansion::Owned(input.source_date_epoch.to_string()),
            ContextValue::PgoStage => ContextExpansion::Borrowed(input.pgo_stage.as_environment_value()),
            ContextValue::TargetTriple => ContextExpansion::Borrowed(&self.target.target_triple),
            ContextValue::BuildPlatform => ContextExpansion::Borrowed(&self.target.build_triple),
            ContextValue::HostPlatform => ContextExpansion::Borrowed(&self.target.host_triple),
            ContextValue::LibSuffix => ContextExpansion::Borrowed(&self.target.lib_suffix),
            ContextValue::Prefix => ContextExpansion::Text(&layout.prefix),
            ContextValue::BinDir => ContextExpansion::Text(&layout.bindir),
            ContextValue::SbinDir => ContextExpansion::Text(&layout.sbindir),
            ContextValue::IncludeDir => ContextExpansion::Text(&layout.includedir),
            ContextValue::LibDir => ContextExpansion::Text(&layout.libdir),
            ContextValue::LibexecDir => ContextExpansion::Text(&layout.libexecdir),
            ContextValue::DataDir => ContextExpansion::Text(&layout.datadir),
            ContextValue::VendorDir => ContextExpansion::Text(&layout.vendordir),
            ContextValue::DocDir => ContextExpansion::Text(&layout.docdir),
            ContextValue::InfoDir => ContextExpansion::Text(&layout.infodir),
            ContextValue::LocaleDir => ContextExpansion::Text(&layout.localedir),
            ContextValue::ManDir => ContextExpansion::Text(&layout.mandir),
            ContextValue::SysconfDir => ContextExpansion::Text(&layout.sysconfdir),
            ContextValue::LocalStateDir => ContextExpansion::Text(&layout.localstatedir),
            ContextValue::SharedStateDir => ContextExpansion::Text(&layout.sharedstatedir),
            ContextValue::RunStateDir => ContextExpansion::Text(&layout.runstatedir),
            ContextValue::CFlags => ContextExpansion::Flags(&input.flags.c, &mold.c),
            ContextValue::CxxFlags => ContextExpansion::Flags(&input.flags.cxx, &mold.cxx),
            ContextValue::FFlags => ContextExpansion::Flags(&input.flags.f, &mold.f),
            ContextValue::DFlags => ContextExpansion::Flags(&input.flags.d, &mold.d),
            ContextValue::RustFlags => ContextExpansion::Flags(&input.flags.rust, &mold.rust),
            ContextValue::ValaFlags => ContextExpansion::Flags(&input.flags.vala, &mold.vala),
            ContextValue::GoFlags => ContextExpansion::Flags(&input.flags.go, &mold.go),
            ContextValue::LdFlags => ContextExpansion::Flags(&input.flags.ld, &mold.ld),
            ContextValue::Cc => ContextExpansion::Owned(self.render_compiler_command(&tools.cc)),
            ContextValue::Cxx => ContextExpansion::Owned(self.render_compiler_command(&tools.cxx)),
            ContextValue::Objc => ContextExpansion::Owned(self.render_compiler_command(&tools.objc)),
            ContextValue::Objcxx => ContextExpansion::Owned(self.render_compiler_command(&tools.objcxx)),
            ContextValue::Cpp => ContextExpansion::Owned(self.render_compiler_command(&tools.cpp)),
            ContextValue::Objcpp => ContextExpansion::Owned(self.render_compiler_command(&tools.objcpp)),
            ContextValue::Objcxxcpp => ContextExpansion::Owned(self.render_compiler_command(&tools.objcxxcpp)),
            ContextValue::Ar => ContextExpansion::Owned(render_build_command(&tools.ar, None)),
            ContextValue::Ld if input.mold_enabled => {
                ContextExpansion::Owned(render_build_command(&self.policy.build_root.mold.linker, None))
            }
            ContextValue::Ld => ContextExpansion::Owned(render_build_command(&tools.ld, None)),
            ContextValue::Objcopy => ContextExpansion::Owned(render_build_command(&tools.objcopy, None)),
            ContextValue::Nm => ContextExpansion::Owned(render_build_command(&tools.nm, None)),
            ContextValue::Ranlib => ContextExpansion::Owned(render_build_command(&tools.ranlib, None)),
            ContextValue::Strip => ContextExpansion::Owned(render_build_command(&tools.strip, None)),
            ContextValue::CcacheDir => ContextExpansion::Borrowed(&cache.ccache_dir),
            ContextValue::SccacheDir => ContextExpansion::Borrowed(&cache.sccache_dir),
            ContextValue::GoCacheDir => ContextExpansion::Borrowed(&cache.go_cache_dir),
            ContextValue::GoModCacheDir => ContextExpansion::Borrowed(&cache.go_mod_cache_dir),
            ContextValue::CargoCacheDir => ContextExpansion::Borrowed(&cache.cargo_cache_dir),
            ContextValue::ZigCacheDir => ContextExpansion::Borrowed(&cache.zig_cache_dir),
            ContextValue::RustcWrapper => ContextExpansion::Borrowed(&cache.sccache.path),
            ContextValue::SourcePath => ContextExpansion::Borrowed(
                self.overlay
                    .source_path
                    .as_deref()
                    .ok_or(ContextError::MissingContext { value })?,
            ),
            ContextValue::SourceDestination => ContextExpansion::Borrowed(
                self.overlay
                    .source_destination
                    .as_deref()
                    .ok_or(ContextError::MissingContext { value })?,
            ),
        })
    }

    fn selected_tools(&self) -> &CompilerToolsSpec {
        match self.inputs.toolchain {
            ToolchainSpec::Llvm => &self.policy.toolchains.llvm,
            ToolchainSpec::Gnu => &self.policy.toolchains.gnu,
        }
    }

    fn render_compiler_command(&self, command: &BuildCommandSpec) -> String {
        let wrapper = self
            .inputs
            .compiler_cache_enabled
            .then_some(&self.policy.build_root.compiler_cache.ccache);
        render_build_command(command, wrapper)
    }

    fn mold_flags(&self) -> &CompilerFlagsSpec {
        if self.inputs.mold_enabled {
            &self.policy.build_root.mold.flags
        } else {
            static EMPTY: std::sync::LazyLock<CompilerFlagsSpec> = std::sync::LazyLock::new(CompilerFlagsSpec::default);
            &EMPTY
        }
    }

    fn resolve_environment(
        &self,
        first: &[EnvironmentBindingSpec],
        second: &[EnvironmentBindingSpec],
    ) -> Result<BTreeMap<String, String>, ContextError> {
        let count = self.active_environment_count(first, second);
        self.ensure_item_capacity(count)?;
        let mut environment = BTreeMap::new();
        for binding in first
            .iter()
            .chain(second)
            .filter(|binding| self.binding_is_active(binding))
        {
            self.budget
                .claim_output_bytes(binding.name.len(), self.limits.max_total_resolved_text_bytes)?;
            environment.insert(binding.name.clone(), self.resolve(&binding.value)?);
        }
        Ok(environment)
    }

    fn resolve_layout(&self) -> Result<InstallLayout, ContextError> {
        self.ensure_item_capacity(InstallLayout::RESOLVED_ITEMS)?;
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
        self.ensure_item_capacity(ResolvedCompilerTools::RESOLVED_ITEMS)?;
        let value = |context| self.resolve(&TextSpec::Context(context));
        Ok(ResolvedCompilerTools {
            cc: value(ContextValue::Cc)?,
            cxx: value(ContextValue::Cxx)?,
            objc: value(ContextValue::Objc)?,
            objcxx: value(ContextValue::Objcxx)?,
            cpp: value(ContextValue::Cpp)?,
            objcpp: value(ContextValue::Objcpp)?,
            objcxxcpp: value(ContextValue::Objcxxcpp)?,
            ar: value(ContextValue::Ar)?,
            ld: value(ContextValue::Ld)?,
            objcopy: value(ContextValue::Objcopy)?,
            nm: value(ContextValue::Nm)?,
            ranlib: value(ContextValue::Ranlib)?,
            strip: value(ContextValue::Strip)?,
        })
    }

    fn resolve_flags_record(&self) -> Result<ResolvedCompilerFlags, ContextError> {
        self.ensure_item_capacity(ResolvedCompilerFlags::RESOLVED_ITEMS)?;
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

fn append_checked(
    outputs: &mut [Option<String>],
    output: usize,
    value: &str,
    limit: usize,
    budget: &ResolutionBudget,
    total_limit: usize,
) -> Result<(), ContextError> {
    let buffer = outputs[output].as_mut().expect("output buffer is live");
    let bytes = buffer.len().saturating_add(value.len());
    if bytes > limit {
        return Err(ContextError::ResolvedTextBytesLimit { bytes, limit });
    }
    budget.claim_output_bytes(value.len(), total_limit)?;
    buffer
        .try_reserve(value.len())
        .map_err(|_| ContextError::TextCapacity { requested: bytes })?;
    buffer.push_str(value);
    Ok(())
}

fn append_joined_checked(
    outputs: &mut [Option<String>],
    output: usize,
    value: &str,
    separator: bool,
    limit: usize,
    budget: &ResolutionBudget,
    total_limit: usize,
) -> Result<(), ContextError> {
    let buffer = outputs[output].as_mut().expect("output buffer is live");
    let added = value.len().saturating_add(usize::from(separator));
    let bytes = buffer.len().saturating_add(added);
    if bytes > limit {
        return Err(ContextError::ResolvedTextBytesLimit { bytes, limit });
    }
    budget.claim_output_bytes(added, total_limit)?;
    buffer
        .try_reserve(added)
        .map_err(|_| ContextError::TextCapacity { requested: bytes })?;
    if separator {
        buffer.push(' ');
    }
    buffer.push_str(value);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error(transparent)]
    PolicyValidation(#[from] BuildPolicyConversionError),
    #[error("selected target is not the exact member of the validated policy")]
    TargetNotInPolicy,
    #[error("policy text requires missing finite context value {value:?}")]
    MissingContext { value: ContextValue },
    #[error("policy text contains a recursive context reference: {chain:?}")]
    RecursiveContext { chain: Vec<ContextValue> },
    #[error("resolved policy text has at least {nodes} nodes, limit is {limit}")]
    TextNodeLimit { nodes: usize, limit: usize },
    #[error("resolved policy text depth is {depth}, limit is {limit}")]
    TextDepthLimit { depth: usize, limit: usize },
    #[error("resolved policy text literal has {bytes} bytes, limit is {limit}")]
    TextLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("resolved policy text literals contain {bytes} bytes in total, limit is {limit}")]
    TextTotalLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("resolved policy text has {bytes} output bytes, limit is {limit}")]
    ResolvedTextBytesLimit { bytes: usize, limit: usize },
    #[error("resolved operation contains {count} items, limit is {limit}")]
    ResolvedItemLimit { count: usize, limit: usize },
    #[error("resolved operation contains {nodes} policy-text nodes, limit is {limit}")]
    TotalTextNodeLimit { nodes: usize, limit: usize },
    #[error("resolved operation appended {bytes} output bytes, limit is {limit}")]
    TotalResolvedTextBytesLimit { bytes: usize, limit: usize },
    #[error("resolved compiler flags contain {count} entries, limit is {limit}")]
    FlagCollectionLimit { count: usize, limit: usize },
    #[error("policy text resolver used {steps} steps, limit is {limit}")]
    ResolverStepLimit { steps: usize, limit: usize },
    #[error("unable to reserve bounded policy-text capacity for {requested} items or bytes")]
    TextCapacity { requested: usize },
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
        assert_eq!(context.tools.cc, "/usr/bin/clang");
        assert_eq!(context.tools.objcpp, "/usr/bin/clang -E -");
        assert_eq!(context.tools.ld, "/usr/bin/ld.mold");
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
        assert_eq!(cached.environment["PATH"], "/usr/bin:/bin");
        assert_eq!(cached.environment["CCACHE_DIR"], "/mason/ccache");
        assert_eq!(cached.environment["RUSTC_WRAPPER"], "/usr/bin/sccache");
        assert_eq!(cached.tools.cc, "/usr/bin/ccache /usr/bin/clang");
        assert_eq!(cached.tools.objcpp, "/usr/bin/ccache /usr/bin/clang -E -");
        assert_eq!(cached.tools.ld, "/usr/bin/ld.lld");
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
    fn compiler_command_tokens_are_shell_quoted_without_path_lookup() {
        let command = BuildCommandSpec {
            program: BuildProgramSpec {
                path: "/usr/bin/clang".to_owned(),
                requirement: BuildToolSpec::Binary("clang".to_owned()),
            },
            args: vec![
                "-E".to_owned(),
                "two words".to_owned(),
                "has'quote".to_owned(),
                "$HOME".to_owned(),
                String::new(),
            ],
        };
        let wrapper = BuildProgramSpec {
            path: "/usr/bin/ccache".to_owned(),
            requirement: BuildToolSpec::Binary("ccache".to_owned()),
        };

        assert_eq!(
            render_build_command(&command, Some(&wrapper)),
            "/usr/bin/ccache /usr/bin/clang -E 'two words' 'has'\"'\"'quote' '$HOME' ''"
        );
    }

    #[test]
    fn target_environment_overrides_global_tool_values() {
        let context = fixture_context("emul32/x86_64", false, false);

        assert_eq!(context.environment["CC"], "/usr/bin/clang -m32");
        assert_eq!(context.environment["CXX"], "/usr/bin/clang++ -m32");
        assert_eq!(context.environment["CPP"], "/usr/bin/clang-cpp -m32");
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
        policy.spec.builders.cmake.build.program.path = "/usr/bin/policy-cmake".to_owned();
        policy.spec.builders.cmake.build.program.requirement = BuildToolSpec::Binary("policy-cmake".to_owned());
        policy.spec.builders.cmake.build.args = vec![
            TextSpec::Literal("--policy-build".to_owned()),
            TextSpec::Context(ContextValue::BuilderDir),
        ];
        let inputs = fixture_context("x86_64", false, false).inputs;
        let target = policy.target("x86_64").unwrap();
        let context = BuildContext::resolve(&policy.spec, target, inputs).unwrap();

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
        };
        let Run {
            program,
            args,
            working_dir,
            ..
        } = context
            .resolve_command(&context.policy.sources.git.copy, &overlay)
            .unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(program.path, "/usr/bin/cp");
        assert_eq!(working_dir, "/mason/build/x86_64");
        assert_eq!(
            args,
            [
                "-Ra",
                "--no-preserve=ownership",
                "/mason/sourcedir/source archive.tar.xz/.",
                "source tree",
            ]
        );
    }

    #[test]
    fn recursive_policy_context_is_rejected_without_interpolation() {
        let mut policy = BuildPolicy::repository_for_tests();
        policy.spec.layout.prefix = TextSpec::Context(ContextValue::Prefix);
        let mut inputs = fixture_context("x86_64", false, false).inputs;
        inputs.flags = CompilerFlagsSpec::default();
        let target = policy.target("x86_64").unwrap();

        assert_eq!(
            BuildContext::resolve(&policy.spec, target, inputs),
            Err(ContextError::RecursiveContext {
                chain: vec![ContextValue::Prefix, ContextValue::Prefix],
            })
        );
    }

    #[test]
    fn detached_target_is_rejected_before_resolution() {
        let policy = BuildPolicy::repository_for_tests();
        let detached = policy.target("x86_64").unwrap().clone();
        let inputs = fixture_context("x86_64", false, false).inputs;

        assert_eq!(
            BuildContext::resolve(&policy.spec, &detached, inputs),
            Err(ContextError::TargetNotInPolicy)
        );
    }

    #[test]
    fn resolver_item_budget_is_shared_across_layout_tools_and_flags() {
        let context = fixture_context("x86_64", false, false);
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_resolved_items = InstallLayout::RESOLVED_ITEMS
            + ResolvedCompilerTools::RESOLVED_ITEMS
            + ResolvedCompilerFlags::RESOLVED_ITEMS;
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );

        resolver.resolve_layout().unwrap();
        resolver.resolve_tools().unwrap();
        resolver.resolve_flags_record().unwrap();
        assert_eq!(
            resolver.resolve(&TextSpec::Literal("x".to_owned())),
            Err(ContextError::ResolvedItemLimit {
                count: limits.max_resolved_items + 1,
                limit: limits.max_resolved_items,
            })
        );
    }

    #[test]
    fn resolver_aggregate_bytes_nodes_and_steps_accept_n_and_reject_n_plus_one() {
        let context = fixture_context("x86_64", false, false);

        let mut byte_limits = BuildPolicyValidationLimits::default();
        byte_limits.max_total_resolved_text_bytes = 3;
        let bytes = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            byte_limits,
        );
        assert_eq!(bytes.resolve(&TextSpec::Literal("ab".to_owned())), Ok("ab".to_owned()));
        assert_eq!(bytes.resolve(&TextSpec::Literal("c".to_owned())), Ok("c".to_owned()));
        assert_eq!(
            bytes.resolve(&TextSpec::Literal("d".to_owned())),
            Err(ContextError::TotalResolvedTextBytesLimit { bytes: 4, limit: 3 })
        );

        let mut node_limits = BuildPolicyValidationLimits::default();
        node_limits.max_total_resolved_text_nodes = 2;
        let nodes = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            node_limits,
        );
        assert_eq!(nodes.resolve(&TextSpec::Literal("a".to_owned())), Ok("a".to_owned()));
        assert_eq!(nodes.resolve(&TextSpec::Literal("b".to_owned())), Ok("b".to_owned()));
        assert_eq!(
            nodes.resolve(&TextSpec::Literal("c".to_owned())),
            Err(ContextError::TotalTextNodeLimit { nodes: 3, limit: 2 })
        );

        let mut step_limits = BuildPolicyValidationLimits::default();
        step_limits.max_resolver_steps = 2;
        let steps = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            step_limits,
        );
        assert_eq!(steps.resolve(&TextSpec::Literal("a".to_owned())), Ok("a".to_owned()));
        assert_eq!(steps.resolve(&TextSpec::Literal("b".to_owned())), Ok("b".to_owned()));
        assert_eq!(
            steps.resolve(&TextSpec::Literal("c".to_owned())),
            Err(ContextError::ResolverStepLimit { steps: 3, limit: 2 })
        );
    }

    #[test]
    fn resolver_step_budget_rejects_a_wide_action_stack_before_reserving_it() {
        let context = fixture_context("x86_64", false, false);
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_nodes = 100_001;
        limits.max_resolver_steps = 1;
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );
        let value = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 100_000]);

        assert_eq!(
            resolver.resolve(&value),
            Err(ContextError::ResolverStepLimit {
                steps: 100_001,
                limit: 1,
            })
        );
    }

    #[test]
    fn command_preflights_aggregate_items_before_building_argv() {
        let mut context = fixture_context("x86_64", false, false);
        context.environment.clear();
        let mut command = context.policy.sources.git.create_directory.clone();
        command.args = vec![TextSpec::Literal("a".to_owned()), TextSpec::Literal("b".to_owned())];
        command.environment.clear();
        command.working_dir = TextSpec::Literal("work".to_owned());

        context.limits.max_resolved_items = 4;
        context
            .resolve_command(&command, &TextContextOverlay::default())
            .unwrap();
        context.limits.max_resolved_items = 3;
        assert_eq!(
            context.resolve_command(&command, &TextContextOverlay::default()),
            Err(ContextError::ResolvedItemLimit { count: 4, limit: 3 })
        );
    }

    #[test]
    fn fragment_boundaries_revalidate_commands_and_environment() {
        let mut context = fixture_context("x86_64", false, false);
        let mut command = context.policy.sources.git.create_directory.clone();
        let allowed_arguments = command.args.len();
        command.args.push(TextSpec::Literal("extra".to_owned()));
        context.limits.max_builder_arguments = allowed_arguments;
        assert!(matches!(
            context.resolve_command(&command, &TextContextOverlay::default()),
            Err(ContextError::PolicyValidation(BuildPolicyConversionError::CollectionLimit {
                field,
                count,
                limit,
            })) if field == "command.args" && count == allowed_arguments + 1 && limit == allowed_arguments
        ));

        context.limits = BuildPolicyValidationLimits::default();
        context.limits.max_environment_bindings = 1;
        let bindings = [
            EnvironmentBindingSpec {
                name: "ONE".to_owned(),
                value: TextSpec::Literal("1".to_owned()),
                condition: EnvironmentCondition::Always,
            },
            EnvironmentBindingSpec {
                name: "TWO".to_owned(),
                value: TextSpec::Literal("2".to_owned()),
                condition: EnvironmentCondition::Always,
            },
        ];
        assert!(matches!(
            context.extend_environment(&bindings),
            Err(ContextError::PolicyValidation(BuildPolicyConversionError::CollectionLimit {
                field,
                count: 2,
                limit: 1,
            })) if field == "environment"
        ));
    }

    #[test]
    fn repeated_environment_extension_is_bounded_by_the_retained_final_state() {
        let mut context = fixture_context("x86_64", false, false);
        context.environment.clear();
        context.limits.max_resolved_items = 2;
        let binding = |name: &str| {
            [EnvironmentBindingSpec {
                name: name.to_owned(),
                value: TextSpec::Literal("x".to_owned()),
                condition: EnvironmentCondition::Always,
            }]
        };

        context.extend_environment(&binding("ONE")).unwrap();
        context.extend_environment(&binding("TWO")).unwrap();
        assert_eq!(context.environment.len(), 2);
        assert_eq!(
            context.extend_environment(&binding("THREE")),
            Err(ContextError::ResolvedItemLimit { count: 3, limit: 2 })
        );
        assert_eq!(context.environment.len(), 2);
    }

    #[test]
    fn resolver_output_limit_accepts_n_and_rejects_n_plus_one() {
        let context = fixture_context("x86_64", false, false);
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_resolved_text_bytes = 3;
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );

        assert_eq!(
            resolver.resolve(&TextSpec::Literal("abc".to_owned())),
            Ok("abc".to_owned())
        );
        assert_eq!(
            resolver.resolve(&TextSpec::Literal("abcd".to_owned())),
            Err(ContextError::ResolvedTextBytesLimit { bytes: 4, limit: 3 })
        );
    }

    #[test]
    fn resolver_wide_concat_limit_is_exact_and_linear() {
        let context = fixture_context("x86_64", false, false);
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_nodes = 10_001;
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );
        let at_limit = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 10_000]);
        assert_eq!(resolver.resolve(&at_limit).unwrap().len(), 10_000);

        let over_limit = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 10_001]);
        assert_eq!(
            resolver.resolve(&over_limit),
            Err(ContextError::TextNodeLimit {
                nodes: 10_002,
                limit: 10_001,
            })
        );
    }

    #[test]
    fn resolver_rejects_deep_text_without_recursive_calls() {
        let context = fixture_context("x86_64", false, false);
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_nodes = 25_000;
        limits.max_text_depth = 64;
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );
        let mut value = TextSpec::Literal("x".to_owned());
        for _ in 0..20_000 {
            value = TextSpec::Concat(vec![value]);
        }

        assert_eq!(
            resolver.resolve(&value),
            Err(ContextError::TextDepthLimit { depth: 65, limit: 64 })
        );
    }

    #[test]
    fn resolver_flag_limit_accepts_n_and_rejects_n_plus_one() {
        let mut context = fixture_context("x86_64", false, false);
        context.inputs.flags.c = vec![TextSpec::Literal("x".to_owned()); 3];
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_compiler_flags = 3;
        let value = TextSpec::Context(ContextValue::CFlags);
        {
            let resolver = TextResolver::new(
                &context.policy,
                &context.target,
                &context.inputs,
                TextContextOverlay::default(),
                limits,
            );
            assert_eq!(resolver.resolve(&value), Ok("x x x".to_owned()));
        }

        context.inputs.flags.c.push(TextSpec::Literal("x".to_owned()));
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );
        assert_eq!(
            resolver.resolve(&value),
            Err(ContextError::FlagCollectionLimit { count: 4, limit: 3 })
        );
    }
}
