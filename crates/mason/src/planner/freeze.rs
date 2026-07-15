use super::*;

pub(super) fn freeze_analysis(
    policy: &BuildPolicySpec,
    package: &PackageSpec,
    build_lock: &BuildLock,
) -> Result<AnalysisPlan, Error> {
    let selected = build::root::selected_analyzer_tools(policy, package);

    Ok(AnalysisPlan {
        handlers: policy.analyzers.clone(),
        tools: AnalysisToolsPlan {
            pkg_config: selected
                .pkg_config
                .map(|tool| freeze_analyzer_tool("analysis.tools.pkg_config", tool, build_lock))
                .transpose()?,
            python: selected
                .python
                .map(|tool| freeze_analyzer_tool("analysis.tools.python", tool, build_lock))
                .transpose()?,
            objcopy: selected
                .objcopy
                .map(|tool| freeze_analyzer_tool("analysis.tools.objcopy", tool, build_lock))
                .transpose()?,
            strip: selected
                .strip
                .map(|tool| freeze_analyzer_tool("analysis.tools.strip", tool, build_lock))
                .transpose()?,
        },
        debug: package.options.debug,
        strip: package.options.strip,
        compress_man: package.options.compressman,
        remove_libtool: package.options.lastrip,
    })
}

fn freeze_analyzer_tool(
    field: &'static str,
    tool: &BuildToolSpec,
    build_lock: &BuildLock,
) -> Result<ExecutablePlan, Error> {
    let dependency = tool
        .dependency()
        .map_err(|source| Error::InvalidAnalyzerTool { field, source })?;
    let requirement = RelationPlan::from(&dependency);
    let path = tool
        .executable_program()
        .ok_or(Error::AnalyzerToolNotExecutable { field })?;
    let request = requirement.canonical_name();
    if !build_lock.requests.iter().any(|locked| locked.request == request) {
        return Err(Error::UnlockedAnalyzerTool { field, request });
    }

    Ok(ExecutablePlan { path, requirement })
}

pub(super) fn freeze_toolchain_commands(
    policy: &BuildPolicySpec,
    toolchain: &stone_recipe::ToolchainSpec,
    compiler_cache: bool,
    mold: bool,
) -> ToolchainCommandsPlan {
    let tools = match toolchain {
        stone_recipe::ToolchainSpec::Llvm => &policy.toolchains.llvm,
        stone_recipe::ToolchainSpec::Gnu => &policy.toolchains.gnu,
    };
    ToolchainCommandsPlan {
        compilers: freeze_compiler_commands(tools),
        ccache: compiler_cache.then(|| build::context::freeze_policy_program(&policy.build_root.compiler_cache.ccache)),
        sccache: compiler_cache
            .then(|| build::context::freeze_policy_program(&policy.build_root.compiler_cache.sccache)),
        mold: mold.then(|| freeze_command(&policy.build_root.mold.linker)),
    }
}

fn freeze_compiler_commands(tools: &CompilerToolsSpec) -> Vec<CompilerCommandPlan> {
    [
        (CompilerExecutableRole::Cc, &tools.cc),
        (CompilerExecutableRole::Cxx, &tools.cxx),
        (CompilerExecutableRole::Objc, &tools.objc),
        (CompilerExecutableRole::Objcxx, &tools.objcxx),
        (CompilerExecutableRole::Cpp, &tools.cpp),
        (CompilerExecutableRole::Objcpp, &tools.objcpp),
        (CompilerExecutableRole::Objcxxcpp, &tools.objcxxcpp),
        (CompilerExecutableRole::Ar, &tools.ar),
        (CompilerExecutableRole::Ld, &tools.ld),
        (CompilerExecutableRole::Objcopy, &tools.objcopy),
        (CompilerExecutableRole::Nm, &tools.nm),
        (CompilerExecutableRole::Ranlib, &tools.ranlib),
        (CompilerExecutableRole::Strip, &tools.strip),
    ]
    .into_iter()
    .map(|(role, command)| CompilerCommandPlan {
        role,
        command: freeze_command(command),
    })
    .collect()
}

fn freeze_command(command: &BuildCommandSpec) -> ExecutableCommandPlan {
    ExecutableCommandPlan {
        program: build::context::freeze_policy_program(&command.program),
        args: command.args.clone(),
    }
}

pub(super) fn freeze_jobs(target: &build::Target) -> Result<Vec<JobPlan>, Error> {
    Ok(target
        .jobs
        .iter()
        .map(|job| JobPlan {
            pgo_stage: job.pgo_stage.map(|stage| format!("{stage:?}").to_lowercase()),
            pgo_dir: job.pgo_stage.map(|_| format!("{}-pgo", job.build_dir.display())),
            build_dir: job.build_dir.display().to_string(),
            work_dir: job.work_dir.display().to_string(),
            phases: job.phases.values().cloned().collect(),
        })
        .collect())
}

