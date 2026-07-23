use crate::build_policy::{
    BuildCommandSpec, BuildPolicySpec, BuildProgramSpec, BuildRootPolicySpec, BuildToolSpec, BuilderCommandSpec,
    CompilerFlagsSpec, CompilerToolsSpec, EnvironmentBindingSpec, InstallLayoutSpec, PgoPolicySpec, PgoStagePolicySpec,
    PlatformPolicySpec, SandboxPolicySpec, SourcePreparationPolicySpec, StandardBuilderPolicySpec, TargetEmulationSpec,
    TargetPolicySpec, TextSpec, ToolchainFlagsSpec, ToolchainInputPolicySpec, TuningOptionSpec, TuningPolicySpec,
};

use super::{BuildPolicyConversionError, BuildPolicyValidationLimits};

pub(in crate::build_policy) struct ResourceValidator {
    pub(in crate::build_policy) limits: BuildPolicyValidationLimits,
    pub(in crate::build_policy) total_collection_items: usize,
    pub(in crate::build_policy) total_string_bytes: usize,
    pub(in crate::build_policy) total_text_nodes: usize,
    pub(in crate::build_policy) total_text_literal_bytes: usize,
}

impl ResourceValidator {
    pub(in crate::build_policy) fn new(limits: BuildPolicyValidationLimits) -> Self {
        Self {
            limits,
            total_collection_items: 0,
            total_string_bytes: 0,
            total_text_nodes: 0,
            total_text_literal_bytes: 0,
        }
    }

    pub(in crate::build_policy) fn policy(
        &mut self,
        policy: &BuildPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.string("build_subdir", &policy.build_subdir)?;
        self.layout(&policy.layout)?;
        self.compiler_tools("toolchains.llvm", &policy.toolchains.llvm)?;
        self.compiler_tools("toolchains.gnu", &policy.toolchains.gnu)?;

        self.collection("targets", policy.targets.len(), self.limits.max_targets)?;
        for (index, target) in policy.targets.iter().enumerate() {
            self.target(&format!("targets[{index}]"), target)?;
        }
        self.collection(
            "retired_targets",
            policy.retired_targets.len(),
            self.limits.max_retired_targets,
        )?;
        for (index, target) in policy.retired_targets.iter().enumerate() {
            self.string(&format!("retired_targets[{index}].name"), &target.name)?;
            self.string(&format!("retired_targets[{index}].reason"), &target.reason)?;
        }

        self.sandbox(&policy.sandbox)?;
        self.build_root(&policy.build_root)?;
        self.sources(&policy.sources)?;
        self.tuning(&policy.tuning)?;
        self.bindings("environment", &policy.environment)?;
        self.builder("builders.cmake", &policy.builders.cmake)?;
        self.builder("builders.meson", &policy.builders.meson)?;
        self.builder("builders.cargo", &policy.builders.cargo)?;
        self.builder("builders.autotools", &policy.builders.autotools)?;
        self.collection("analyzers", policy.analyzers.len(), self.limits.max_analyzers)?;
        self.pgo(&policy.pgo)
    }

    pub(in crate::build_policy) fn collection(
        &mut self,
        field: &str,
        count: usize,
        limit: usize,
    ) -> Result<(), BuildPolicyConversionError> {
        if count > limit {
            return Err(BuildPolicyConversionError::CollectionLimit {
                field: field.to_owned(),
                count,
                limit,
            });
        }
        self.total_collection_items = self.total_collection_items.saturating_add(count);
        if self.total_collection_items > self.limits.max_total_collection_items {
            return Err(BuildPolicyConversionError::TotalCollectionItemsLimit {
                count: self.total_collection_items,
                limit: self.limits.max_total_collection_items,
            });
        }
        Ok(())
    }

    pub(in crate::build_policy) fn string(
        &mut self,
        field: &str,
        value: &str,
    ) -> Result<(), BuildPolicyConversionError> {
        let bytes = value.len();
        if bytes > self.limits.max_string_bytes {
            return Err(BuildPolicyConversionError::StringBytesLimit {
                field: field.to_owned(),
                bytes,
                limit: self.limits.max_string_bytes,
            });
        }
        self.total_string_bytes = self.total_string_bytes.saturating_add(bytes);
        if self.total_string_bytes > self.limits.max_total_string_bytes {
            return Err(BuildPolicyConversionError::TotalStringBytesLimit {
                bytes: self.total_string_bytes,
                limit: self.limits.max_total_string_bytes,
            });
        }
        Ok(())
    }

