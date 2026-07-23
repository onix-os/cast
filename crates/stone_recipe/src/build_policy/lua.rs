//! Lua declaration DTOs for the build-policy domain (Phase L5, in progress).
//!
//! The build policy is pervaded by tuple/newtype enum variants (`TextSpec`,
//! `BuildToolSpec`, `ValuePatch`, …). Serde's internally-tagged `#[serde(tag =
//! "kind")]` encoding — the uniform Lua encoding every other domain uses — does
//! not support tuple variants, so those enums cannot be decoded by deriving
//! `Deserialize` on the domain type the way the build lock's struct/unit enums
//! were. This module holds the struct-variant Lua DTOs plus `From` conversions
//! that bridge that gap. It is the foundation of the full build-policy adapter;
//! the remaining spec tree is layered on top in later slices.

// The full build-policy adapter is assembled over several slices; these
// foundation DTOs are exercised by the tests below until the top-level
// evaluator that consumes them lands.
#![cfg_attr(not(test), allow(dead_code))]

use lua_config::LuaPatch;
use serde::Deserialize;

use super::{
    ArrayPatch, BuildCommandSpec, BuildProgramSpec, BuildToolSpec, CompilerFlagsSpec,
    CompilerToolsSpec, ContextValue, EnvironmentBindingSpec, EnvironmentCondition,
    InstallLayoutSpec, PlatformPolicySpec, TargetEmulationSpec, TargetPolicySpec, TextSpec,
    ToolchainFlagsSpec, ToolchainsSpec, ValuePatch,
};

/// Map a `Vec` of Lua DTOs to a `Vec` of their domain values.
fn text_vec(values: Vec<LuaTextSpec>) -> Vec<TextSpec> {
    values.into_iter().map(Into::into).collect()
}

/// Convert a decoded [`LuaPatch`] into the domain [`ValuePatch`], mapping the
/// `set` payload through its own `Into` conversion so patched DTO values reach
/// their domain form.
pub(crate) fn value_patch<L, D>(patch: LuaPatch<L>) -> ValuePatch<D>
where
    D: From<L>,
{
    match patch {
        LuaPatch::Keep => ValuePatch::Keep,
        LuaPatch::Set { value } => ValuePatch::Set(value.into()),
    }
}

/// The Lua encoding of an [`ArrayPatch`]: `{ kind = "keep" }` or a
/// `{ kind = "replace" | "prepend" | "append", values = { … } }` overlay.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaArrayPatch<T> {
    Keep,
    Replace { values: Vec<T> },
    Prepend { values: Vec<T> },
    Append { values: Vec<T> },
}

/// Convert a decoded [`LuaArrayPatch`] into the domain [`ArrayPatch`], mapping
/// each element through its own `Into` conversion.
pub(crate) fn array_patch<L, D>(patch: LuaArrayPatch<L>) -> ArrayPatch<D>
where
    D: From<L>,
{
    let convert = |values: Vec<L>| values.into_iter().map(Into::into).collect();
    match patch {
        LuaArrayPatch::Keep => ArrayPatch::Keep,
        LuaArrayPatch::Replace { values } => ArrayPatch::Replace(convert(values)),
        LuaArrayPatch::Prepend { values } => ArrayPatch::Prepend(convert(values)),
        LuaArrayPatch::Append { values } => ArrayPatch::Append(convert(values)),
    }
}

/// The Lua encoding of a [`TextSpec`]. The domain enum's tuple variants become
/// struct variants so the uniform `{ kind = … }` tag applies; `Context` reuses
/// the all-unit [`ContextValue`], which decodes directly from its snake_case
/// name.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaTextSpec {
    Literal { value: String },
    Context { value: ContextValue },
    Concat { values: Vec<LuaTextSpec> },
}

impl From<LuaTextSpec> for TextSpec {
    fn from(text: LuaTextSpec) -> Self {
        match text {
            LuaTextSpec::Literal { value } => Self::Literal(value),
            LuaTextSpec::Context { value } => Self::Context(value),
            LuaTextSpec::Concat { values } => {
                Self::Concat(values.into_iter().map(Into::into).collect())
            }
        }
    }
}

