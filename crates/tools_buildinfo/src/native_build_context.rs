//! Normalization of native compiler, linker, and dependency build inputs.
//!
//! Cargo exposes these values to every native dependency build script, but it
//! does not include them in the repository source tree.  Keep the selection
//! rules here pure so the build script and regression tests use the same
//! canonical representation.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::OsStrExt as _,
    path::Path,
};

use crate::tool_identity::{CommandIdentity, normalize_workspace};

pub(crate) type ExplicitInput = (String, Vec<u8>);

const TARGET_SCOPED_VALUES: &[&str] = &[
    "ARFLAGS",
    "CFLAGS",
    "CPPFLAGS",
    "CXXFLAGS",
    "CXXSTDLIB",
    "LDFLAGS",
    "PKG_CONFIG_ALLOW_CROSS",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_SYSROOT_DIR",
    "RANLIBFLAGS",
];

const EXACT_VALUES: &[&str] = &[
    "BINDGEN_EXTRA_CLANG_ARGS",
    "CC_FORCE_DISABLE",
    "CC_KNOWN_WRAPPER_CUSTOM",
    "CC_SHELL_ESCAPED_FLAGS",
    "CLANG_PATH",
    "COMPILER_PATH",
    "CPATH",
    "CPLUS_INCLUDE_PATH",
    "C_INCLUDE_PATH",
    "CMAKE_GENERATOR",
    "CMAKE_GENERATOR_INSTANCE",
    "CMAKE_GENERATOR_PLATFORM",
    "CMAKE_GENERATOR_TOOLSET",
    "CMAKE_INCLUDE_PATH",
    "CMAKE_LIBRARY_PATH",
    "CMAKE_PREFIX_PATH",
    "CMAKE_TOOLCHAIN_FILE",
    "CRATE_CC_NO_DEFAULTS",
    "CROSS_COMPILE",
    "LIBCLANG_PATH",
    "LIBRARY_PATH",
    "LD_RUN_PATH",
    "NIX_BINTOOLS",
    "NIX_BINTOOLS_FOR_BUILD",
    "NIX_CC",
    "NIX_CC_FOR_BUILD",
    "NIX_CFLAGS_COMPILE",
    "NIX_CFLAGS_COMPILE_FOR_BUILD",
    "NIX_CFLAGS_LINK",
    "NIX_CFLAGS_LINK_FOR_BUILD",
    "NIX_HARDENING_DISABLE",
    "NIX_HARDENING_ENABLE",
    "NIX_LDFLAGS",
    "NIX_LDFLAGS_FOR_BUILD",
    "PKG_CONFIG_ALL_DYNAMIC",
    "PKG_CONFIG_ALL_STATIC",
    "PKG_CONFIG_ALLOW_SYSTEM_CFLAGS",
    "PKG_CONFIG_ALLOW_SYSTEM_LIBS",
    "SDKROOT",
    "SYSROOT",
    "WASI_SDK_PATH",
    "WASI_SYSROOT",
    "WASM_MUSL_SYSROOT",
];

