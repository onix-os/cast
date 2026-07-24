//! Language-independence proof for the authored ABI.
//!
//! The exit criterion for lifting the authoring ABI into shared Rust is that
//! *either* configuration language can be removed and the other still authors a
//! complete package. This test authors the SAME non-trivial package twice —
//! once through `GluonPackageEvaluator` with Lua nowhere in the path, once
//! through `LuaPackageEvaluator` with Gluon nowhere in the path — and asserts the
//! two independently-produced `PackageSpec`s are byte-identical. Because the
//! defaults and builder lowering live in shared Rust (`lower`), neither engine
//! hosts authoring logic, and each is sufficient on its own.

use declarative_config::Source;
use stone_recipe::package::{GluonPackageEvaluator, LuaPackageEvaluator};

/// A Gluon recipe: names every field of the authored record; the shared `lower`
/// fills the defaults selected by `unset` / `outputs.default`.
const GLUON: &str = r#"let a = import! cast.authored.v1
{
    meta = {
        pname = "independent-widget",
        version = "2.3.1",
        release = 4,
        homepage = "https://example.invalid/independent-widget",
        license = ["Apache-2.0"],
    },
    builder = a.builder.cmake { flags = ["-DBUILD_SHARED_LIBS=ON", "-DENABLE_TOOLS=OFF"], run_tests = a.true },
    sources = [],
    native_build_inputs = [a.dep.binary "cmake"],
    build_inputs = [a.dep.package "zlib"],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = ["x86_64", "aarch64"],
    tuning = [],
    emul32 = a.false,
    mold = a.true,
    hooks = a.unset,
}
"#;

/// The same package as a minimal Lua table: structural, so every field that
/// takes its shared-Rust default is simply omitted.
const LUA: &str = r#"return {
    meta = {
        pname = "independent-widget",
        version = "2.3.1",
        release = 4,
        homepage = "https://example.invalid/independent-widget",
        license = { "Apache-2.0" },
    },
    builder = { kind = "cmake", flags = { "-DBUILD_SHARED_LIBS=ON", "-DENABLE_TOOLS=OFF" }, run_tests = true },
    native_build_inputs = { { kind = "binary", value = "cmake" } },
    build_inputs = { { kind = "package", value = { name = "zlib" } } },
    architectures = { "x86_64", "aarch64" },
    mold = true,
}
"#;

#[test]
fn either_language_authors_the_same_package_on_its_own() {
    // Gluon only — the Lua engine is not constructed or referenced.
    let gluon = GluonPackageEvaluator::default()
        .evaluate_authored(&Source::new("stone.glu", GLUON))
        .expect("gluon authors the package");

    // Lua only — the Gluon engine is not constructed or referenced.
    let lua = LuaPackageEvaluator::default()
        .evaluate_authored(&Source::new("stone.lua", LUA))
        .expect("lua authors the package");

    // Each language, on its own, lowered to the identical frozen package value.
    assert_eq!(gluon, lua, "authored PackageSpec must be language-independent");

    // Sanity: it really is the non-trivial package we authored, fully lowered.
    assert_eq!(gluon.meta.pname, "independent-widget");
    assert_eq!(gluon.architectures, ["x86_64", "aarch64"]);
    assert!(gluon.mold);
    assert_eq!(gluon.outputs.len(), 9, "shared default split-output set");
    assert_eq!(
        gluon.builder.phases.setup.steps,
        vec![stone_recipe::package::StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_SHARED_LIBS=ON".to_owned(), "-DENABLE_TOOLS=OFF".to_owned()],
        }],
    );
    assert_eq!(
        gluon.builder.phases.check.steps,
        vec![stone_recipe::package::StepSpec::CMakeTest],
        "run_tests=true lowered to a cmake check step",
    );
}