/// The Lua encoding of a [`BuildToolSpec`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaBuildToolSpec {
    Package { value: String },
    Binary { value: String },
    SystemBinary { value: String },
}

impl From<LuaBuildToolSpec> for BuildToolSpec {
    fn from(tool: LuaBuildToolSpec) -> Self {
        match tool {
            LuaBuildToolSpec::Package { value } => Self::Package(value),
            LuaBuildToolSpec::Binary { value } => Self::Binary(value),
            LuaBuildToolSpec::SystemBinary { value } => Self::SystemBinary(value),
        }
    }
}

/// The Lua encoding of a [`CompilerFlagsSpec`] — eight ordered flag lists, each
/// element a [`TextSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaCompilerFlagsSpec {
    pub c: Vec<LuaTextSpec>,
    pub cxx: Vec<LuaTextSpec>,
    pub f: Vec<LuaTextSpec>,
    pub d: Vec<LuaTextSpec>,
    pub rust: Vec<LuaTextSpec>,
    pub vala: Vec<LuaTextSpec>,
    pub go: Vec<LuaTextSpec>,
    pub ld: Vec<LuaTextSpec>,
}

impl From<LuaCompilerFlagsSpec> for CompilerFlagsSpec {
    fn from(flags: LuaCompilerFlagsSpec) -> Self {
        Self {
            c: text_vec(flags.c),
            cxx: text_vec(flags.cxx),
            f: text_vec(flags.f),
            d: text_vec(flags.d),
            rust: text_vec(flags.rust),
            vala: text_vec(flags.vala),
            go: text_vec(flags.go),
            ld: text_vec(flags.ld),
        }
    }
}

/// The Lua encoding of an [`InstallLayoutSpec`] — the canonical install
/// directory locators, each a [`TextSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaInstallLayoutSpec {
    pub prefix: LuaTextSpec,
    pub bindir: LuaTextSpec,
    pub sbindir: LuaTextSpec,
    pub includedir: LuaTextSpec,
    pub libdir: LuaTextSpec,
    pub libexecdir: LuaTextSpec,
    pub datadir: LuaTextSpec,
    pub vendordir: LuaTextSpec,
    pub docdir: LuaTextSpec,
    pub infodir: LuaTextSpec,
    pub localedir: LuaTextSpec,
    pub mandir: LuaTextSpec,
    pub sysconfdir: LuaTextSpec,
    pub localstatedir: LuaTextSpec,
    pub sharedstatedir: LuaTextSpec,
    pub runstatedir: LuaTextSpec,
    pub sysusersdir: LuaTextSpec,
    pub tmpfilesdir: LuaTextSpec,
    pub udevrulesdir: LuaTextSpec,
    pub bash_completions_dir: LuaTextSpec,
    pub fish_completions_dir: LuaTextSpec,
    pub elvish_completions_dir: LuaTextSpec,
    pub zsh_completions_dir: LuaTextSpec,
}

impl From<LuaInstallLayoutSpec> for InstallLayoutSpec {
    fn from(layout: LuaInstallLayoutSpec) -> Self {
        Self {
            prefix: layout.prefix.into(),
            bindir: layout.bindir.into(),
            sbindir: layout.sbindir.into(),
            includedir: layout.includedir.into(),
            libdir: layout.libdir.into(),
            libexecdir: layout.libexecdir.into(),
            datadir: layout.datadir.into(),
            vendordir: layout.vendordir.into(),
            docdir: layout.docdir.into(),
            infodir: layout.infodir.into(),
            localedir: layout.localedir.into(),
            mandir: layout.mandir.into(),
            sysconfdir: layout.sysconfdir.into(),
            localstatedir: layout.localstatedir.into(),
            sharedstatedir: layout.sharedstatedir.into(),
            runstatedir: layout.runstatedir.into(),
            sysusersdir: layout.sysusersdir.into(),
            tmpfilesdir: layout.tmpfilesdir.into(),
            udevrulesdir: layout.udevrulesdir.into(),
            bash_completions_dir: layout.bash_completions_dir.into(),
            fish_completions_dir: layout.fish_completions_dir.into(),
            elvish_completions_dir: layout.elvish_completions_dir.into(),
            zsh_completions_dir: layout.zsh_completions_dir.into(),
        }
    }
}