// These are the knobs read by native dependencies in the Mason/Stone
// closure.  Restricting this list avoids turning the complete ambient process
// environment into implementation identity.
const DEPENDENCY_VALUES: &[&str] = &[
    "AWS_LC_SYS_C_STD",
    "AWS_LC_SYS_CC",
    "AWS_LC_SYS_CFLAGS",
    "AWS_LC_SYS_CMAKE",
    "AWS_LC_SYS_CMAKE_BUILDER",
    "AWS_LC_SYS_CMAKE_GENERATOR",
    "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE",
    "AWS_LC_SYS_CXX",
    "AWS_LC_SYS_EFFECTIVE_TARGET",
    "AWS_LC_SYS_EXTERNAL_BINDGEN",
    "AWS_LC_SYS_INCLUDES",
    "AWS_LC_SYS_NO_ASM",
    "AWS_LC_SYS_NO_JITTER_ENTROPY",
    "AWS_LC_SYS_NO_PREFIX",
    "AWS_LC_SYS_NO_PREGENERATED_SRC",
    "AWS_LC_SYS_NO_U1_BINDINGS",
    "AWS_LC_SYS_PREBUILT_NASM",
    "AWS_LC_SYS_PREGENERATING_BINDINGS",
    "AWS_LC_SYS_SANITIZER",
    "AWS_LC_SYS_STATIC",
    "AWS_LC_SYS_TARGET_CC",
    "AWS_LC_SYS_TARGET_CFLAGS",
    "AWS_LC_SYS_TARGET_CXX",
    "BLAKE3_CI",
    "LIBSQLITE3_FLAGS",
    "LIBSQLITE3_SYS_BUNDLING",
    "LIBSQLITE3_SYS_USE_PKG_CONFIG",
    "LIBZSTD_DYNAMIC",
    "LIBZSTD_NO_PKG_CONFIG",
    "LIBZSTD_STATIC",
    "SQLITE3_DYNAMIC",
    "SQLITE3_DLL_NAME",
    "SQLITE3_INCLUDE_DIR",
    "SQLITE3_LIB_DIR",
    "SQLITE3_LIB_NAME",
    "SQLITE3_LINK_LIB",
    "SQLITE3_NO_PKG_CONFIG",
    "SQLITE3_STATIC",
    "SQLITE_MAX_COLUMN",
    "SQLITE_MAX_EXPR_DEPTH",
    "SQLITE_MAX_VARIABLE_NUMBER",
    "ZSTD_SYS_USE_PKG_CONFIG",
];

const TARGET_SCOPED_TOOLS: &[ToolSpec] = &[
    ToolSpec::targeted("cc", "CC", "cc"),
    ToolSpec::targeted("cxx", "CXX", "c++"),
    ToolSpec::targeted("archiver", "AR", "ar"),
    ToolSpec::targeted("ranlib", "RANLIB", "ranlib"),
    ToolSpec::targeted("linker", "LD", "ld"),
    ToolSpec::targeted("assembler", "AS", "as"),
    ToolSpec::targeted("symbol-reader", "NM", "nm"),
];

const EXACT_TOOLS: &[ToolSpec] = &[ToolSpec::exact("rust-linker", "RUSTC_LINKER", "cc")];

// aws-lc-sys first applies crate-specific TARGET_CC/TARGET_CXX and CC/CXX
// overrides, then writes the result back into cc-rs' target-specific
// environment.  These selectors can therefore choose a different compiler
// from the generic cc-rs selector above and need their own byte identity.
const AWS_COMPILER_TOOLS: &[ToolSpec] = &[
    ToolSpec::aws_compiler("aws-lc-cc", "CC", "cc"),
    ToolSpec::aws_compiler("aws-lc-cxx", "CXX", "c++"),
];

#[derive(Clone, Copy)]
struct ToolSpec {
    role: &'static str,
    variable: &'static str,
    default_program: &'static str,
    selection: ToolSelection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolSelection {
    Targeted,
    Exact,
    AwsCompiler,
}

impl ToolSpec {
    const fn targeted(role: &'static str, variable: &'static str, default_program: &'static str) -> Self {
        Self {
            role,
            variable,
            default_program,
            selection: ToolSelection::Targeted,
        }
    }

    const fn exact(role: &'static str, variable: &'static str, default_program: &'static str) -> Self {
        Self {
            role,
            variable,
            default_program,
            selection: ToolSelection::Exact,
        }
    }

    const fn aws_compiler(role: &'static str, variable: &'static str, default_program: &'static str) -> Self {
        Self {
            role,
            variable,
            default_program,
            selection: ToolSelection::AwsCompiler,
        }
    }
}

#[derive(Debug)]
pub(crate) struct NativeBuildContext {
    inputs: Vec<ExplicitInput>,
    watched_environment: Vec<String>,
}

impl NativeBuildContext {
    pub(crate) fn inputs(self) -> Vec<ExplicitInput> {
        self.inputs
    }

