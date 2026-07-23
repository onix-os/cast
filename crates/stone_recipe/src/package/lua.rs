//! Lua declaration DTOs for the package recipe domain (Phase L5, in progress).
//!
//! Like the build policy, the package recipe reaches its domain value through an
//! infallible `From<GluonPackageSpec>`, so it is the neutral shape — pure
//! struct/unit types derive `Deserialize` directly on the domain type, while the
//! tuple/newtype enums (`DependencySpec`, `StepSpec`, …) get struct-variant Lua
//! DTOs with `From` conversions. This module holds that DTO tree; it is
//! assembled bottom-up over several slices toward a full `LuaPackageSpec`.

// The full package adapter is built across several slices; these DTOs are
// exercised by the tests below until the top-level evaluator lands.
#![cfg_attr(not(test), allow(dead_code))]

use serde::Deserialize;

use super::{DependencySpec, OutputRef, PackageRef};

/// The Lua encoding of a [`DependencySpec`]. The domain enum's tuple variants
/// become struct variants so the uniform `{ kind = … }` tag applies; the two
/// reference variants reuse the pure [`PackageRef`]/[`OutputRef`] domain types.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaDependencySpec {
    Package { value: PackageRef },
    Output { value: OutputRef },
    Binary { value: String },
    SystemBinary { value: String },
    PkgConfig { value: String },
    PkgConfig32 { value: String },
    Soname { value: String },
    #[serde(rename = "cmake")]
    CMake { value: String },
    Python { value: String },
    Interpreter { value: String },
}

impl From<LuaDependencySpec> for DependencySpec {
    fn from(dependency: LuaDependencySpec) -> Self {
        match dependency {
            LuaDependencySpec::Package { value } => Self::Package(value),
            LuaDependencySpec::Output { value } => Self::Output(value),
            LuaDependencySpec::Binary { value } => Self::Binary(value),
            LuaDependencySpec::SystemBinary { value } => Self::SystemBinary(value),
            LuaDependencySpec::PkgConfig { value } => Self::PkgConfig(value),
            LuaDependencySpec::PkgConfig32 { value } => Self::PkgConfig32(value),
            LuaDependencySpec::Soname { value } => Self::Soname(value),
            LuaDependencySpec::CMake { value } => Self::CMake(value),
            LuaDependencySpec::Python { value } => Self::Python(value),
            LuaDependencySpec::Interpreter { value } => Self::Interpreter(value),
        }
    }
}

/// Map a `Vec` of Lua dependency DTOs to their domain values.
pub(crate) fn dependency_vec(values: Vec<LuaDependencySpec>) -> Vec<DependencySpec> {
    values.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::super::MetaSpec;
    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("package.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn meta_decodes_directly_as_pure_data() {
        let meta: MetaSpec = decode(
            r#"return { pname = "hello", version = "1.0", release = 1, homepage = "https://x", license = { "MIT" } }"#,
        );
        assert_eq!(meta.pname, "hello");
        assert_eq!(meta.release, 1);
        assert_eq!(meta.license, vec!["MIT".to_owned()]);
    }

    #[test]
    fn dependency_variants_decode_including_references() {
        let binary: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "binary", value = "cc" }"#).into();
        assert_eq!(binary, DependencySpec::Binary("cc".to_owned()));

        let cmake: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "cmake", value = "Foo" }"#).into();
        assert_eq!(cmake, DependencySpec::CMake("Foo".to_owned()));

        let package: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "package", value = { name = "glibc" } }"#).into();
        assert_eq!(package, DependencySpec::Package(PackageRef { name: "glibc".to_owned() }));

        let output: DependencySpec = decode::<LuaDependencySpec>(
            r#"return { kind = "output", value = { package = { name = "llvm" }, output = "dev" } }"#,
        )
        .into();
        assert_eq!(
            output,
            DependencySpec::Output(OutputRef {
                package: PackageRef { name: "llvm".to_owned() },
                output: "dev".to_owned(),
            })
        );
    }
}
