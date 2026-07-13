// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use container::{Container, DevPolicy, LoopbackPolicy, ProcPolicy, PseudoFilesystemPolicy, SysPolicy, TmpPolicy};
use stone_recipe::derivation::{
    BuilderLayout, DerivationPlan, DevFilesystem, ExecutionCredentials, FilesystemPolicy, NetworkMode, SysFilesystem,
    TmpFilesystem,
};
use thiserror::Error;

use crate::Paths;

pub fn exec<E>(paths: &Paths, networking: bool, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    run(paths, networking, f)
}

/// Execute a frozen plan without exposing mutable recipe or global cache
/// inputs to build steps.
pub fn exec_frozen<E>(paths: &Paths, plan: &DerivationPlan, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let sandbox = frozen_sandbox(paths, plan)?;
    let rootfs = paths.rootfs().host;
    let mut container = Container::new(rootfs)
        .hostname(&sandbox.hostname)
        .networking(matches!(plan.execution.network, NetworkMode::Enabled))
        .loopback(frozen_loopback_policy())
        .pseudo_filesystems(frozen_pseudo_filesystems(plan.execution.filesystems))
        .ignore_host_sigint(true)
        .work_dir(&sandbox.work_dir);

    for mount in sandbox.mounts {
        container = container.bind_rw(&mount.host, &mount.guest);
    }
    container.run::<E>(f)?;
    Ok(())
}

fn frozen_loopback_policy() -> LoopbackPolicy {
    LoopbackPolicy::KernelDefault
}

