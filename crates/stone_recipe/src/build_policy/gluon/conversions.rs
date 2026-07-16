impl From<GluonBoolean> for bool {
    fn from(value: GluonBoolean) -> Self {
        matches!(value, GluonBoolean::True)
    }
}

impl From<GluonContextValue> for ContextValue {
    fn from(value: GluonContextValue) -> Self {
        match value {
            GluonContextValue::PackageName => Self::PackageName,
            GluonContextValue::PackageVersion => Self::PackageVersion,
            GluonContextValue::PackageRelease => Self::PackageRelease,
            GluonContextValue::SourceDir => Self::SourceDir,
            GluonContextValue::InstallRoot => Self::InstallRoot,
            GluonContextValue::BuildRoot => Self::BuildRoot,
            GluonContextValue::WorkDir => Self::WorkDir,
            GluonContextValue::BuilderDir => Self::BuilderDir,
            GluonContextValue::PgoDir => Self::PgoDir,
            GluonContextValue::Jobs => Self::Jobs,
            GluonContextValue::SourceDateEpoch => Self::SourceDateEpoch,
            GluonContextValue::PgoStage => Self::PgoStage,
            GluonContextValue::TargetTriple => Self::TargetTriple,
            GluonContextValue::BuildPlatform => Self::BuildPlatform,
            GluonContextValue::HostPlatform => Self::HostPlatform,
            GluonContextValue::LibSuffix => Self::LibSuffix,
            GluonContextValue::Prefix => Self::Prefix,
            GluonContextValue::BinDir => Self::BinDir,
            GluonContextValue::SbinDir => Self::SbinDir,
            GluonContextValue::IncludeDir => Self::IncludeDir,
            GluonContextValue::LibDir => Self::LibDir,
            GluonContextValue::LibexecDir => Self::LibexecDir,
            GluonContextValue::DataDir => Self::DataDir,
            GluonContextValue::VendorDir => Self::VendorDir,
            GluonContextValue::DocDir => Self::DocDir,
            GluonContextValue::InfoDir => Self::InfoDir,
            GluonContextValue::LocaleDir => Self::LocaleDir,
            GluonContextValue::ManDir => Self::ManDir,
            GluonContextValue::SysconfDir => Self::SysconfDir,
            GluonContextValue::LocalStateDir => Self::LocalStateDir,
            GluonContextValue::SharedStateDir => Self::SharedStateDir,
            GluonContextValue::RunStateDir => Self::RunStateDir,
            GluonContextValue::CFlags => Self::CFlags,
            GluonContextValue::CxxFlags => Self::CxxFlags,
            GluonContextValue::FFlags => Self::FFlags,
            GluonContextValue::DFlags => Self::DFlags,
            GluonContextValue::RustFlags => Self::RustFlags,
            GluonContextValue::ValaFlags => Self::ValaFlags,
            GluonContextValue::GoFlags => Self::GoFlags,
            GluonContextValue::LdFlags => Self::LdFlags,
            GluonContextValue::Cc => Self::Cc,
            GluonContextValue::Cxx => Self::Cxx,
            GluonContextValue::Objc => Self::Objc,
            GluonContextValue::Objcxx => Self::Objcxx,
            GluonContextValue::Cpp => Self::Cpp,
            GluonContextValue::Objcpp => Self::Objcpp,
            GluonContextValue::Objcxxcpp => Self::Objcxxcpp,
            GluonContextValue::Ar => Self::Ar,
            GluonContextValue::Ld => Self::Ld,
            GluonContextValue::Objcopy => Self::Objcopy,
            GluonContextValue::Nm => Self::Nm,
            GluonContextValue::Ranlib => Self::Ranlib,
            GluonContextValue::Strip => Self::Strip,
            GluonContextValue::CcacheDir => Self::CcacheDir,
            GluonContextValue::SccacheDir => Self::SccacheDir,
            GluonContextValue::GoCacheDir => Self::GoCacheDir,
            GluonContextValue::GoModCacheDir => Self::GoModCacheDir,
            GluonContextValue::CargoCacheDir => Self::CargoCacheDir,
            GluonContextValue::ZigCacheDir => Self::ZigCacheDir,
            GluonContextValue::RustcWrapper => Self::RustcWrapper,
            GluonContextValue::SourcePath => Self::SourcePath,
            GluonContextValue::SourceDestination => Self::SourceDestination,
        }
    }
}

