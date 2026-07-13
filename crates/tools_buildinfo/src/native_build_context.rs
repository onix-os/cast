// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

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
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
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
    "CMAKE_GENERATOR_PLATFORM",
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

// These are the knobs read by native dependencies in the Boulder/Stone
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
    ToolSpec::targeted("pkg-config", "PKG_CONFIG", "pkg-config"),
];

const EXACT_TOOLS: &[ToolSpec] = &[
    ToolSpec::exact("cmake", "CMAKE", "cmake"),
    ToolSpec::exact("rust-linker", "RUSTC_LINKER", "cc"),
    ToolSpec::aws_targeted("aws-lc-cmake", "CMAKE", "cmake"),
];

const GENERATOR_TOOL_VARIABLES: &[&str] = &["CMAKE_MAKE_PROGRAM", "MAKE", "NINJA", "NMAKE"];

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
    AwsTargeted,
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

    const fn aws_targeted(role: &'static str, variable: &'static str, default_program: &'static str) -> Self {
        Self {
            role,
            variable,
            default_program,
            selection: ToolSelection::AwsTargeted,
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
/// `probe` receives the tool role and selected command and returns its content-strong
/// executable identity, including stable `--version` output.  `None` is used
/// only while reproducing AWS-LC's implicit
/// `cmake3`-then-`cmake` discovery.  Every command that is ultimately selected
/// must have an identity; otherwise two different implementations could share
/// one Boulder semantic fingerprint.
pub(crate) fn collect<I, F>(environment: I, workspace_root: &Path, mut probe: F) -> io::Result<NativeBuildContext>
where
    I: IntoIterator<Item = (OsString, OsString)>,
    F: FnMut(&str, &[OsString]) -> io::Result<Option<CommandIdentity>>,
{
    let environment = normalize_environment(environment)?;
    let host = required_utf8(&environment, "HOST")?;
    let target = required_utf8(&environment, "TARGET")?;
    let kind = if host == target { "HOST" } else { "TARGET" };
    let target_underscored = target.replace(['-', '.'], "_");
    let workspace = workspace_root.as_os_str().as_bytes();

    let mut watched = watched_environment(kind, target, &target_underscored);
    let mut inputs = BTreeMap::<String, Vec<u8>>::new();

    for key in &watched {
        if matches!(key.as_str(), "HOST" | "TARGET") {
            continue;
        }
        if is_tool_selector(key, kind, target, &target_underscored) || GENERATOR_TOOL_VARIABLES.contains(&key.as_str())
        {
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
        let (command, identity) = if spec.selection == ToolSelection::AwsTargeted && selector.is_none() {
            let cmake3 = vec![OsString::from("cmake3")];
            match probe(spec.role, &cmake3)? {
                Some(identity) => (cmake3, identity),
                None => {
                    let cmake = vec![OsString::from("cmake")];
                    let identity = require_tool_identity(spec.role, &cmake, probe(spec.role, &cmake)?)?;
                    (cmake, identity)
                }
            }
        } else {
            let selector = selector.unwrap_or_else(|| OsString::from(spec.default_program));
            let command = parse_command(selector.as_os_str()).unwrap_or_else(|| vec![selector.clone()]);
            let identity = require_tool_identity(spec.role, &command, probe(spec.role, &command)?)?;
            (command, identity)
        };
        let encoded_command = encode_command(&command, workspace);
        inputs.insert(format!("native.tool.{}.command", spec.role), encoded_command);
        inputs.insert(
            format!("native.tool.{}.identity", spec.role),
            identity.encode(workspace_root),
        );
    }

    if let Some(selector) = select_cmake_generator_tool(&environment, target, &target_underscored) {
        let command = parse_command(selector.as_os_str()).unwrap_or_else(|| vec![selector.clone()]);
        inputs.insert(
            "native.tool.cmake-generator.command".to_owned(),
            encode_command(&command, workspace),
        );
        let identity = require_tool_identity("cmake-generator", &command, probe("cmake-generator", &command)?)?;
        inputs.insert(
            "native.tool.cmake-generator.identity".to_owned(),
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
        let command = parse_command(selector.as_os_str()).unwrap_or_else(|| vec![selector.clone()]);
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

fn watched_environment(kind: &str, target: &str, target_underscored: &str) -> BTreeSet<String> {
    let mut watched = BTreeSet::from(["HOST".to_owned(), "TARGET".to_owned()]);
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
    watched.extend(GENERATOR_TOOL_VARIABLES.iter().map(|key| (*key).to_owned()));
    watched.insert(format!("CMAKE_GENERATOR_{target_underscored}"));

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
    TARGET_SCOPED_TOOLS.iter().chain(EXACT_TOOLS).any(|spec| {
        tool_keys(*spec, kind, target, target_underscored)
            .iter()
            .any(|candidate| candidate == key)
    })
}

fn tool_keys(spec: ToolSpec, kind: &str, target: &str, target_underscored: &str) -> Vec<String> {
    match spec.selection {
        ToolSelection::Targeted => targeted_keys(spec.variable, kind, target, target_underscored).into(),
        ToolSelection::Exact => vec![spec.variable.to_owned()],
        ToolSelection::AwsTargeted => vec![
            format!("AWS_LC_SYS_{}_{target_underscored}", spec.variable),
            format!("AWS_LC_SYS_{}", spec.variable),
            format!("{}_{target_underscored}", spec.variable),
            spec.variable.to_owned(),
        ],
    }
}

fn select_cmake_generator_tool(
    environment: &BTreeMap<String, OsString>,
    target: &str,
    target_underscored: &str,
) -> Option<OsString> {
    let generator = [
        format!("AWS_LC_SYS_CMAKE_GENERATOR_{target_underscored}"),
        "AWS_LC_SYS_CMAKE_GENERATOR".to_owned(),
        format!("CMAKE_GENERATOR_{target_underscored}"),
        "CMAKE_GENERATOR".to_owned(),
    ]
    .into_iter()
    .find_map(|key| environment.get(&key));
    let generator = generator.and_then(|value| value.to_str()).map(str::to_ascii_lowercase);

    let (override_variable, default_program) = match generator.as_deref() {
        Some(generator) if generator.contains("ninja") => ("NINJA", "ninja"),
        Some(generator) if generator.contains("nmake") => ("NMAKE", "nmake"),
        Some(generator) if generator.contains("makefiles") => ("MAKE", "make"),
        Some(_) => return None,
        None if target.contains("windows-msvc") => return None,
        None => ("MAKE", "make"),
    };

    Some(
        environment
            .get("CMAKE_MAKE_PROGRAM")
            .or_else(|| environment.get(override_variable))
            .cloned()
            .unwrap_or_else(|| OsString::from(default_program)),
    )
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
    if !matches!(role, "cc" | "cxx") {
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

fn parse_command(value: &OsStr) -> Option<Vec<OsString>> {
    #[derive(Clone, Copy)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut quote = Quote::None;
    let mut escaped = false;
    let mut started = false;

    for byte in value.as_bytes() {
        if escaped {
            word.push(*byte);
            escaped = false;
            started = true;
            continue;
        }
        match (quote, *byte) {
            (Quote::None, b'\\') | (Quote::Double, b'\\') => escaped = true,
            (Quote::None, b'\'') => {
                quote = Quote::Single;
                started = true;
            }
            (Quote::Single, b'\'') => quote = Quote::None,
            (Quote::None, b'"') => {
                quote = Quote::Double;
                started = true;
            }
            (Quote::Double, b'"') => quote = Quote::None,
            (Quote::None, byte) if byte.is_ascii_whitespace() => {
                if started {
                    words.push(OsString::from_vec(std::mem::take(&mut word)));
                    started = false;
                }
            }
            (_, byte) => {
                word.push(byte);
                started = true;
            }
        }
    }

    if escaped || !matches!(quote, Quote::None) {
        return None;
    }
    if started {
        words.push(OsString::from_vec(word));
    }
    (!words.is_empty()).then_some(words)
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
