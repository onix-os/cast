//! Lua adapter for the canonical build-lock (Phase L5).
//!
//! The build-lock domain types are engine-neutral and carry their own semantic
//! `validate`, so the Lua adapter decodes an authored manifest straight into
//! [`BuildLock`] via serde (using the tagged Lua encoding for the input-origin
//! and role variants) and runs the identical validation the Gluon codec runs.
//! Equivalent Gluon and Lua sources normalize to equal locks with intentionally
//! distinct evaluation identities.

use std::fmt::Write as _;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string};

use super::{
    AnalyzerRole, BuildLock, BuildLockValidationError, CompilerCacheRole, CompilerExecutableRole,
    InputOrigin, JobExecutableRole, JobStepSection, LockedIdentity, LockedOutput, LockedOutputRef,
    LockedPackage, LockedRequest, PackageInputSelection, Platform, RepositorySnapshot,
};

/// Emit a build lock as canonical, generated-marked Lua source that re-decodes
/// through this adapter into the same [`BuildLock`]. This is the lock's write
/// path — what a generated-slot authority switch writes when it converts a
/// `build.lock.glu` to `build.lock.lua`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_lua_lock(lock: &BuildLock) -> String {
    let mut output = String::from(GENERATED_LUA_MARKER);
    let _ = write!(
        output,
        "return {{\n\
         schema_version = {},\n\
         request_fingerprint = {},\n\
         repositories = {},\n\
         requests = {},\n\
         packages = {},\n\
         build_platform = {},\n\
         host_platform = {},\n\
         target_platform = {},\n\
         policy = {},\n\
         target = {},\n\
         profile = {},\n\
         toolchain = {},\n\
         builder = {},\n\
         }}\n",
        lock.schema_version,
        lua_string(&lock.request_fingerprint),
        seq(&lock.repositories, repository_snapshot),
        seq(&lock.requests, locked_request),
        seq(&lock.packages, locked_package),
        platform(&lock.build_platform),
        platform(&lock.host_platform),
        platform(&lock.target_platform),
        identity(&lock.policy),
        identity(&lock.target),
        identity(&lock.profile),
        identity(&lock.toolchain),
        identity(&lock.builder),
    );
    output
}

fn seq<T>(items: &[T], encode: impl Fn(&T) -> String) -> String {
    let body = items.iter().map(encode).collect::<Vec<_>>().join(", ");
    format!("{{ {body} }}")
}

fn platform(p: &Platform) -> String {
    format!(
        "{{ architecture = {}, vendor = {}, operating_system = {}, abi = {} }}",
        lua_string(&p.architecture),
        lua_string(&p.vendor),
        lua_string(&p.operating_system),
        lua_string(&p.abi),
    )
}

fn repository_snapshot(r: &RepositorySnapshot) -> String {
    format!(
        "{{ id = {}, index_uri = {}, snapshot = {} }}",
        lua_string(&r.id),
        lua_string(&r.index_uri),
        lua_string(&r.snapshot),
    )
}

fn identity(i: &LockedIdentity) -> String {
    format!("{{ name = {}, fingerprint = {} }}", lua_string(&i.name), lua_string(&i.fingerprint))
}

fn locked_output(o: &LockedOutput) -> String {
    format!("{{ name = {} }}", lua_string(&o.name))
}

fn locked_output_ref(o: &LockedOutputRef) -> String {
    format!("{{ package_id = {}, output = {} }}", lua_string(&o.package_id), lua_string(&o.output))
}

fn locked_package(p: &LockedPackage) -> String {
    format!(
        "{{ package_id = {}, name = {}, version = {}, architecture = {}, repository = {}, outputs = {}, dependencies = {} }}",
        lua_string(&p.package_id),
        lua_string(&p.name),
        lua_string(&p.version),
        lua_string(&p.architecture),
        lua_string(&p.repository),
        seq(&p.outputs, locked_output),
        seq(&p.dependencies, locked_output_ref),
    )
}

fn locked_request(r: &LockedRequest) -> String {
    format!(
        "{{ request = {}, package_id = {}, output = {}, origins = {} }}",
        lua_string(&r.request),
        lua_string(&r.package_id),
        lua_string(&r.output),
        seq(&r.origins, input_origin),
    )
}

