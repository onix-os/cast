// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use container::Container;
use stone_recipe::derivation::{DerivationPlan, NetworkMode};
use thiserror::Error;

use crate::Paths;

pub fn exec<E>(paths: &Paths, networking: bool, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    run(paths, networking, f)
}

/// Execute a frozen plan without exposing mutable recipe, verification, or
/// global cache inputs to build steps.
pub fn exec_frozen<E>(paths: &Paths, plan: &DerivationPlan, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let rootfs = paths.rootfs().host;
    let build = paths.build();
    let mut container = Container::new(rootfs)
        .hostname("boulder")
        .networking(matches!(plan.execution.network, NetworkMode::Enabled))
        .ignore_host_sigint(true)
        .work_dir(&build.guest);

    for mount in frozen_mounts(paths, plan.execution.compiler_cache, plan.derivation_id().as_str())? {
        container = container.bind_rw(&mount.host, &mount.guest);
    }
    container.run::<E>(f)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenMount {
    host: std::path::PathBuf,
    guest: std::path::PathBuf,
}

fn frozen_mounts(paths: &Paths, compiler_cache: bool, derivation_id: &str) -> Result<Vec<FrozenMount>, Error> {
    let mut mappings = vec![paths.artefacts(), paths.build()];
    if compiler_cache {
        mappings.extend(
            ["ccache", "gocache", "gomodcache", "cargocache", "zigcache", "sccache"]
                .map(|name| paths.derivation_cache(derivation_id, name)),
        );
    }
    mappings
        .into_iter()
        .map(|mapping| {
            moss::util::ensure_dir_exists(&mapping.host)?;
            Ok(FrozenMount {
                host: mapping.host,
                guest: mapping.guest,
            })
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
    let ccache_conf = paths.ccache_config();

    let mut container = Container::new(rootfs)
        .hostname("boulder")
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
        .bind_ro(&recipe.host, &recipe.guest)
        .bind_ro_if_exists(&ccache_conf.host, &ccache_conf.guest);

    if let Some(manifest) = paths.verify_manifest() {
        container = container.bind_ro(&manifest.host, &manifest.guest);
    }

    container.run::<E>(f)?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Container(#[from] container::Error),
    #[error("prepare frozen mount")]
    Mount(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::Recipe;

    #[test]
    fn frozen_container_excludes_recipe_verify_and_disabled_caches() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let verify = runtime.path().join("manifest.bin");
        fs_err::write(&verify, b"verify").unwrap();
        let paths = Paths::new(&recipe, Some(verify), runtime.path(), "/mason", output.path()).unwrap();

        let disabled = frozen_mounts(&paths, false, "derivation-id").unwrap();
        assert_eq!(disabled.len(), 2);
        assert!(!disabled.iter().any(|mount| {
            mount.host == paths.recipe().host || paths.verify_manifest().is_some_and(|verify| mount.host == verify.host)
        }));

        let enabled = frozen_mounts(&paths, true, "derivation-id").unwrap();
        assert_eq!(enabled.len(), 8);
        assert!(
            enabled
                .iter()
                .skip(2)
                .all(|mount| mount.host.starts_with(runtime.path().join("derivations/derivation-id")))
        );
        assert!(!enabled.iter().any(|mount| mount.host == PathBuf::from("/etc/ccache")));
    }
}