    pub(in crate::build_policy) fn text(
        &mut self,
        field: &str,
        value: &TextSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        let mut stack = vec![(value, 1usize)];
        let mut nodes = 0usize;
        let mut literal_bytes = 0usize;

        while let Some((value, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            if nodes > self.limits.max_text_nodes {
                return Err(BuildPolicyConversionError::TextNodeLimit {
                    field: field.to_owned(),
                    nodes,
                    limit: self.limits.max_text_nodes,
                });
            }
            self.total_text_nodes = self.total_text_nodes.saturating_add(1);
            if self.total_text_nodes > self.limits.max_total_text_nodes {
                return Err(BuildPolicyConversionError::TotalTextNodesLimit {
                    nodes: self.total_text_nodes,
                    limit: self.limits.max_total_text_nodes,
                });
            }
            if depth > self.limits.max_text_depth {
                return Err(BuildPolicyConversionError::TextDepthLimit {
                    field: field.to_owned(),
                    depth,
                    limit: self.limits.max_text_depth,
                });
            }

            match value {
                TextSpec::Literal(value) => {
                    let bytes = value.len();
                    if bytes > self.limits.max_text_literal_bytes {
                        return Err(BuildPolicyConversionError::TextLiteralBytesLimit {
                            field: field.to_owned(),
                            bytes,
                            limit: self.limits.max_text_literal_bytes,
                        });
                    }
                    literal_bytes = literal_bytes.saturating_add(bytes);
                    if literal_bytes > self.limits.max_text_total_literal_bytes {
                        return Err(BuildPolicyConversionError::TextTotalLiteralBytesLimit {
                            field: field.to_owned(),
                            bytes: literal_bytes,
                            limit: self.limits.max_text_total_literal_bytes,
                        });
                    }
                    self.total_text_literal_bytes = self.total_text_literal_bytes.saturating_add(bytes);
                    if self.total_text_literal_bytes > self.limits.max_total_text_literal_bytes {
                        return Err(BuildPolicyConversionError::TotalTextLiteralBytesLimit {
                            bytes: self.total_text_literal_bytes,
                            limit: self.limits.max_total_text_literal_bytes,
                        });
                    }
                    self.string(field, value)?;
                }
                TextSpec::Context(_) => {}
                TextSpec::Concat(parts) => {
                    let minimum_nodes = nodes.saturating_add(stack.len()).saturating_add(parts.len());
                    if minimum_nodes > self.limits.max_text_nodes {
                        return Err(BuildPolicyConversionError::TextNodeLimit {
                            field: field.to_owned(),
                            nodes: minimum_nodes,
                            limit: self.limits.max_text_nodes,
                        });
                    }
                    let minimum_total = self
                        .total_text_nodes
                        .saturating_add(stack.len())
                        .saturating_add(parts.len());
                    if minimum_total > self.limits.max_total_text_nodes {
                        return Err(BuildPolicyConversionError::TotalTextNodesLimit {
                            nodes: minimum_total,
                            limit: self.limits.max_total_text_nodes,
                        });
                    }
                    stack
                        .try_reserve(parts.len())
                        .map_err(|_| BuildPolicyConversionError::Capacity {
                            field: field.to_owned(),
                            count: minimum_nodes,
                        })?;
                    let child_depth = depth.saturating_add(1);
                    for part in parts.iter().rev() {
                        stack.push((part, child_depth));
                    }
                }
            }
        }
        Ok(())
    }

    pub(in crate::build_policy) fn texts(
        &mut self,
        field: &str,
        values: &[TextSpec],
        limit: usize,
    ) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, values.len(), limit)?;
        for (index, value) in values.iter().enumerate() {
            self.text(&format!("{field}[{index}]"), value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn layout(
        &mut self,
        layout: &InstallLayoutSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("prefix", &layout.prefix),
            ("bindir", &layout.bindir),
            ("sbindir", &layout.sbindir),
            ("includedir", &layout.includedir),
            ("libdir", &layout.libdir),
            ("libexecdir", &layout.libexecdir),
            ("datadir", &layout.datadir),
            ("vendordir", &layout.vendordir),
            ("docdir", &layout.docdir),
            ("infodir", &layout.infodir),
            ("localedir", &layout.localedir),
            ("mandir", &layout.mandir),
            ("sysconfdir", &layout.sysconfdir),
            ("localstatedir", &layout.localstatedir),
            ("sharedstatedir", &layout.sharedstatedir),
            ("runstatedir", &layout.runstatedir),
            ("sysusersdir", &layout.sysusersdir),
            ("tmpfilesdir", &layout.tmpfilesdir),
            ("udevrulesdir", &layout.udevrulesdir),
            ("bash_completions_dir", &layout.bash_completions_dir),
            ("fish_completions_dir", &layout.fish_completions_dir),
            ("elvish_completions_dir", &layout.elvish_completions_dir),
            ("zsh_completions_dir", &layout.zsh_completions_dir),
        ] {
            self.text(&format!("layout.{name}"), value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn compiler_tools(
        &mut self,
        field: &str,
        tools: &CompilerToolsSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("cc", &tools.cc),
            ("cxx", &tools.cxx),
            ("objc", &tools.objc),
            ("objcxx", &tools.objcxx),
            ("cpp", &tools.cpp),
            ("objcpp", &tools.objcpp),
            ("objcxxcpp", &tools.objcxxcpp),
            ("ar", &tools.ar),
            ("ld", &tools.ld),
            ("objcopy", &tools.objcopy),
            ("nm", &tools.nm),
            ("ranlib", &tools.ranlib),
            ("strip", &tools.strip),
        ] {
            self.build_command(&format!("{field}.{name}"), value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn target(
        &mut self,
        field: &str,
        target: &TargetPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("name", &target.name),
            ("target_triple", &target.target_triple),
            ("build_triple", &target.build_triple),
            ("host_triple", &target.host_triple),
            ("lib_suffix", &target.lib_suffix),
            ("artifact_architecture", &target.artifact_architecture),
        ] {
            self.string(&format!("{field}.{name}"), value)?;
        }
        if let TargetEmulationSpec::Emul32 { host_architecture } = &target.emulation {
            self.string(&format!("{field}.emulation.host_architecture"), host_architecture)?;
        }
        self.platform(&format!("{field}.build_platform"), &target.build_platform)?;
        self.platform(&format!("{field}.host_platform"), &target.host_platform)?;
        self.platform(&format!("{field}.target_platform"), &target.target_platform)?;
        self.toolchain_flags(&format!("{field}.architecture_flags"), &target.architecture_flags)?;
        self.bindings(&format!("{field}.environment"), &target.environment)
    }

    pub(in crate::build_policy) fn platform(
        &mut self,
        field: &str,
        platform: &PlatformPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("architecture", &platform.architecture),
            ("vendor", &platform.vendor),
            ("operating_system", &platform.operating_system),
            ("abi", &platform.abi),
        ] {
            self.string(&format!("{field}.{name}"), value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn sandbox(
        &mut self,
        sandbox: &SandboxPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("hostname", &sandbox.hostname),
            ("guest_root", &sandbox.guest_root),
            ("artifacts_dir", &sandbox.artifacts_dir),
            ("build_dir", &sandbox.build_dir),
            ("source_dir", &sandbox.source_dir),
            ("recipe_dir", &sandbox.recipe_dir),
            ("package_dir", &sandbox.package_dir),
            ("install_dir", &sandbox.install_dir),
        ] {
            self.string(&format!("sandbox.{name}"), value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn build_root(
        &mut self,
        root: &BuildRootPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.tools("build_root.base", &root.base)?;
        self.toolchain_inputs("build_root.toolchains", &root.toolchains)?;
        self.tools("build_root.emul32.base", &root.emul32.base)?;
        self.toolchain_inputs("build_root.emul32.toolchains", &root.emul32.toolchains)?;
        for (field, tool) in [
            ("build_root.analyzer_tools.pkg_config", &root.analyzer_tools.pkg_config),
            ("build_root.analyzer_tools.python", &root.analyzer_tools.python),
            (
                "build_root.analyzer_tools.llvm.objcopy",
                &root.analyzer_tools.llvm.objcopy,
            ),
            ("build_root.analyzer_tools.llvm.strip", &root.analyzer_tools.llvm.strip),
            (
                "build_root.analyzer_tools.gnu.objcopy",
                &root.analyzer_tools.gnu.objcopy,
            ),
            ("build_root.analyzer_tools.gnu.strip", &root.analyzer_tools.gnu.strip),
        ] {
            self.tool(field, tool)?;
        }
        let cache = &root.compiler_cache;
        self.program("build_root.compiler_cache.ccache", &cache.ccache)?;
        self.program("build_root.compiler_cache.sccache", &cache.sccache)?;
        for (name, value) in [
            ("ccache_dir", &cache.ccache_dir),
            ("sccache_dir", &cache.sccache_dir),
            ("go_cache_dir", &cache.go_cache_dir),
            ("go_mod_cache_dir", &cache.go_mod_cache_dir),
            ("cargo_cache_dir", &cache.cargo_cache_dir),
            ("zig_cache_dir", &cache.zig_cache_dir),
        ] {
            self.string(&format!("build_root.compiler_cache.{name}"), value)?;
        }
        self.build_command("build_root.mold.linker", &root.mold.linker)?;
        self.compiler_flags("build_root.mold.flags", &root.mold.flags)
    }

    pub(in crate::build_policy) fn toolchain_inputs(
        &mut self,
        field: &str,
        inputs: &ToolchainInputPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.tools(&format!("{field}.llvm"), &inputs.llvm)?;
        self.tools(&format!("{field}.gnu"), &inputs.gnu)
    }

    pub(in crate::build_policy) fn tools(
        &mut self,
        field: &str,
        tools: &[BuildToolSpec],
    ) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, tools.len(), self.limits.max_build_root_tools)?;
        for (index, tool) in tools.iter().enumerate() {
            self.tool(&format!("{field}[{index}]"), tool)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn tool(
        &mut self,
        field: &str,
        tool: &BuildToolSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        let value = match tool {
            BuildToolSpec::Package(value) | BuildToolSpec::Binary(value) | BuildToolSpec::SystemBinary(value) => value,
        };
        self.string(field, value)
    }

    pub(in crate::build_policy) fn sources(
        &mut self,
        sources: &SourcePreparationPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.command("sources.git.create_directory", &sources.git.create_directory)?;
        self.command("sources.git.copy", &sources.git.copy)
    }

    pub(in crate::build_policy) fn tuning(
        &mut self,
        tuning: &TuningPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.collection("tuning.flags", tuning.flags.len(), self.limits.max_tuning_flags)?;
        for (index, flag) in tuning.flags.iter().enumerate() {
            let field = format!("tuning.flags[{index}]");
            self.string(&format!("{field}.name"), &flag.name)?;
            self.toolchain_flags(&format!("{field}.value"), &flag.value)?;
        }
        self.collection("tuning.groups", tuning.groups.len(), self.limits.max_tuning_groups)?;
        for (index, group) in tuning.groups.iter().enumerate() {
            let field = format!("tuning.groups[{index}]");
            self.string(&format!("{field}.name"), &group.name)?;
            self.tuning_option(&format!("{field}.value.base"), &group.value.base)?;
            if let Some(default) = &group.value.default {
                self.string(&format!("{field}.value.default"), default)?;
            }
            self.collection(
                &format!("{field}.value.choices"),
                group.value.choices.len(),
                self.limits.max_tuning_choices,
            )?;
            for (choice_index, choice) in group.value.choices.iter().enumerate() {
                let choice_field = format!("{field}.value.choices[{choice_index}]");
                self.string(&format!("{choice_field}.name"), &choice.name)?;
                self.tuning_option(&format!("{choice_field}.value"), &choice.value)?;
            }
        }
        self.collection(
            "tuning.default_groups",
            tuning.default_groups.len(),
            self.limits.max_tuning_default_groups,
        )?;
        for (index, group) in tuning.default_groups.iter().enumerate() {
            self.string(&format!("tuning.default_groups[{index}]"), group)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn tuning_option(
        &mut self,
        field: &str,
        option: &TuningOptionSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, values) in [("enabled", &option.enabled), ("disabled", &option.disabled)] {
            let values_field = format!("{field}.{name}");
            self.collection(&values_field, values.len(), self.limits.max_tuning_option_flags)?;
            for (index, value) in values.iter().enumerate() {
                self.string(&format!("{values_field}[{index}]"), value)?;
            }
        }
        Ok(())
    }

    pub(in crate::build_policy) fn bindings(
        &mut self,
        field: &str,
        bindings: &[EnvironmentBindingSpec],
    ) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, bindings.len(), self.limits.max_environment_bindings)?;
        for (index, binding) in bindings.iter().enumerate() {
            self.string(&format!("{field}[{index}].name"), &binding.name)?;
            self.text(&format!("{field}[{index}].value"), &binding.value)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn builder(
        &mut self,
        field: &str,
        builder: &StandardBuilderPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.bindings(&format!("{field}.environment"), &builder.environment)?;
        self.command(&format!("{field}.setup"), &builder.setup)?;
        self.command(&format!("{field}.build"), &builder.build)?;
        self.command(&format!("{field}.install"), &builder.install)?;
        self.command(&format!("{field}.check"), &builder.check)
    }

    pub(in crate::build_policy) fn command(
        &mut self,
        field: &str,
        command: &BuilderCommandSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.program(&format!("{field}.program"), &command.program)?;
        self.text(&format!("{field}.working_dir"), &command.working_dir)?;
        self.texts(
            &format!("{field}.args"),
            &command.args,
            self.limits.max_builder_arguments,
        )?;
        self.bindings(&format!("{field}.environment"), &command.environment)
    }

    pub(in crate::build_policy) fn build_command(
        &mut self,
        field: &str,
        command: &BuildCommandSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.program(&format!("{field}.program"), &command.program)?;
        self.collection(
            &format!("{field}.args"),
            command.args.len(),
            self.limits.max_builder_arguments,
        )?;
        for (index, argument) in command.args.iter().enumerate() {
            self.string(&format!("{field}.args[{index}]"), argument)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn program(
        &mut self,
        field: &str,
        program: &BuildProgramSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.string(&format!("{field}.path"), &program.path)?;
        self.tool(&format!("{field}.requirement"), &program.requirement)
    }

    pub(in crate::build_policy) fn toolchain_flags(
        &mut self,
        field: &str,
        flags: &ToolchainFlagsSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.compiler_flags(&format!("{field}.common"), &flags.common)?;
        self.compiler_flags(&format!("{field}.gnu"), &flags.gnu)?;
        self.compiler_flags(&format!("{field}.llvm"), &flags.llvm)
    }

    pub(in crate::build_policy) fn compiler_flags(
        &mut self,
        field: &str,
        flags: &CompilerFlagsSpec,
    ) -> Result<(), BuildPolicyConversionError> {
        for (name, values) in [
            ("c", &flags.c),
            ("cxx", &flags.cxx),
            ("f", &flags.f),
            ("d", &flags.d),
            ("rust", &flags.rust),
            ("vala", &flags.vala),
            ("go", &flags.go),
            ("ld", &flags.ld),
        ] {
            self.texts(&format!("{field}.{name}"), values, self.limits.max_compiler_flags)?;
        }
        Ok(())
    }

    pub(in crate::build_policy) fn pgo(&mut self, pgo: &PgoPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.program("pgo.shell_interpreter", &pgo.shell_interpreter)?;
        self.program("pgo.merge_program", &pgo.merge_program)?;
        self.texts("pgo.merge_args", &pgo.merge_args, self.limits.max_pgo_arguments)?;
        self.program("pgo.copy_program", &pgo.copy_program)?;
        self.program("pgo.remove_program", &pgo.remove_program)?;
        self.toolchain_flags("pgo.sample", &pgo.sample)?;
        self.pgo_stage("pgo.stage_one", &pgo.stage_one)?;
        self.pgo_stage("pgo.stage_two", &pgo.stage_two)?;
        self.pgo_stage("pgo.use_profile", &pgo.use_profile)
    }

    pub(in crate::build_policy) fn pgo_stage(
        &mut self,
        field: &str,
        stage: &PgoStagePolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.toolchain_flags(&format!("{field}.flags"), &stage.flags)?;
        let Some(finish) = &stage.finish else {
            return Ok(());
        };
        self.text(&format!("{field}.finish.output"), &finish.output)?;
        self.texts(
            &format!("{field}.finish.inputs"),
            &finish.inputs,
            self.limits.max_pgo_inputs,
        )?;
        if let Some(copy_to) = &finish.copy_to {
            self.text(&format!("{field}.finish.copy_to"), copy_to)?;
        }
        Ok(())
    }
}