fn selection(s: &PackageInputSelection) -> String {
    match s {
        PackageInputSelection::Package => r#"{ kind = "package" }"#.to_owned(),
        PackageInputSelection::Profile { name } => {
            format!(r#"{{ kind = "profile", name = {} }}"#, lua_string(name))
        }
    }
}

fn job_step_section(s: &JobStepSection) -> &'static str {
    match s {
        JobStepSection::Pre => r#"{ kind = "pre" }"#,
        JobStepSection::Steps => r#"{ kind = "steps" }"#,
        JobStepSection::Post => r#"{ kind = "post" }"#,
    }
}

fn job_executable_role(r: &JobExecutableRole) -> String {
    match r {
        JobExecutableRole::RunProgram => r#"{ kind = "run_program" }"#.to_owned(),
        JobExecutableRole::ShellInterpreter => r#"{ kind = "shell_interpreter" }"#.to_owned(),
        JobExecutableRole::ShellDeclaredProgram { index } => {
            format!(r#"{{ kind = "shell_declared_program", index = {index} }}"#)
        }
    }
}

fn analyzer_role(r: &AnalyzerRole) -> &'static str {
    match r {
        AnalyzerRole::PkgConfig => r#"{ kind = "pkg_config" }"#,
        AnalyzerRole::Python => r#"{ kind = "python" }"#,
        AnalyzerRole::Objcopy => r#"{ kind = "objcopy" }"#,
        AnalyzerRole::Strip => r#"{ kind = "strip" }"#,
    }
}

fn compiler_executable_role(r: &CompilerExecutableRole) -> &'static str {
    match r {
        CompilerExecutableRole::Cc => r#"{ kind = "cc" }"#,
        CompilerExecutableRole::Cxx => r#"{ kind = "cxx" }"#,
        CompilerExecutableRole::Objc => r#"{ kind = "objc" }"#,
        CompilerExecutableRole::Objcxx => r#"{ kind = "objcxx" }"#,
        CompilerExecutableRole::Cpp => r#"{ kind = "cpp" }"#,
        CompilerExecutableRole::Objcpp => r#"{ kind = "objcpp" }"#,
        CompilerExecutableRole::Objcxxcpp => r#"{ kind = "objcxxcpp" }"#,
        CompilerExecutableRole::Ar => r#"{ kind = "ar" }"#,
        CompilerExecutableRole::Ld => r#"{ kind = "ld" }"#,
        CompilerExecutableRole::Objcopy => r#"{ kind = "objcopy" }"#,
        CompilerExecutableRole::Nm => r#"{ kind = "nm" }"#,
        CompilerExecutableRole::Ranlib => r#"{ kind = "ranlib" }"#,
        CompilerExecutableRole::Strip => r#"{ kind = "strip" }"#,
    }
}

fn compiler_cache_role(r: &CompilerCacheRole) -> &'static str {
    match r {
        CompilerCacheRole::Ccache => r#"{ kind = "ccache" }"#,
        CompilerCacheRole::Sccache => r#"{ kind = "sccache" }"#,
    }
}

