// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::io;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;

use crate::build;
use crate::executor::Executor;
use crate::package::FrozenPackager;
use crate::{Env, Timing, container, package, planner, profile, timing};
use chrono::Local;
use clap::Parser;
use moss::signal::inhibit;
use thiserror::Error;
use thread_priority::{NormalThreadSchedulePolicy, ThreadPriority, ThreadSchedulePolicy, thread_native_id};

#[derive(Debug, Parser)]
#[command(about = "Build stone package(s) from a stone recipe file")]
pub struct Command {
    #[arg(short, long, default_value = "default-x86_64")]
    profile: profile::Id,
    #[arg(
        short,
        long = "compiler-cache",
        help = "Enable compiler caching",
        default_value_t = false
    )]
    ccache: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Resolve and atomically update build.lock.glu before building"
    )]
    update_lock: bool,
    #[arg(
        long,
        default_value_t = false,
        requires = "update_lock",
        help = "Refresh repositories before updating build.lock.glu"
    )]
    refresh_repositories: bool,
    #[arg(long, help = "Exact build target")]
    target: String,
    #[arg(long, help = "Explicit reproducible source timestamp")]
    source_date_epoch: i64,
    #[arg(long, default_value = "1", help = "Explicit parallel job count")]
    jobs: NonZeroU32,
    #[arg(
        long = "normal-priority",
        help = "Run the build without lowering the process priority",
        default_value_t = false
    )]
    normal_priority: bool,
    #[arg(short, long, default_value = ".", help = "Directory to store build results")]
    output: PathBuf,
    #[arg(
        default_value = "./stone.glu",
        help = "Path to a stone.glu recipe file or recipe directory"
    )]
    recipe: PathBuf,
    #[arg(
        short,
        long,
        default_value = "1",
        help = "Specify the build release number used for this build"
    )]
    build_release: NonZeroU64,
    #[arg(
        long,
        help = "Automatically cleanup all build related artefacts",
        default_value_t = false
    )]
    cleanup: bool,
    /// Compare the emitted binary manifest byte-for-byte with [MANIFEST].
    ///
    /// The comparison file is read on the host after the isolated build and is
    /// never exposed to build steps.
    #[arg(long = "verify", value_name = "MANIFEST")]
    verify_against: Option<PathBuf>,
}

pub fn handle(command: Command, env: Env) -> Result<(), Error> {
    let output = command.output.clone();
    let Command {
        profile,
        recipe: recipe_path,
        ccache,
        update_lock,
        refresh_repositories,
        normal_priority,
        build_release,
        cleanup,
        verify_against,
        target,
        source_date_epoch,
        jobs,
        ..
    } = command;

    let mut timing = Timing::default();
    let timer = timing.begin(timing::Kind::Initialize);

    if !output.exists() {
        return Err(Error::MissingOutput(output));
    }

    // Ensure verify against path isn't json/jsonc since
    // we verify against binary manifest
    if let Some(path) = verify_against.as_ref()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.starts_with("json"))
    {
        return Err(Error::VerifyBinaryManifestRequired(path.to_owned()));
    }
    if let Some(path) = verify_against.as_ref()
        && !path.is_file()
    {
        return Err(Error::MissingVerifyManifest(path.to_owned()));
    }

    let planned = planner::plan_for_build(
        env,
        planner::Request {
            recipe: recipe_path,
            profile,
            target,
            source_date_epoch,
            build_release,
            jobs,
            compiler_cache: ccache,
            update_lock,
            refresh_repositories,
        },
        &output,
    )?;
    let plan = planned.plan;
    let runtime = planned.runtime;
    let executor = Executor::new(&plan)?;
    let packager = FrozenPackager::from_plan(&runtime.paths, &plan)?;
    let derivation_id = plan.derivation_id();
    let pkg_name = format!(
        "{}-{}-{}",
        plan.package.name, plan.package.version, plan.package.source_release
    );
    println!("boulder {}", tools_buildinfo::get_simple_version());
    println!("└─ building {pkg_name}-{}\n", plan.package.build_release);
    println!("└─ derivation {derivation_id}\n");
    runtime.setup(&plan, &mut timing, timer)?;

    let paths = &runtime.paths;

    // Set the current thread priority to SCHED_BATCH so that it's inherited by all child processes
    if !normal_priority {
        println!("Changing boulder thread priority to SCHED_BATCH during build:");
        match thread_priority::set_thread_priority_and_policy(
            thread_native_id(),
            ThreadPriority::Min,
            ThreadSchedulePolicy::Normal(NormalThreadSchedulePolicy::Batch),
        ) {
            Ok(_) => {
                println!("└─ priority set.\n");
            }
            Err(e) => {
                println!("└─ unable to change boulder thread scheduling priority to SCHED_BATCH.");
                println!("└─ error message: {e:?}");
                println!("└─ priority left at its default value.\n");
            }
        }
        println!("Continuing build:\n");
    }

    // hold a fd
    let _fd = inhibit(
        vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
        "boulder".into(),
        format!("Build in-progress: {pkg_name}"),
        "block".into(),
    );

    // Build & package from within container
    container::exec_frozen::<Error>(paths, &plan, || {
        executor.run(&mut timing)?;
        packager.package(&mut timing)?;

        timing.print_table();

        Ok(())
    })?;

    if let Some(expected) = verify_against.as_ref() {
        verify_frozen_manifest(paths, &plan, expected)?;
    }

    // Copy artefacts to host recipe dir
    package::sync_artefacts(paths).map_err(Error::SyncArtefacts)?;

    if cleanup {
        runtime
            .cleanup(&plan)
            .map_err(|error| Error::Cleanup(Box::new(error)))?;
    }

    println!(
        "Build finished successfully at {}",
        Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );

    Ok(())
}