impl From<GluonTextSpec> for TextSpec {
    fn from(value: GluonTextSpec) -> Self {
        let mut parts = value.parts.into_iter().map(|part| match part {
            GluonTextPartSpec::LiteralPart { value } => Self::Literal(value),
            GluonTextPartSpec::ContextPart { value } => Self::Context(value.into()),
        });
        let Some(first) = parts.next() else {
            return Self::Concat(Vec::new());
        };
        match parts.next() {
            None => first,
            Some(second) => Self::Concat([first, second].into_iter().chain(parts).collect()),
        }
    }
}

impl From<GluonCompilerFlagsSpec> for CompilerFlagsSpec {
    fn from(value: GluonCompilerFlagsSpec) -> Self {
        Self {
            c: value.c.into_iter().map(Into::into).collect(),
            cxx: value.cxx.into_iter().map(Into::into).collect(),
            f: value.f.into_iter().map(Into::into).collect(),
            d: value.d.into_iter().map(Into::into).collect(),
            rust: value.rust.into_iter().map(Into::into).collect(),
            vala: value.vala.into_iter().map(Into::into).collect(),
            go: value.go.into_iter().map(Into::into).collect(),
            ld: value.ld.into_iter().map(Into::into).collect(),
        }
    }
}

macro_rules! convert_record {
    ($from:ty => $to:ty { $($field:ident),+ $(,)? }) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                Self { $($field: value.$field.into()),+ }
            }
        }
    };
}

convert_record!(GluonInstallLayoutSpec => InstallLayoutSpec {
    prefix, bindir, sbindir, includedir, libdir, libexecdir, datadir, vendordir, docdir, infodir, localedir,
    mandir, sysconfdir, localstatedir, sharedstatedir, runstatedir, sysusersdir, tmpfilesdir, udevrulesdir,
    bash_completions_dir, fish_completions_dir, elvish_completions_dir, zsh_completions_dir,
});
convert_record!(GluonCompilerToolsSpec => CompilerToolsSpec {
    cc, cxx, objc, objcxx, cpp, objcpp, objcxxcpp, ar, ld, objcopy, nm, ranlib, strip,
});
convert_record!(GluonToolchainsSpec => ToolchainsSpec { llvm, gnu });
convert_record!(GluonPlatformPolicySpec => PlatformPolicySpec {
    architecture, vendor, operating_system, abi,
});
impl From<GluonTargetPolicySpec> for TargetPolicySpec {
    fn from(value: GluonTargetPolicySpec) -> Self {
        Self {
            name: value.name,
            target_triple: value.target_triple,
            build_triple: value.build_triple,
            host_triple: value.host_triple,
            lib_suffix: value.lib_suffix,
            artifact_architecture: value.artifact_architecture,
            emulation: value.emulation.into(),
            build_platform: value.build_platform.into(),
            host_platform: value.host_platform.into(),
            target_platform: value.target_platform.into(),
            architecture_flags: value.architecture_flags.into(),
            environment: value.environment.into_iter().map(Into::into).collect(),
        }
    }
}
convert_record!(GluonRetiredTargetPolicySpec => RetiredTargetPolicySpec { name, reason });
convert_record!(GluonEnvironmentBindingSpec => EnvironmentBindingSpec { name, value, condition });
convert_record!(GluonSandboxFilesystemPolicySpec => SandboxFilesystemPolicySpec { tmp, sys, dev });
convert_record!(GluonSandboxPolicySpec => SandboxPolicySpec {
    hostname, credentials, filesystems, guest_root, artifacts_dir, build_dir, source_dir, recipe_dir, package_dir, install_dir,
});
convert_record!(GluonSourcePreparationPolicySpec => SourcePreparationPolicySpec { git });
convert_record!(GluonBuildersPolicySpec => BuildersPolicySpec { cmake, meson, cargo, autotools });
convert_record!(GluonToolchainFlagsSpec => ToolchainFlagsSpec { common, gnu, llvm });
convert_record!(GluonNamedTuningFlagSpec => NamedTuningFlagSpec { name, value });
convert_record!(GluonTuningOptionSpec => TuningOptionSpec { enabled, disabled });
convert_record!(GluonNamedTuningChoiceSpec => NamedTuningChoiceSpec { name, value });