fn frozen_pseudo_filesystems(filesystems: FilesystemPolicy) -> PseudoFilesystemPolicy {
    PseudoFilesystemPolicy {
        proc: ProcPolicy::None,
        tmp: match filesystems.tmp {
            TmpFilesystem::Empty => TmpPolicy::Empty,
        },
        sys: match filesystems.sys {
            SysFilesystem::None => SysPolicy::None,
        },
        dev: match filesystems.dev {
            DevFilesystem::None => DevPolicy::None,
            DevFilesystem::Minimal => DevPolicy::Minimal,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenMount {
    host: std::path::PathBuf,
    guest: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenSandbox {
    hostname: String,
    work_dir: std::path::PathBuf,
    mounts: Vec<FrozenMount>,
}

fn frozen_sandbox(paths: &Paths, plan: &DerivationPlan) -> Result<FrozenSandbox, Error> {
    if !matches!(plan.execution.credentials, ExecutionCredentials::IsolatedRoot) {
        return Err(Error::FrozenCredentialPolicyMismatch {
            found: plan.execution.credentials.as_str(),
        });
    }
    if paths.layout() != &plan.layout {
        return Err(Error::FrozenLayoutMismatch);
    }
    Ok(FrozenSandbox {
        hostname: plan.layout.hostname.clone(),
        work_dir: plan.layout.build_dir.clone().into(),
        mounts: frozen_mounts(
            paths,
            &plan.layout,
            plan.execution.compiler_cache,
            plan.derivation_id().as_str(),
        )?,
    })
}

fn frozen_mounts(
    paths: &Paths,
    layout: &BuilderLayout,
    compiler_cache: bool,
    derivation_id: &str,
) -> Result<Vec<FrozenMount>, Error> {
    let mut mounts = vec![
        FrozenMount {
            host: paths.artefacts().host,
            guest: layout.artifacts_dir.clone().into(),
        },
        FrozenMount {
            host: paths.build().host,
            guest: layout.build_dir.clone().into(),
        },
    ];
    if compiler_cache {
        mounts.extend(layout.cache_destinations().map(|(name, guest)| FrozenMount {
            host: paths.derivation_cache_host(derivation_id, name),
            guest: guest.into(),
        }));
    }
    mounts
        .into_iter()
        .map(|mount| {
            moss::util::ensure_dir_exists(&mount.host)?;
            Ok(mount)
        })
        .collect()
}

fn run<E>(paths: &Paths, networking: bool, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let rootfs = paths.rootfs().host;
    let artefacts = paths.artefacts();
    let build = paths.build();
    let compiler = paths.ccache();
    let gocache = paths.gocache();
    let gomodcache = paths.gomodcache();
    let cargocache = paths.cargocache();
    let zigcache = paths.zigcache();
    let rustc_wrapper = paths.sccache();
    let recipe = paths.recipe();

    let container = Container::new(rootfs)
        .hostname(&paths.layout().hostname)
        .networking(networking)
        .ignore_host_sigint(true)
        .work_dir(&build.guest)
        .bind_rw(&artefacts.host, &artefacts.guest)
        .bind_rw(&build.host, &build.guest)
        .bind_rw(&compiler.host, &compiler.guest)
        .bind_rw(&gocache.host, &gocache.guest)
        .bind_rw(&gomodcache.host, &gomodcache.guest)
        .bind_rw(&cargocache.host, &cargocache.guest)
        .bind_rw(&zigcache.host, &zigcache.guest)
        .bind_rw(&rustc_wrapper.host, &rustc_wrapper.guest)
        .bind_ro(&recipe.host, &recipe.guest);

    container.run::<E>(f)?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Container(#[from] container::Error),
    #[error("prepare frozen mount")]
    Mount(#[from] std::io::Error),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
    #[error("frozen execution requires credential policy `isolated-root`, found `{found}`")]
    FrozenCredentialPolicyMismatch { found: &'static str },
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stone_recipe::derivation::ProcFilesystem;

    use super::*;
    use crate::{BuildPolicy, Recipe, package};

    fn non_default_layout() -> BuilderLayout {
        let mut policy = BuildPolicy::repository_for_tests();
        policy.spec.sandbox.hostname = "forge-builder".to_owned();
        policy.spec.sandbox.guest_root = "/forge".to_owned();
        policy.spec.sandbox.artifacts_dir = "/forge/output".to_owned();
        policy.spec.sandbox.build_dir = "/forge/work".to_owned();
        policy.spec.sandbox.source_dir = "/forge/sources".to_owned();
        policy.spec.sandbox.recipe_dir = "/forge/recipe".to_owned();
        policy.spec.sandbox.package_dir = "/forge/recipe/package".to_owned();
        policy.spec.sandbox.install_dir = "/forge/destination".to_owned();
        {
            let cache = &mut policy.spec.build_root.compiler_cache;
            cache.ccache_dir = "/forge/cache-cc".to_owned();
            cache.sccache_dir = "/forge/cache-rust".to_owned();
            cache.go_cache_dir = "/forge/cache-go".to_owned();
            cache.go_mod_cache_dir = "/forge/cache-go-mod".to_owned();
            cache.cargo_cache_dir = "/forge/cache-cargo".to_owned();
            cache.zig_cache_dir = "/forge/cache-zig".to_owned();
        }
        policy.spec.validate().unwrap();
        BuilderLayout::from_policy(&policy.spec.sandbox, &policy.spec.build_root.compiler_cache)
    }

    #[test]
    fn frozen_filesystems_override_legacy_container_mounts() {
        let frozen = FilesystemPolicy {
            proc: ProcFilesystem::None,
            tmp: TmpFilesystem::Empty,
            sys: SysFilesystem::None,
            dev: DevFilesystem::None,
        };

        let mapped = frozen_pseudo_filesystems(frozen);
        assert_eq!(mapped.proc, ProcPolicy::None);
        assert_eq!(mapped.tmp, TmpPolicy::Empty);
        assert_eq!(mapped.sys, SysPolicy::None);
        assert_eq!(mapped.dev, DevPolicy::None);
        assert_ne!(mapped, PseudoFilesystemPolicy::default());
        assert_eq!(frozen_loopback_policy(), LoopbackPolicy::KernelDefault);
    }

    #[test]
    fn frozen_minimal_dev_is_exact_and_sys_is_absent() {
        let mapped = frozen_pseudo_filesystems(FilesystemPolicy::default());

        assert_eq!(mapped.proc, ProcPolicy::None);
        assert_eq!(mapped.tmp, TmpPolicy::Empty);
        assert_eq!(mapped.sys, SysPolicy::None);
        assert_eq!(mapped.dev, DevPolicy::Minimal);
        assert_eq!(::container::MINIMAL_DEV_NODES, ["null", "zero", "full"]);
    }

    #[test]
    fn frozen_container_excludes_recipe_and_disabled_global_caches() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = package::test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();

        let disabled = frozen_mounts(&paths, &plan.layout, false, "derivation-id").unwrap();
        assert_eq!(disabled.len(), 2);
        assert!(!disabled.iter().any(|mount| mount.host == paths.recipe().host));

        let enabled = frozen_mounts(&paths, &plan.layout, true, "derivation-id").unwrap();
        assert_eq!(enabled.len(), 8);
        assert!(
            enabled
                .iter()
                .skip(2)
                .all(|mount| mount.host.starts_with(runtime.path().join("derivations/derivation-id")))
        );
        assert!(!enabled.iter().any(|mount| mount.host == paths.recipe().host));
    }

    #[test]
    fn frozen_container_uses_non_default_policy_layout_as_one_authority() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let default_plan = package::test_derivation_plan();
        let default_id = default_plan.derivation_id();
        let mut plan = default_plan;
        plan.layout = non_default_layout();
        plan.execution.compiler_cache = true;
        plan.validate().unwrap();
        let derivation_id = plan.derivation_id();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();

        assert_ne!(default_id, derivation_id);
        assert_eq!(paths.install().guest, Path::new("/forge/destination"));
        assert_eq!(
            paths.install().host,
            paths.rootfs().host.join("forge").join("destination")
        );

        let sandbox = frozen_sandbox(&paths, &plan).unwrap();
        assert_eq!(sandbox.hostname, "forge-builder");
        assert_eq!(sandbox.work_dir, Path::new("/forge/work"));
        assert_eq!(
            sandbox
                .mounts
                .iter()
                .map(|mount| mount.guest.as_path())
                .collect::<Vec<_>>(),
            [
                Path::new("/forge/output"),
                Path::new("/forge/work"),
                Path::new("/forge/cache-cc"),
                Path::new("/forge/cache-rust"),
                Path::new("/forge/cache-go"),
                Path::new("/forge/cache-go-mod"),
                Path::new("/forge/cache-cargo"),
                Path::new("/forge/cache-zig"),
            ]
        );
        assert!(sandbox.mounts.iter().skip(2).all(|mount| {
            mount
                .host
                .starts_with(runtime.path().join("derivations").join(derivation_id.as_str()))
        }));
    }

    #[test]
    fn frozen_container_rejects_runtime_and_plan_layout_mismatch() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = package::test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.layout.hostname = "different-builder".to_owned();
        plan.validate().unwrap();

        assert!(matches!(
            frozen_sandbox(&paths, &plan),
            Err(Error::FrozenLayoutMismatch)
        ));
    }

    #[test]
    fn frozen_container_rejects_non_isolated_credentials() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = package::test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.execution.credentials = ExecutionCredentials::Unspecified;

        assert!(matches!(
            frozen_sandbox(&paths, &plan),
            Err(Error::FrozenCredentialPolicyMismatch { found: "unspecified" })
        ));
    }
}