    pub(crate) fn watched_environment(&self) -> &[String] {
        &self.watched_environment
    }
}

/// Collect the effective native build context from an explicit environment.
///
/// `probe` receives the tool role and selected command and returns its
/// content-strong executable identity, including stable `--version` output.
/// Every selected command must return an identity; otherwise two different
/// implementations could share one Mason semantic fingerprint.
pub(crate) fn collect<I, F>(environment: I, workspace_root: &Path, mut probe: F) -> io::Result<NativeBuildContext>
where
    I: IntoIterator<Item = (OsString, OsString)>,
    F: FnMut(&str, &[OsString]) -> io::Result<Option<CommandIdentity>>,
{
    let environment = normalize_environment(environment)?;
    let host = required_utf8(&environment, "HOST")?;
    let target = required_utf8(&environment, "TARGET")?;
    let target_os = required_utf8(&environment, "CARGO_CFG_TARGET_OS")?;
    let target_env = required_utf8(&environment, "CARGO_CFG_TARGET_ENV")?;
    let kind = if host == target { "HOST" } else { "TARGET" };
    let target_underscored = target.replace(['-', '.'], "_");
    let workspace = workspace_root.as_os_str().as_bytes();

    if target_os != "linux" {
        return Err(invalid_data(format!(
            "unsupported native target OS {target_os:?} for {target}: exact native-tool identity is currently Linux-only"
        )));
    }
    validate_cross_tool_selection(&environment, host, target, kind, &target_underscored)?;
    validate_aws_lc_external_tools(&environment, target)?;
    validate_aws_lc_builder_selection(&environment, target, target_env)?;
    validate_cmake_context(&environment, kind, target)?;
    validate_external_native_libraries(&environment)?;

    let mut watched = watched_environment(kind, target, &target_underscored);
    let mut inputs = BTreeMap::<String, Vec<u8>>::new();

    for key in &watched {
        if matches!(
            key.as_str(),
            "HOST" | "TARGET" | "CARGO_CFG_TARGET_OS" | "CARGO_CFG_TARGET_ENV"
        ) {
            continue;
        }
        if is_tool_selector(key, kind, target, &target_underscored) {
            continue;
        }
        if let Some(value) = environment.get(key) {
            inputs.insert(
                format!("native.env.{key}"),
                normalize_workspace(value.as_bytes(), workspace),
            );
        }
    }

    for spec in TARGET_SCOPED_TOOLS.iter().chain(EXACT_TOOLS) {
        let selector = select_tool(&environment, *spec, kind, target, &target_underscored);
        let selector = selector.unwrap_or_else(|| OsString::from(spec.default_program));
        let command = command_for_tool(*spec, selector)?;
        let identity = require_tool_identity(spec.role, &command, probe(spec.role, &command)?)?;
        let encoded_command = encode_command(&command, workspace);
        inputs.insert(format!("native.tool.{}.command", spec.role), encoded_command);
        inputs.insert(
            format!("native.tool.{}.identity", spec.role),
            identity.encode(workspace_root),
        );
    }

    for spec in AWS_COMPILER_TOOLS {
        let Some(selector) = select_tool(&environment, *spec, kind, target, &target_underscored) else {
            // Without an AWS-specific override, aws-lc-sys delegates compiler
            // selection to cc-rs.  The generic cc/cxx identities above are
            // therefore the complete effective identity.
            continue;
        };
        let command = command_for_tool(*spec, selector)?;
        let identity = require_tool_identity(spec.role, &command, probe(spec.role, &command)?)?;
        inputs.insert(
            format!("native.tool.{}.command", spec.role),
            encode_command(&command, workspace),
        );
        inputs.insert(
            format!("native.tool.{}.identity", spec.role),
            identity.encode(workspace_root),
        );
    }

    // Cargo normally resolves a target linker into RUSTC_LINKER.  Retain the
    // explicit target form as a watched, canonical fallback for Cargo
    // frontends which expose only their configuration environment variable.
    let cargo_linker = format!("CARGO_TARGET_{}_LINKER", target_underscored.to_ascii_uppercase());
    watched.insert(cargo_linker.clone());
    if !environment.contains_key("RUSTC_LINKER")
        && let Some(selector) = environment.get(&cargo_linker)
    {
        inputs.remove("native.tool.rust-linker.identity");
        let command = direct_command("rust-linker", selector.clone())?;
        inputs.insert(
            "native.tool.rust-linker.command".to_owned(),
            encode_command(&command, workspace),
        );
        let identity = require_tool_identity("rust-linker", &command, probe("rust-linker", &command)?)?;
        inputs.insert(
            "native.tool.rust-linker.identity".to_owned(),
            identity.encode(workspace_root),
        );
    }

    Ok(NativeBuildContext {
        inputs: inputs.into_iter().collect(),
        watched_environment: watched.into_iter().collect(),
    })
}

fn require_tool_identity(
    role: &str,
    command: &[OsString],
    identity: Option<CommandIdentity>,
) -> io::Result<CommandIdentity> {
    identity.ok_or_else(|| {
        invalid_data(format!(
            "cannot identify selected native tool {role} with command {command:?}"
        ))
    })
}

fn normalize_environment<I>(environment: I) -> io::Result<BTreeMap<String, OsString>>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut normalized = BTreeMap::new();
    for (key, value) in environment {
        let Ok(key) = key.into_string() else {
            // Every relevant name is ASCII.  Non-UTF-8 ambient names cannot
            // select a native build input and are intentionally ignored.
            continue;
        };
        if normalized.insert(key.clone(), value).is_some() {
            return Err(invalid_data(format!("duplicate native build environment input: {key}")));
        }
    }
    Ok(normalized)
}