impl From<GluonTuningGroupSpec> for TuningGroupSpec {
    fn from(value: GluonTuningGroupSpec) -> Self {
        Self {
            base: value.base.into(),
            default: match value.default {
                GluonOptionalChoiceName::NoChoice => None,
                GluonOptionalChoiceName::SomeChoice(value) => Some(value),
            },
            choices: value.choices.into_iter().map(Into::into).collect(),
        }
    }
}
convert_record!(GluonNamedTuningGroupSpec => NamedTuningGroupSpec { name, value });

impl From<GluonTuningPolicySpec> for TuningPolicySpec {
    fn from(value: GluonTuningPolicySpec) -> Self {
        Self {
            flags: value.flags.into_iter().map(Into::into).collect(),
            groups: value.groups.into_iter().map(Into::into).collect(),
            default_groups: value.default_groups,
        }
    }
}

impl From<GluonBuilderCommandSpec> for BuilderCommandSpec {
    fn from(value: GluonBuilderCommandSpec) -> Self {
        Self {
            program: value.program.into(),
            args: value.args.into_iter().map(Into::into).collect(),
            environment: value.environment.into_iter().map(Into::into).collect(),
            working_dir: value.working_dir.into(),
        }
    }
}

impl From<GluonBuildProgramSpec> for BuildProgramSpec {
    fn from(value: GluonBuildProgramSpec) -> Self {
        Self {
            path: value.path,
            requirement: value.requirement.into(),
        }
    }
}

impl From<GluonBuildCommandSpec> for BuildCommandSpec {
    fn from(value: GluonBuildCommandSpec) -> Self {
        Self {
            program: value.program.into(),
            args: value.args,
        }
    }
}

fn convert_tools(tools: Vec<GluonBuildToolSpec>) -> Vec<BuildToolSpec> {
    tools.into_iter().map(Into::into).collect()
}

impl From<GluonToolchainInputPolicySpec> for ToolchainInputPolicySpec {
    fn from(value: GluonToolchainInputPolicySpec) -> Self {
        Self {
            llvm: convert_tools(value.llvm),
            gnu: convert_tools(value.gnu),
        }
    }
}

impl From<GluonEmul32InputPolicySpec> for Emul32InputPolicySpec {
    fn from(value: GluonEmul32InputPolicySpec) -> Self {
        Self {
            base: convert_tools(value.base),
            toolchains: value.toolchains.into(),
        }
    }
}

impl From<GluonAnalyzerToolchainPolicySpec> for AnalyzerToolchainPolicySpec {
    fn from(value: GluonAnalyzerToolchainPolicySpec) -> Self {
        Self {
            objcopy: value.objcopy.into(),
            strip: value.strip.into(),
        }
    }
}

impl From<GluonAnalyzerToolsPolicySpec> for AnalyzerToolsPolicySpec {
    fn from(value: GluonAnalyzerToolsPolicySpec) -> Self {
        Self {
            pkg_config: value.pkg_config.into(),
            python: value.python.into(),
            llvm: value.llvm.into(),
            gnu: value.gnu.into(),
        }
    }
}

impl From<GluonCompilerCachePolicySpec> for CompilerCachePolicySpec {
    fn from(value: GluonCompilerCachePolicySpec) -> Self {
        Self {
            ccache: value.ccache.into(),
            sccache: value.sccache.into(),
            ccache_dir: value.ccache_dir,
            sccache_dir: value.sccache_dir,
            go_cache_dir: value.go_cache_dir,
            go_mod_cache_dir: value.go_mod_cache_dir,
            cargo_cache_dir: value.cargo_cache_dir,
            zig_cache_dir: value.zig_cache_dir,
        }
    }
}

impl From<GluonMoldPolicySpec> for MoldPolicySpec {
    fn from(value: GluonMoldPolicySpec) -> Self {
        Self {
            linker: value.linker.into(),
            flags: value.flags.into(),
        }
    }
}