fn input_origin(origin: &InputOrigin) -> String {
    match origin {
        InputOrigin::BuilderTool { selection: s, index } => {
            format!(r#"{{ kind = "builder_tool", selection = {}, index = {index} }}"#, selection(s))
        }
        InputOrigin::NativeBuild { selection: s, index } => {
            format!(r#"{{ kind = "native_build", selection = {}, index = {index} }}"#, selection(s))
        }
        InputOrigin::Build { selection: s, index } => {
            format!(r#"{{ kind = "build", selection = {}, index = {index} }}"#, selection(s))
        }
        InputOrigin::Check { selection: s, index } => {
            format!(r#"{{ kind = "check", selection = {}, index = {index} }}"#, selection(s))
        }
        InputOrigin::OutputRuntime { output, index } => {
            format!(r#"{{ kind = "output_runtime", output = {}, index = {index} }}"#, lua_string(output))
        }
        InputOrigin::Policy { source, field, index } => format!(
            r#"{{ kind = "policy", source = {}, field = {}, index = {index} }}"#,
            lua_string(source),
            lua_string(field),
        ),
        InputOrigin::JobExecutable { job, phase, phase_name, section, step, role } => format!(
            r#"{{ kind = "job_executable", job = {job}, phase = {phase}, phase_name = {}, section = {}, step = {step}, role = {} }}"#,
            lua_string(phase_name),
            job_step_section(section),
            job_executable_role(role),
        ),
        InputOrigin::Analyzer { role } => format!(r#"{{ kind = "analyzer", role = {} }}"#, analyzer_role(role)),
        InputOrigin::CompilerExecutable { role } => {
            format!(r#"{{ kind = "compiler_executable", role = {} }}"#, compiler_executable_role(role))
        }
        InputOrigin::CompilerCache { role } => {
            format!(r#"{{ kind = "compiler_cache", role = {} }}"#, compiler_cache_role(role))
        }
        InputOrigin::MoldLinker => r#"{ kind = "mold_linker" }"#.to_owned(),
    }
}

/// Stateful Lua adapter for the canonical build-lock declaration.
///
/// Proven by the parity tests below; the `.lua` build-lock loader wiring that
/// constructs it in production is a later slice.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub struct LuaBuildLockCodec {
    engine: LuaEngine,
}

impl DeclarationEvaluator<BuildLock> for LuaBuildLockCodec {
    type Identity = EvaluationIdentity;
    type Error = BuildLockValidationError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<BuildLock, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<BuildLock>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let lock = evaluation.value;
        lock.validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: lock,
            identity: evaluation.identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::super::sample_lock;
    use super::*;

    // The Lua declaration profile forbids helper functions and locals, so the
    // platform and identity records are written inline.
    const LUA_LOCK: &str = r#"
return {
    schema_version = 6,
    request_fingerprint = "request-fingerprint",
    repositories = {
        {
            id = "volatile",
            index_uri = "https://example.invalid/stone.index",
            snapshot = "repository-snapshot",
        },
    },
    requests = {
        {
            request = "binary(hello)",
            package_id = "hello-id",
            output = "out",
            origins = {
                {
                    kind = "builder_tool",
                    selection = { kind = "package" },
                    index = 0,
                },
            },
        },
    },
    packages = {
        {
            package_id = "cmake-id",
            name = "cmake",
            version = "3.31.0-1",
            architecture = "x86_64",
            repository = "volatile",
            outputs = { { name = "out" } },
            dependencies = {},
        },
        {
            package_id = "hello-id",
            name = "hello",
            version = "1.0.0-1",
            architecture = "x86_64",
            repository = "volatile",
            outputs = { { name = "out" } },
            dependencies = { { package_id = "cmake-id", output = "out" } },
        },
    },
    build_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    host_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    target_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    policy = { name = "aerynos", fingerprint = "policy-fingerprint" },
    target = { name = "x86_64", fingerprint = "target-fingerprint" },
    profile = { name = "default-x86_64", fingerprint = "profile-fingerprint" },
    toolchain = { name = "llvm", fingerprint = "toolchain-fingerprint" },
    builder = { name = "cmake", fingerprint = "builder-fingerprint" },
}
"#;

    #[test]
    fn a_lua_build_lock_normalizes_to_the_same_value_as_the_domain_sample() {
        let decoded = LuaBuildLockCodec::default()
            .evaluate(&Source::new("build.lock.lua", LUA_LOCK))
            .expect("lua build lock evaluates")
            .value;

        assert_eq!(decoded, sample_lock());
    }

    #[test]
    fn an_emitted_build_lock_re_decodes_to_the_same_value() {
        let original = sample_lock();
        let emitted = encode_lua_lock(&original);
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let round_tripped = LuaBuildLockCodec::default()
            .evaluate(&Source::new("build.lock.lua", &emitted))
            .expect("emitted build lock re-decodes")
            .value;
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn the_lua_build_lock_identity_is_the_lua_engine() {
        let identity = LuaBuildLockCodec::default()
            .evaluate(&Source::new("build.lock.lua", LUA_LOCK))
            .expect("lua build lock evaluates")
            .identity;
        assert_eq!(identity.engine.implementation(), "lua");
    }
}