/// The Lua encoding of a [`BuildProgramSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildProgramSpec {
    pub path: String,
    pub requirement: LuaBuildToolSpec,
}

impl From<LuaBuildProgramSpec> for BuildProgramSpec {
    fn from(program: LuaBuildProgramSpec) -> Self {
        Self {
            path: program.path,
            requirement: program.requirement.into(),
        }
    }
}

/// The Lua encoding of a [`BuildCommandSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildCommandSpec {
    pub program: LuaBuildProgramSpec,
    pub args: Vec<String>,
}

impl From<LuaBuildCommandSpec> for BuildCommandSpec {
    fn from(command: LuaBuildCommandSpec) -> Self {
        Self {
            program: command.program.into(),
            args: command.args,
        }
    }
}

/// The Lua encoding of a [`CompilerToolsSpec`] — one build command per toolchain
/// executable role.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaCompilerToolsSpec {
    pub cc: LuaBuildCommandSpec,
    pub cxx: LuaBuildCommandSpec,
    pub objc: LuaBuildCommandSpec,
    pub objcxx: LuaBuildCommandSpec,
    pub cpp: LuaBuildCommandSpec,
    pub objcpp: LuaBuildCommandSpec,
    pub objcxxcpp: LuaBuildCommandSpec,
    pub ar: LuaBuildCommandSpec,
    pub ld: LuaBuildCommandSpec,
    pub objcopy: LuaBuildCommandSpec,
    pub nm: LuaBuildCommandSpec,
    pub ranlib: LuaBuildCommandSpec,
    pub strip: LuaBuildCommandSpec,
}

impl From<LuaCompilerToolsSpec> for CompilerToolsSpec {
    fn from(tools: LuaCompilerToolsSpec) -> Self {
        Self {
            cc: tools.cc.into(),
            cxx: tools.cxx.into(),
            objc: tools.objc.into(),
            objcxx: tools.objcxx.into(),
            cpp: tools.cpp.into(),
            objcpp: tools.objcpp.into(),
            objcxxcpp: tools.objcxxcpp.into(),
            ar: tools.ar.into(),
            ld: tools.ld.into(),
            objcopy: tools.objcopy.into(),
            nm: tools.nm.into(),
            ranlib: tools.ranlib.into(),
            strip: tools.strip.into(),
        }
    }
}

/// The Lua encoding of a [`ToolchainsSpec`] — the LLVM and GNU tool tables.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaToolchainsSpec {
    pub llvm: LuaCompilerToolsSpec,
    pub gnu: LuaCompilerToolsSpec,
}

impl From<LuaToolchainsSpec> for ToolchainsSpec {
    fn from(toolchains: LuaToolchainsSpec) -> Self {
        Self {
            llvm: toolchains.llvm.into(),
            gnu: toolchains.gnu.into(),
        }
    }
}

/// The Lua encoding of a [`ToolchainFlagsSpec`] — common/GNU/LLVM flag sets.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaToolchainFlagsSpec {
    pub common: LuaCompilerFlagsSpec,
    pub gnu: LuaCompilerFlagsSpec,
    pub llvm: LuaCompilerFlagsSpec,
}

impl From<LuaToolchainFlagsSpec> for ToolchainFlagsSpec {
    fn from(flags: LuaToolchainFlagsSpec) -> Self {
        Self {
            common: flags.common.into(),
            gnu: flags.gnu.into(),
            llvm: flags.llvm.into(),
        }
    }
}