impl From<GluonBuildRootPolicySpec> for BuildRootPolicySpec {
    fn from(value: GluonBuildRootPolicySpec) -> Self {
        Self {
            base: convert_tools(value.base),
            toolchains: value.toolchains.into(),
            emul32: value.emul32.into(),
            analyzer_tools: value.analyzer_tools.into(),
            compiler_cache: value.compiler_cache.into(),
            mold: value.mold.into(),
        }
    }
}

impl From<GluonGitPreparationPolicySpec> for GitPreparationPolicySpec {
    fn from(value: GluonGitPreparationPolicySpec) -> Self {
        Self {
            create_directory: value.create_directory.into(),
            copy: value.copy.into(),
        }
    }
}

impl From<GluonStandardBuilderPolicySpec> for StandardBuilderPolicySpec {
    fn from(value: GluonStandardBuilderPolicySpec) -> Self {
        Self {
            environment: value.environment.into_iter().map(Into::into).collect(),
            setup: value.setup.into(),
            build: value.build.into(),
            install: value.install.into(),
            check: value.check.into(),
        }
    }
}

impl From<GluonPgoFinishSpec> for PgoFinishSpec {
    fn from(value: GluonPgoFinishSpec) -> Self {
        Self {
            output: value.output.into(),
            inputs: value.inputs.into_iter().map(Into::into).collect(),
            copy_to: match value.copy_to {
                GluonOptionalTextSpec::NoText => None,
                GluonOptionalTextSpec::SomeText(value) => Some(value.into()),
            },
            remove_output_first: value.remove_output_first.into(),
        }
    }
}

impl From<GluonPgoStagePolicySpec> for PgoStagePolicySpec {
    fn from(value: GluonPgoStagePolicySpec) -> Self {
        Self {
            flags: value.flags.into(),
            finish: match value.finish {
                GluonOptionalPgoFinishSpec::NoPgoFinish => None,
                GluonOptionalPgoFinishSpec::SomePgoFinish(value) => Some(value.into()),
            },
        }
    }
}

impl From<GluonPgoPolicySpec> for PgoPolicySpec {
    fn from(value: GluonPgoPolicySpec) -> Self {
        Self {
            shell_interpreter: value.shell_interpreter.into(),
            merge_program: value.merge_program.into(),
            merge_args: value.merge_args.into_iter().map(Into::into).collect(),
            copy_program: value.copy_program.into(),
            remove_program: value.remove_program.into(),
            sample: value.sample.into(),
            stage_one: value.stage_one.into(),
            stage_two: value.stage_two.into(),
            use_profile: value.use_profile.into(),
        }
    }
}

impl From<GluonBuildPolicySpec> for BuildPolicySpec {
    fn from(value: GluonBuildPolicySpec) -> Self {
        Self {
            build_subdir: value.build_subdir,
            layout: value.layout.into(),
            toolchains: value.toolchains.into(),
            targets: value.targets.into_iter().map(Into::into).collect(),
            retired_targets: value.retired_targets.into_iter().map(Into::into).collect(),
            sandbox: value.sandbox.into(),
            build_root: value.build_root.into(),
            sources: value.sources.into(),
            tuning: value.tuning.into(),
            environment: value.environment.into_iter().map(Into::into).collect(),
            builders: value.builders.into(),
            analyzers: value.analyzers.into_iter().map(Into::into).collect(),
            pgo: value.pgo.into(),
        }
    }
}

impl<T, U> From<GluonValuePatch<T>> for ValuePatch<U>
where
    T: Into<U>,
{
    fn from(value: GluonValuePatch<T>) -> Self {
        match value {
            GluonValuePatch::KeepValue => Self::Keep,
            GluonValuePatch::SetValue(value) => Self::Set(value.into()),
        }
    }
}

impl<T, U> From<GluonArrayPatch<T>> for ArrayPatch<U>
where
    T: Into<U>,
{
    fn from(value: GluonArrayPatch<T>) -> Self {
        let convert = |values: Vec<T>| values.into_iter().map(Into::into).collect();
        match value {
            GluonArrayPatch::KeepArray => Self::Keep,
            GluonArrayPatch::ReplaceArray(values) => Self::Replace(convert(values)),
            GluonArrayPatch::PrependArray(values) => Self::Prepend(convert(values)),
            GluonArrayPatch::AppendArray(values) => Self::Append(convert(values)),
        }
    }
}

