//! Corpus migration guard.
//!
//! Every example package under `docs/examples/gluon/packages` is being migrated
//! from the legacy `cast.package.v3` authoring ABI to the language-agnostic
//! authored form (`import! cast.authored.v1` + a minimal record lowered by shared
//! Rust). During the migration each package carries BOTH files: the untouched
//! legacy `stone.glu` and the new `stone.authored.glu`. This test asserts they
//! evaluate to the *identical* `PackageSpec` — the invariant that lets the
//! rewrite proceed safely, package by package, before the default engine path is
//! switched and the legacy ABI deleted.

use std::path::{Path, PathBuf};

use declarative_config::{DeclarationEvaluator, SourceRoot};
use stone_recipe::package::{GluonPackageEvaluator, PackageSpec};

fn packages_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/packages")
}

/// The authored form of each migrated package reproduces exactly the
/// `PackageSpec` its legacy recipe produced.
#[test]
fn every_authored_recipe_reproduces_its_legacy_package_spec() {
    let mut checked = 0u32;
    let mut failures = Vec::new();

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(packages_root())
        .expect("packages directory is readable")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let legacy = dir.join("stone.glu");
        let authored = dir.join("stone.authored.glu");
        if !legacy.exists() || !authored.exists() {
            continue;
        }

        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let source_root = SourceRoot::new(&dir).expect("source root");
        let evaluator = <GluonPackageEvaluator as DeclarationEvaluator<PackageSpec>>::with_source_root(
            &GluonPackageEvaluator::default(),
            source_root.clone(),
        );

        let legacy_src = source_root
            .load(Path::new("stone.glu"), 1 << 20)
            .expect("load legacy recipe");
        let authored_src = source_root
            .load(Path::new("stone.authored.glu"), 1 << 20)
            .expect("load authored recipe");

        let legacy_spec = match DeclarationEvaluator::<PackageSpec>::evaluate(&evaluator, &legacy_src) {
            Ok(evaluation) => evaluation.value,
            Err(error) => {
                failures.push(format!("{name}: legacy recipe failed to evaluate: {error:?}"));
                continue;
            }
        };

        match evaluator.evaluate_authored(&authored_src) {
            Ok(authored_spec) if authored_spec == legacy_spec => checked += 1,
            Ok(authored_spec) => failures.push(format!(
                "{name}: authored PackageSpec differs from legacy\n  legacy:   {legacy_spec:?}\n  authored: {authored_spec:?}"
            )),
            Err(error) => {
                failures.push(format!("{name}: authored recipe failed to evaluate: {error:?}"))
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        checked > 0,
        "no migrated packages found (expected at least one stone.authored.glu)"
    );
}