fn required_utf8<'a>(environment: &'a BTreeMap<String, OsString>, key: &str) -> io::Result<&'a str> {
    environment
        .get(key)
        .ok_or_else(|| invalid_data(format!("missing required native build environment input: {key}")))?
        .to_str()
        .ok_or_else(|| invalid_data(format!("native build environment value is not UTF-8: {key}")))
}

fn validate_cross_tool_selection(
    environment: &BTreeMap<String, OsString>,
    host: &str,
    target: &str,
    kind: &str,
    target_underscored: &str,
) -> io::Result<()> {
    if host == target {
        return Ok(());
    }

    // cc-rs may derive `${CROSS_COMPILE}gcc`, `${CROSS_COMPILE}g++`, and
    // multiple archive-tool candidates.  Reproducing that open-ended PATH
    // discovery here would be brittle.  Cross builds instead fail closed
    // unless every compiler/archive role has an explicit cc-rs selector which
    // the normal collection loop can content-identify exactly.
    let required = &TARGET_SCOPED_TOOLS[..4];
    let missing = required
        .iter()
        .filter(|spec| select_tool(environment, **spec, kind, target, target_underscored).is_none())
        .map(|spec| spec.variable)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(invalid_data(format!(
        "cross build {host} -> {target} requires explicit target tool selectors for {}; implicit CROSS_COMPILE/PATH discovery cannot be content-identified",
        missing.join(", ")
    )))
}

fn validate_aws_lc_external_tools(environment: &BTreeMap<String, OsString>, target: &str) -> io::Result<()> {
    // The locked aws-lc-sys 0.40 dependency is selected through aws-lc-rs
    // with default features disabled.  Its only enabled feature is
    // `prebuilt-nasm`, so it has universal pregenerated bindings but no
    // in-process bindgen implementation.  The binding controls below bypass
    // those pregenerated bindings and fall back to a PATH-selected `bindgen`
    // CLI (which in turn selects rustfmt and libclang).  NO_PREGENERATED_SRC
    // separately enables AWS-LC's external Go/Perl source generation and also
    // forces its CMake builder.  Neither open toolchain can be represented by
    // the identities collected here, so reject them before assigning a build
    // an incomplete fingerprint.
    let target_underscored = target.replace('-', "_");
    for control in [
        "EXTERNAL_BINDGEN",
        "NO_PREFIX",
        "PREGENERATING_BINDINGS",
        "NO_PREGENERATED_SRC",
    ] {
        if aws_lc_bool(environment, control, &target_underscored) == Some(true) {
            let external_tools = if control == "NO_PREGENERATED_SRC" {
                "Go/Perl source-generation toolchain"
            } else {
                "external bindgen toolchain"
            };
            return Err(invalid_data(format!(
                "unsupported aws-lc-sys native context for {target}: effective AWS_LC_SYS_{control} enables an unidentifiable {external_tools}"
            )));
        }
    }

    Ok(())
}