fn verify_frozen_manifest(
    paths: &crate::Paths,
    plan: &stone_recipe::derivation::DerivationPlan,
    expected: &std::path::Path,
) -> Result<(), Error> {
    let generated = paths
        .artefacts()
        .host
        .join(format!("manifest.{}.bin", plan.package.architecture));
    if fs_err::read(&generated)? != fs_err::read(expected)? {
        return Err(Error::VerificationMismatch {
            generated,
            expected: expected.to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("output directory does not exist: {0:?}")]
    MissingOutput(PathBuf),
    #[error("build recipe")]
    Build(#[source] Box<build::Error>),
    #[error("package artifacts")]
    Package(#[from] package::Error),
    #[error("sync artefacts")]
    SyncArtefacts(#[source] io::Error),
    #[error("container")]
    Container(#[from] container::Error),
    #[error("setting thread priority")]
    Priority(#[from] thread_priority::Error),
    #[error("cleanup")]
    Cleanup(#[source] Box<build::Error>),
    #[error("Binary manifest required for verification, got {0:?}")]
    VerifyBinaryManifestRequired(PathBuf),
    #[error("verification manifest does not exist or is not a file: {0:?}")]
    MissingVerifyManifest(PathBuf),
    #[error("plan build")]
    Planner(#[source] Box<planner::Error>),
    #[error("execute frozen plan")]
    Executor(#[from] crate::executor::Error),
    #[error("verify frozen manifest IO")]
    VerifyIo(#[from] io::Error),
    #[error("generated manifest {generated:?} does not match {expected:?}")]
    VerificationMismatch { generated: PathBuf, expected: PathBuf },
}

impl From<build::Error> for Error {
    fn from(error: build::Error) -> Self {
        Self::Build(Box::new(error))
    }
}

impl From<planner::Error> for Error {
    fn from(error: planner::Error) -> Self {
        Self::Planner(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_recipe_is_gluon() {
        let command =
            Command::try_parse_from(["build", "--target", "x86_64", "--source-date-epoch", "1700000000"]).unwrap();

        assert_eq!(command.recipe, PathBuf::from("./stone.glu"));
        assert_eq!(command.jobs, NonZeroU32::new(1).unwrap());
    }

    #[test]
    fn frozen_build_requires_target_and_timestamp() {
        assert!(Command::try_parse_from(["build"]).is_err());
    }
}