/// The Lua encoding of an [`EnvironmentBindingSpec`]. `condition` reuses the
/// all-unit [`EnvironmentCondition`], which decodes from its snake_case name.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaEnvironmentBindingSpec {
    pub name: String,
    pub value: LuaTextSpec,
    pub condition: EnvironmentCondition,
}

impl From<LuaEnvironmentBindingSpec> for EnvironmentBindingSpec {
    fn from(binding: LuaEnvironmentBindingSpec) -> Self {
        Self {
            name: binding.name,
            value: binding.value.into(),
            condition: binding.condition,
        }
    }
}

/// The Lua encoding of a [`TargetPolicySpec`]. The triple/platform fields are
/// plain data; emulation and the platforms decode directly on their domain
/// types, while the architecture flags and environment reuse the Lua wrappers.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaTargetPolicySpec {
    pub name: String,
    pub target_triple: String,
    pub build_triple: String,
    pub host_triple: String,
    pub lib_suffix: String,
    pub artifact_architecture: String,
    pub emulation: TargetEmulationSpec,
    pub build_platform: PlatformPolicySpec,
    pub host_platform: PlatformPolicySpec,
    pub target_platform: PlatformPolicySpec,
    pub architecture_flags: LuaToolchainFlagsSpec,
    pub environment: Vec<LuaEnvironmentBindingSpec>,
}