fn validate_aws_lc_builder_selection(
    environment: &BTreeMap<String, OsString>,
    target: &str,
    target_env: &str,
) -> io::Result<()> {
    let target_underscored = target.replace('-', "_");
    let cmake_builder = aws_lc_bool(environment, "CMAKE_BUILDER", &target_underscored);
    if cmake_builder == Some(true) {
        return Err(unsupported_aws_lc_cmake(
            target,
            "effective AWS_LC_SYS_CMAKE_BUILDER=true",
        ));
    }

    if aws_lc_bool(environment, "STATIC", &target_underscored) == Some(false) {
        return Err(unsupported_aws_lc_cmake(target, "effective AWS_LC_SYS_STATIC=false"));
    }
    if target_env == "ohos" {
        return Err(unsupported_aws_lc_cmake(target, "CARGO_CFG_TARGET_ENV=ohos"));
    }

    // An explicit false selector is the upstream escape hatch which chooses
    // the static cc-rs builder before the NO_ASM/sanitizer CMake fallback.
    if cmake_builder != Some(false) {
        if aws_lc_bool(environment, "NO_ASM", &target_underscored) == Some(true) {
            return Err(unsupported_aws_lc_cmake(target, "effective AWS_LC_SYS_NO_ASM=true"));
        }
        if aws_lc_value(environment, "SANITIZER", &target_underscored).is_some() {
            return Err(unsupported_aws_lc_cmake(
                target,
                "effective AWS_LC_SYS_SANITIZER is present",
            ));
        }
    }

    Ok(())
}

fn unsupported_aws_lc_cmake(target: &str, reason: &str) -> io::Error {
    invalid_data(format!(
        "unsupported aws-lc-sys CMake builder lane for {target}: {reason}; the locked exact-tool contract requires the static cc-rs builder"
    ))
}

fn validate_cmake_context(environment: &BTreeMap<String, OsString>, kind: &str, target: &str) -> io::Result<()> {
    for key in unsupported_cmake_context_keys(kind, target) {
        if environment.get(&key).is_some_and(|value| !value.is_empty()) {
            return Err(invalid_data(format!(
                "unsupported CMake native context: {key} is outside the locked static cc-rs builder contract"
            )));
        }
    }
    Ok(())
}

fn validate_external_native_libraries(environment: &BTreeMap<String, OsString>) -> io::Result<()> {
    // The locked zstd-sys and libsqlite3-sys feature graph builds vendored
    // sources. Their environment escape hatches instead select arbitrary
    // pkg-config headers and libraries whose bytes are not represented by
    // their path strings; identifying pkg-config itself would not bind those
    // selected bytes either. Reject the external-library lanes instead.
    if environment.contains_key("ZSTD_SYS_USE_PKG_CONFIG") {
        return Err(invalid_data(
            "unsupported external zstd native library: ZSTD_SYS_USE_PKG_CONFIG must be absent",
        ));
    }
    if let Some(value) = environment.get("LIBSQLITE3_SYS_USE_PKG_CONFIG")
        && value != "0"
    {
        return Err(invalid_data(
            "unsupported external SQLite native library: LIBSQLITE3_SYS_USE_PKG_CONFIG must be absent or exactly 0",
        ));
    }
    Ok(())
}

fn aws_lc_bool(environment: &BTreeMap<String, OsString>, control: &str, target_underscored: &str) -> Option<bool> {
    let value = aws_lc_value(environment, control, target_underscored)?;
    let value = value.to_str()?.to_lowercase();

    if value.starts_with('0') || value.starts_with('n') || value.starts_with("off") || value.starts_with('f') {
        Some(false)
    } else if value.starts_with(|character: char| character.is_ascii_digit())
        || value.starts_with('y')
        || value.starts_with("on")
        || value.starts_with('t')
    {
        Some(true)
    } else {
        None
    }
}