pub(super) fn jobs_use_package_directory(jobs: &[JobPlan], package_dir: &str) -> bool {
    jobs.iter()
        .flat_map(|job| &job.phases)
        .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
        .any(|step| match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => std::iter::once(program.path.as_str())
                .chain(args.iter().map(String::as_str))
                .chain(environment.values().map(String::as_str))
                .chain(std::iter::once(working_dir.as_str()))
                .any(|value| value.contains(package_dir)),
            StepPlan::Shell {
                interpreter,
                declared_programs,
                script,
                environment,
                working_dir,
            } => std::iter::once(interpreter.path.as_str())
                .chain(declared_programs.iter().map(|program| program.path.as_str()))
                .chain(std::iter::once(script.as_str()))
                .chain(environment.values().map(String::as_str))
                .chain(std::iter::once(working_dir.as_str()))
                .any(|value| value.contains(package_dir)),
            StepPlan::ExtractArchive { destination, .. } => destination.contains(package_dir),
        })
}

pub(super) fn freeze_outputs(
    root_name: &str,
    packages: &BTreeMap<String, ResolvedOutput>,
    lock: &BuildLock,
) -> Result<Vec<OutputPlan>, Error> {
    let names = packages
        .keys()
        .map(|name| (name.clone(), output_name(root_name, name)))
        .collect::<BTreeMap<_, _>>();
    packages
        .iter()
        .map(|(name, package)| {
            let runtime_inputs = package
                .runtime_inputs
                .iter()
                .map(|dependency| {
                    let request_name = dependency.to_name();
                    if let Some(output) = names.get(&request_name) {
                        Ok(OutputRelation::Planned { output: output.clone() })
                    } else if let Some(request) = lock.requests.iter().find(|request| request.request == request_name) {
                        Ok(OutputRelation::Locked {
                            relation: RelationPlan::from(dependency),
                            reference: LockedOutputRef {
                                package_id: request.package_id.clone(),
                                output: request.output.clone(),
                            },
                        })
                    } else {
                        Err(Error::UnlockedRuntimeDependency {
                            package: name.clone(),
                            dependency: request_name,
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(OutputPlan {
                name: names[name].clone(),
                package_name: name.clone(),
                include_in_manifest: package.include_in_manifest,
                summary: package.summary.clone(),
                description: package.description.clone(),
                provides_exclude: package.provides_exclude.clone(),
                runtime_exclude: package.runtime_exclude.clone(),
                runtime_inputs,
                conflicts: package.conflicts.iter().map(RelationPlan::from).collect(),
            })
        })
        .collect()
}

pub(super) fn output_name(root: &str, package: &str) -> String {
    if package == root {
        "out".to_owned()
    } else {
        package.strip_prefix(&format!("{root}-")).unwrap_or(package).to_owned()
    }
}

pub(super) fn freeze_sources(recipe: &crate::Recipe) -> Vec<LockedSource> {
    recipe
        .source_lock
        .as_ref()
        .map(|lock| {
            lock.sources
                .iter()
                .map(|source| match source {
                    SourceResolution::Archive(source) => LockedSource::Archive {
                        order: source.order,
                        url: source.url.clone(),
                        sha256: source.sha256.clone(),
                        filename: recipe
                            .declaration
                            .sources
                            .get(source.order as usize)
                            .and_then(|upstream| match upstream {
                                stone_recipe::UpstreamSpec::Archive { rename, .. } => rename.clone(),
                                stone_recipe::UpstreamSpec::Git { .. } => None,
                            })
                            .unwrap_or_else(|| {
                                url::Url::parse(&source.url)
                                    .map(|url| forge::util::uri_file_name(&url).to_owned())
                                    .unwrap_or_default()
                            }),
                    },
                    SourceResolution::Git(source) => LockedSource::Git {
                        order: source.order,
                        url: source.url.clone(),
                        requested_ref: source.requested_ref.clone(),
                        commit: source.commit.clone(),
                        materialization_sha256: source.materialization_sha256.clone(),
                        directory: recipe
                            .declaration
                            .sources
                            .get(source.order as usize)
                            .and_then(|upstream| match upstream {
                                stone_recipe::UpstreamSpec::Git { clone_dir, .. } => clone_dir.clone(),
                                stone_recipe::UpstreamSpec::Archive { .. } => None,
                            })
                            .unwrap_or_else(|| {
                                url::Url::parse(&source.url)
                                    .map(|url| forge::util::uri_file_name(&url).to_owned())
                                    .unwrap_or_default()
                            }),
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}