impl From<LuaTargetPolicySpec> for TargetPolicySpec {
    fn from(target: LuaTargetPolicySpec) -> Self {
        Self {
            name: target.name,
            target_triple: target.target_triple,
            build_triple: target.build_triple,
            host_triple: target.host_triple,
            lib_suffix: target.lib_suffix,
            artifact_architecture: target.artifact_architecture,
            emulation: target.emulation,
            build_platform: target.build_platform,
            host_platform: target.host_platform,
            target_platform: target.target_platform,
            architecture_flags: target.architecture_flags.into(),
            environment: target.environment.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("build-policy.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn a_literal_text_spec_decodes() {
        let text: TextSpec = decode::<LuaTextSpec>(r#"return { kind = "literal", value = "cc" }"#).into();
        assert_eq!(text, TextSpec::Literal("cc".to_owned()));
    }

    #[test]
    fn a_context_text_spec_decodes_the_unit_context_value() {
        let text: TextSpec =
            decode::<LuaTextSpec>(r#"return { kind = "context", value = "package_name" }"#).into();
        assert_eq!(text, TextSpec::Context(ContextValue::PackageName));
    }

    #[test]
    fn a_nested_concat_text_spec_decodes_recursively() {
        let source = r#"
return {
    kind = "concat",
    values = {
        { kind = "literal", value = "lib" },
        { kind = "context", value = "lib_suffix" },
    },
}
"#;
        let text: TextSpec = decode::<LuaTextSpec>(source).into();
        assert_eq!(
            text,
            TextSpec::Concat(vec![
                TextSpec::Literal("lib".to_owned()),
                TextSpec::Context(ContextValue::LibSuffix),
            ])
        );
    }

    #[test]
    fn build_tool_variants_decode() {
        let package: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "package", value = "cmake" }"#).into();
        assert_eq!(package, BuildToolSpec::Package("cmake".to_owned()));

        let system: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "system_binary", value = "/bin/sh" }"#)
                .into();
        assert_eq!(system, BuildToolSpec::SystemBinary("/bin/sh".to_owned()));
    }

    #[test]
    fn a_value_patch_keeps_or_sets_the_converted_payload() {
        let keep = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ValuePatch::Keep);

        let set = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "set", value = { kind = "binary", value = "meson" } }"#,
        ));
        assert_eq!(set, ValuePatch::Set(BuildToolSpec::Binary("meson".to_owned())));
    }

    #[test]
    fn an_array_patch_maps_every_operation_and_element() {
        let keep = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ArrayPatch::Keep);

        let append = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "append", values = { { kind = "package", value = "ninja" } } }"#,
        ));
        assert_eq!(
            append,
            ArrayPatch::Append(vec![BuildToolSpec::Package("ninja".to_owned())])
        );
    }

    #[test]
    fn compiler_flags_decode_with_empty_and_populated_lists() {
        let source = r#"
return {
    c = { { kind = "literal", value = "-Wall" } },
    cxx = {},
    f = {},
    d = {},
    rust = {},
    vala = {},
    go = {},
    ld = { { kind = "context", value = "ld_flags" } },
}
"#;
        let flags: CompilerFlagsSpec = decode::<LuaCompilerFlagsSpec>(source).into();
        assert_eq!(flags.c, vec![TextSpec::Literal("-Wall".to_owned())]);
        assert!(flags.cxx.is_empty());
        assert_eq!(flags.ld, vec![TextSpec::Context(ContextValue::LdFlags)]);
    }

    #[test]
    fn a_build_command_decodes_program_requirement_and_args() {
        let source = r#"
return {
    program = {
        path = "/usr/bin/cc",
        requirement = { kind = "package", value = "llvm" },
    },
    args = { "-fPIC", "-O2" },
}
"#;
        let command: BuildCommandSpec = decode::<LuaBuildCommandSpec>(source).into();
        assert_eq!(command.program.path, "/usr/bin/cc");
        assert_eq!(command.program.requirement, BuildToolSpec::Package("llvm".to_owned()));
        assert_eq!(command.args, vec!["-fPIC".to_owned(), "-O2".to_owned()]);
    }

    #[test]
    fn pure_target_types_decode_directly_on_the_domain_type() {
        let platform: PlatformPolicySpec = decode(
            r#"return { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" }"#,
        );
        assert_eq!(platform.architecture, "x86_64");

        let native: TargetEmulationSpec = decode(r#"return { kind = "native" }"#);
        assert_eq!(native, TargetEmulationSpec::Native);

        let emul32: TargetEmulationSpec =
            decode(r#"return { kind = "emul32", host_architecture = "x86_64" }"#);
        assert_eq!(
            emul32,
            TargetEmulationSpec::Emul32 { host_architecture: "x86_64".to_owned() }
        );

        let condition: EnvironmentCondition = decode(r#"return "compiler_cache_enabled""#);
        assert_eq!(condition, EnvironmentCondition::CompilerCacheEnabled);
    }

    #[test]
    fn an_environment_binding_decodes_value_and_condition() {
        let source = r#"
return {
    name = "CFLAGS",
    value = { kind = "context", value = "c_flags" },
    condition = "always",
}
"#;
        let binding: EnvironmentBindingSpec = decode::<LuaEnvironmentBindingSpec>(source).into();
        assert_eq!(binding.name, "CFLAGS");
        assert_eq!(binding.value, TextSpec::Context(ContextValue::CFlags));
        assert_eq!(binding.condition, EnvironmentCondition::Always);
    }

    fn literal_layout_field(name: &str) -> String {
        format!(r#"{name} = {{ kind = "literal", value = "/{name}" }}"#)
    }

    #[test]
    fn an_install_layout_decodes_every_locator() {
        let fields = [
            "prefix", "bindir", "sbindir", "includedir", "libdir", "libexecdir", "datadir",
            "vendordir", "docdir", "infodir", "localedir", "mandir", "sysconfdir", "localstatedir",
            "sharedstatedir", "runstatedir", "sysusersdir", "tmpfilesdir", "udevrulesdir",
            "bash_completions_dir", "fish_completions_dir", "elvish_completions_dir",
            "zsh_completions_dir",
        ];
        let body = fields.iter().map(|name| literal_layout_field(name)).collect::<Vec<_>>().join(",\n    ");
        let source = format!("return {{\n    {body},\n}}");

        let layout: InstallLayoutSpec = decode::<LuaInstallLayoutSpec>(&source).into();
        assert_eq!(layout.prefix, TextSpec::Literal("/prefix".to_owned()));
        assert_eq!(layout.zsh_completions_dir, TextSpec::Literal("/zsh_completions_dir".to_owned()));
    }
}