fn aws_lc_value<'a>(
    environment: &'a BTreeMap<String, OsString>,
    control: &str,
    target_underscored: &str,
) -> Option<&'a OsStr> {
    environment
        .get(&format!("AWS_LC_SYS_{control}_{target_underscored}"))
        .or_else(|| environment.get(&format!("AWS_LC_SYS_{control}")))
        .map(OsString::as_os_str)
}

fn watched_environment(kind: &str, target: &str, target_underscored: &str) -> BTreeSet<String> {
    let mut watched = BTreeSet::from([
        "HOST".to_owned(),
        "TARGET".to_owned(),
        "CARGO_CFG_TARGET_OS".to_owned(),
        "CARGO_CFG_TARGET_ENV".to_owned(),
    ]);
    watched.extend(EXACT_VALUES.iter().map(|key| (*key).to_owned()));
    watched.extend(DEPENDENCY_VALUES.iter().map(|key| (*key).to_owned()));

    for base in TARGET_SCOPED_VALUES {
        watched.extend(targeted_keys(base, kind, target, target_underscored));
    }
    for spec in TARGET_SCOPED_TOOLS {
        watched.extend(tool_keys(*spec, kind, target, target_underscored));
    }
    for spec in EXACT_TOOLS {
        watched.extend(tool_keys(*spec, kind, target, target_underscored));
    }
    for spec in AWS_COMPILER_TOOLS {
        watched.extend(tool_keys(*spec, kind, target, target_underscored));
    }
    watched.extend(unsupported_cmake_context_keys(kind, target));

    // libsqlite3-sys supports a target-prefix form in addition to the cc-rs
    // suffix form.  AWS-LC accepts crate-specific target suffixes.
    let upper_target = target_underscored.to_ascii_uppercase();
    for key in DEPENDENCY_VALUES {
        watched.insert(format!("{upper_target}_{key}"));
        if let Some(rest) = key.strip_prefix("AWS_LC_SYS_") {
            watched.insert(format!("AWS_LC_SYS_{rest}_{target_underscored}"));
        }
    }

    watched
}

fn targeted_keys(base: &str, kind: &str, target: &str, target_underscored: &str) -> [String; 4] {
    [
        format!("{base}_{target}"),
        format!("{base}_{target_underscored}"),
        format!("{kind}_{base}"),
        base.to_owned(),
    ]
}

fn select_tool(
    environment: &BTreeMap<String, OsString>,
    spec: ToolSpec,
    kind: &str,
    target: &str,
    target_underscored: &str,
) -> Option<OsString> {
    for key in tool_keys(spec, kind, target, target_underscored) {
        if let Some(value) = environment.get(&key) {
            return Some(value.clone());
        }
    }
    None
}

fn is_tool_selector(key: &str, kind: &str, target: &str, target_underscored: &str) -> bool {
    TARGET_SCOPED_TOOLS
        .iter()
        .chain(EXACT_TOOLS)
        .chain(AWS_COMPILER_TOOLS)
        .any(|spec| {
            tool_keys(*spec, kind, target, target_underscored)
                .iter()
                .any(|candidate| candidate == key)
        })
}

fn tool_keys(spec: ToolSpec, kind: &str, target: &str, target_underscored: &str) -> Vec<String> {
    match spec.selection {
        ToolSelection::Targeted => targeted_keys(spec.variable, kind, target, target_underscored).into(),
        ToolSelection::Exact => vec![spec.variable.to_owned()],
        ToolSelection::AwsCompiler => vec![
            format!("AWS_LC_SYS_TARGET_{}_{target_underscored}", spec.variable),
            format!("AWS_LC_SYS_TARGET_{}", spec.variable),
            format!("TARGET_{}_{target_underscored}", spec.variable),
            format!("TARGET_{}", spec.variable),
            format!("AWS_LC_SYS_{}_{target_underscored}", spec.variable),
            format!("AWS_LC_SYS_{}", spec.variable),
            format!("{}_{target_underscored}", spec.variable),
            spec.variable.to_owned(),
        ],
    }
}