impl From<GluonBuildPolicyPatchSpec> for BuildPolicyPatchSpec {
    fn from(value: GluonBuildPolicyPatchSpec) -> Self {
        Self {
            build_subdir: value.build_subdir.into(),
            layout: value.layout.into(),
            toolchains: value.toolchains.into(),
            targets: value.targets.into(),
            retired_targets: value.retired_targets.into(),
            sandbox: value.sandbox.into(),
            build_root: value.build_root.into(),
            sources: value.sources.into(),
            tuning: value.tuning.into(),
            environment: value.environment.into(),
            builders: value.builders.into(),
            analyzers: value.analyzers.into(),
            pgo: value.pgo.into(),
        }
    }
}

impl From<GluonAnalyzerKind> for AnalyzerKind {
    fn from(value: GluonAnalyzerKind) -> Self {
        match value {
            GluonAnalyzerKind::AnalyzerIgnoreBlocked => Self::IgnoreBlocked,
            GluonAnalyzerKind::AnalyzerBinary => Self::Binary,
            GluonAnalyzerKind::AnalyzerElf => Self::Elf,
            GluonAnalyzerKind::AnalyzerPkgConfig => Self::PkgConfig,
            GluonAnalyzerKind::AnalyzerPython => Self::Python,
            GluonAnalyzerKind::AnalyzerCMake => Self::CMake,
            GluonAnalyzerKind::AnalyzerCompressMan => Self::CompressMan,
            GluonAnalyzerKind::AnalyzerIncludeAny => Self::IncludeAny,
        }
    }
}

impl From<GluonSandboxTmpPolicySpec> for SandboxTmpPolicySpec {
    fn from(value: GluonSandboxTmpPolicySpec) -> Self {
        match value {
            GluonSandboxTmpPolicySpec::EmptyTmp => Self::Empty,
        }
    }
}

impl From<GluonSandboxSysPolicySpec> for SandboxSysPolicySpec {
    fn from(value: GluonSandboxSysPolicySpec) -> Self {
        match value {
            GluonSandboxSysPolicySpec::NoSys => Self::None,
        }
    }
}

impl From<GluonSandboxDevPolicySpec> for SandboxDevPolicySpec {
    fn from(value: GluonSandboxDevPolicySpec) -> Self {
        match value {
            GluonSandboxDevPolicySpec::NoDev => Self::None,
            GluonSandboxDevPolicySpec::MinimalDev => Self::Minimal,
        }
    }
}

impl From<GluonSandboxCredentialPolicySpec> for SandboxCredentialPolicySpec {
    fn from(value: GluonSandboxCredentialPolicySpec) -> Self {
        match value {
            GluonSandboxCredentialPolicySpec::IsolatedRootCredentials => Self::IsolatedRoot,
        }
    }
}

impl From<GluonEnvironmentCondition> for EnvironmentCondition {
    fn from(value: GluonEnvironmentCondition) -> Self {
        match value {
            GluonEnvironmentCondition::Always => Self::Always,
            GluonEnvironmentCondition::CompilerCacheEnabled => Self::CompilerCacheEnabled,
            GluonEnvironmentCondition::CompilerCacheDisabled => Self::CompilerCacheDisabled,
        }
    }
}

impl From<GluonBuildToolSpec> for BuildToolSpec {
    fn from(value: GluonBuildToolSpec) -> Self {
        match value {
            GluonBuildToolSpec::Package { target } => Self::Package(target),
            GluonBuildToolSpec::Binary { target } => Self::Binary(target),
            GluonBuildToolSpec::SystemBinary { target } => Self::SystemBinary(target),
        }
    }
}

impl From<GluonTargetEmulationSpec> for TargetEmulationSpec {
    fn from(value: GluonTargetEmulationSpec) -> Self {
        match value {
            GluonTargetEmulationSpec::Native => Self::Native,
            GluonTargetEmulationSpec::Emul32 { host_architecture } => Self::Emul32 { host_architecture },
        }
    }
}
