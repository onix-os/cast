//! Single-package authored-migration verifier.
//!
//! Usage: `cargo run -q --example verify_authored -- <package-dir>`
//!
//! Evaluates the package's legacy `stone.glu` (via the default `cast.package.v3`
//! path) and its migrated `stone.authored.glu` (via the shared authored path),
//! and exits 0 only when they produce the identical `PackageSpec`. On any
//! difference it prints both specs and exits non-zero. This is the per-package
//! check the corpus migration drives, without recompiling the whole test binary.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use declarative_config::{DeclarationEvaluator, SourceRoot};
use stone_recipe::package::{GluonPackageEvaluator, PackageSpec};

fn main() -> ExitCode {
    let dir = match std::env::args().nth(1) {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("usage: verify_authored <package-dir>");
            return ExitCode::FAILURE;
        }
    };

    let source_root = match SourceRoot::new(&dir) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("source root {}: {error:?}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    let evaluator = <GluonPackageEvaluator as DeclarationEvaluator<PackageSpec>>::with_source_root(
        &GluonPackageEvaluator::default(),
        source_root.clone(),
    );

    let legacy_src = source_root
        .load(Path::new("stone.glu"), 1 << 20)
        .expect("load stone.glu");
    let authored_src = source_root
        .load(Path::new("stone.authored.glu"), 1 << 20)
        .expect("load stone.authored.glu");

    let legacy = match DeclarationEvaluator::<PackageSpec>::evaluate(&evaluator, &legacy_src) {
        Ok(evaluation) => evaluation.value,
        Err(error) => {
            eprintln!("LEGACY-ERROR: {error:?}");
            return ExitCode::FAILURE;
        }
    };

    match evaluator.evaluate_authored(&authored_src) {
        Ok(authored) if authored == legacy => {
            println!("OK");
            ExitCode::SUCCESS
        }
        Ok(authored) => {
            eprintln!("MISMATCH between legacy and authored PackageSpec");
            eprintln!("--- legacy ---\n{legacy:#?}");
            eprintln!("--- authored ---\n{authored:#?}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("AUTHORED-ERROR: {error:?}");
            ExitCode::FAILURE
        }
    }
}