fn unsupported_cmake_context_keys(kind: &str, target: &str) -> Vec<String> {
    let target_underscored = target.replace('-', "_");
    let mut keys = vec![
        format!("AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE_{target_underscored}"),
        "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE".to_owned(),
    ];
    for base in [
        "CMAKE_TOOLCHAIN_FILE",
        "CMAKE_GENERATOR_PLATFORM",
        "CMAKE_GENERATOR_TOOLSET",
        "CMAKE_GENERATOR_INSTANCE",
    ] {
        keys.extend([
            format!("{base}_{target}"),
            format!("{base}_{target_underscored}"),
            format!("{kind}_{base}"),
            base.to_owned(),
        ]);
    }
    keys
}

/// Return the compiler delegated to by a wrapper syntax understood by cc-rs.
///
/// cc-rs treats these wrappers specially for the `CC` and `CXX` selectors and
/// executes both the wrapper and the following compiler.  Keep that delegated
/// executable visible to the build identity rather than relying on its version
/// output through the wrapper.
pub(crate) fn delegated_compiler<'a>(
    role: &str,
    command: &'a [OsString],
    custom_wrapper: Option<&OsStr>,
) -> Option<&'a OsStr> {
    if !matches!(role, "cc" | "cxx" | "aws-lc-cc" | "aws-lc-cxx") {
        return None;
    }
    let wrapper = Path::new(command.first()?).file_stem()?.to_str()?;
    let known = ["ccache", "distcc", "sccache", "icecc", "cachepot", "buildcache"];
    if known.contains(&wrapper) || custom_wrapper.and_then(OsStr::to_str) == Some(wrapper) {
        command.get(1).map(OsString::as_os_str)
    } else {
        None
    }
}

fn command_for_tool(spec: ToolSpec, selector: OsString) -> io::Result<Vec<OsString>> {
    if matches!(spec.selection, ToolSelection::AwsCompiler) || matches!(spec.role, "cc" | "cxx" | "archiver" | "ranlib")
    {
        cc_rs_command(spec.role, selector.as_os_str())
    } else {
        direct_command(spec.role, selector)
    }
}

/// Reproduce cc-rs' environment-tool parsing rather than shell parsing.
///
/// An existing exact path is one executable even when it contains spaces.
/// Otherwise cc-rs trims and splits on whitespace without interpreting quotes
/// or escapes.  Mirroring that distinction is required to hash the executable
/// which cc-rs actually runs.
fn cc_rs_command(role: &str, selector: &OsStr) -> io::Result<Vec<OsString>> {
    let selector = selector.to_string_lossy();
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(invalid_data(format!("selected native tool {role} is empty")));
    }
    if Path::new(selector).exists() {
        return Ok(vec![OsString::from(selector)]);
    }

    let command = selector.split_whitespace().map(OsString::from).collect::<Vec<_>>();
    if command.is_empty() {
        Err(invalid_data(format!("selected native tool {role} is empty")))
    } else {
        Ok(command)
    }
}

fn direct_command(role: &str, selector: OsString) -> io::Result<Vec<OsString>> {
    if selector.is_empty() {
        Err(invalid_data(format!("selected native tool {role} is empty")))
    } else {
        Ok(vec![selector])
    }
}

fn encode_command(command: &[OsString], workspace: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    write_count(&mut encoded, command.len());
    for (index, argument) in command.iter().enumerate() {
        let value = if index == 0 {
            argument
                .as_os_str()
                .as_bytes()
                .rsplit(|byte| *byte == b'/')
                .next()
                .unwrap_or_default()
                .to_vec()
        } else {
            normalize_workspace(argument.as_os_str().as_bytes(), workspace)
        };
        write_field(&mut encoded, &value);
    }
    encoded
}

fn write_count(output: &mut Vec<u8>, count: usize) {
    let count = u64::try_from(count).expect("native command argument count fits in u64");
    output.extend_from_slice(&count.to_be_bytes());
}

fn write_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("native command argument length fits in u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
