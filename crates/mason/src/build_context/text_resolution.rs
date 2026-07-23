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
