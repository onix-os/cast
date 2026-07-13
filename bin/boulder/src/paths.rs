// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use fs_err::File;
use moss::util;
use nix::fcntl::{FlockArg, flock};
use stone_recipe::derivation::{BuilderLayout, DerivationPlan};

use crate::Recipe;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Id {
    Recipe(String),
    Derivation(String),
}

impl Id {
    fn recipe(recipe: &Recipe) -> Self {
        Self::Recipe(format!(
            "{}-{}-{}",
            recipe.declaration.meta.pname, recipe.declaration.meta.version, recipe.declaration.meta.release
        ))
    }

    fn derivation(plan: &DerivationPlan) -> Self {
        Self::Derivation(plan.derivation_id().to_string())
    }

    fn value(&self) -> &str {
        match self {
            Self::Recipe(value) | Self::Derivation(value) => value,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    id: Id,
    host_root: PathBuf,
    layout: BuilderLayout,
    recipe_dir: PathBuf,
    output_dir: PathBuf,
}

impl Paths {
    pub fn new(
        recipe: &Recipe,
        layout: BuilderLayout,
        host_root: impl Into<PathBuf>,
        output_dir: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        recipe
            .declaration
            .validate()
            .map_err(|error| invalid_binding(format!("invalid package identity before path creation: {error}")))?;
        let id = Id::recipe(recipe);

        let recipe_dir = recipe.path.parent().unwrap_or(&PathBuf::default()).canonicalize()?;

        let job = Self {
            id,
            host_root: host_root.into().canonicalize()?,
            layout,
            recipe_dir,
            output_dir: output_dir.into(),
        };

        util::ensure_dir_exists(&job.rootfs().host)?;
        util::ensure_dir_exists(&job.artefacts().host)?;
        util::ensure_dir_exists(&job.build().host)?;
        util::ensure_dir_exists(&job.ccache().host)?;
        util::ensure_dir_exists(&job.gocache().host)?;
        util::ensure_dir_exists(&job.gomodcache().host)?;
        util::ensure_dir_exists(&job.cargocache().host)?;
        util::ensure_dir_exists(&job.zigcache().host)?;
        util::ensure_dir_exists(&job.sccache().host)?;
        util::ensure_dir_exists(&job.upstreams().host)?;

        Ok(job)
    }

    /// Bind paths used by frozen execution to the exact validated derivation.
    ///
    /// `Paths::new` intentionally retains the recipe-keyed workspace used by
    /// legacy/chroot operation. The planner calls this only after validating a
    /// frozen plan, before constructing its runtime.
    pub fn bind_to_plan(&mut self, plan: &DerivationPlan) -> io::Result<()> {
        if self.layout != plan.layout {
            return Err(invalid_binding(
                "runtime paths do not use the frozen plan's builder layout".to_owned(),
            ));
        }
        let expected = Id::derivation(plan);
        match &self.id {
            Id::Recipe(_) => self.id = expected,
            current if current == &expected => {}
            Id::Derivation(current) => {
                return Err(invalid_binding(format!(
                    "runtime paths are already bound to derivation {current}, not {}",
                    plan.derivation_id()
                )));
            }
        }

        util::ensure_dir_exists(&self.rootfs().host)?;
        util::ensure_dir_exists(&self.artefacts().host)?;
        util::ensure_dir_exists(&self.build().host)?;
        util::ensure_dir_exists(&self.execution_lock_dir())?;
        Ok(())
    }

    /// Require these paths to be bound to this exact plan.
    pub fn require_plan(&self, plan: &DerivationPlan) -> io::Result<()> {
        let expected = plan.derivation_id();
        match &self.id {
            Id::Derivation(current) if current == expected.as_str() => Ok(()),
            Id::Derivation(current) => Err(invalid_binding(format!(
                "runtime paths are bound to derivation {current}, not {expected}"
            ))),
            Id::Recipe(current) => Err(invalid_binding(format!(
                "recipe-keyed paths {current} are not bound to frozen derivation {expected}"
            ))),
        }
    }

    /// Stable host lock path shared by every execution of this derivation.
    pub fn execution_lock_path(&self, plan: &DerivationPlan) -> io::Result<PathBuf> {
        self.require_plan(plan)?;
        Ok(self.execution_lock_dir().join(format!("{}.lock", plan.derivation_id())))
    }

    /// Acquire the process-level lock that serializes identical derivations.
    pub fn acquire_execution_lock(&self, plan: &DerivationPlan) -> io::Result<ExecutionLock> {
        let path = self.execution_lock_path(plan)?;
        util::ensure_dir_exists(&self.execution_lock_dir())?;
        let file = File::options().create(true).write(true).truncate(false).open(&path)?;
        lock_exclusive(&file)?;
        Ok(ExecutionLock { _file: file, path })
    }

    pub fn require_execution_lock(&self, lock: &ExecutionLock, plan: &DerivationPlan) -> io::Result<()> {
        let expected = self.execution_lock_path(plan)?;
        if lock.path == expected {
            Ok(())
        } else {
            Err(invalid_binding(format!(
                "execution guard {:?} does not lock frozen derivation path {expected:?}",
                lock.path
            )))
        }
    }

    fn execution_lock_dir(&self) -> PathBuf {
        self.host_root.join("locks")
    }

    pub fn rootfs(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("root").join(self.id.value()),
            guest: "/".into(),
        }
    }

    pub fn artefacts(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("artefacts").join(self.id.value()),
            guest: self.layout.artifacts_dir.clone().into(),
        }
    }

    pub fn build(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("build").join(self.id.value()),
            guest: self.layout.build_dir.clone().into(),
        }
    }

    pub fn ccache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("ccache"),
            guest: self.layout.ccache_dir.clone().into(),
        }
    }

    pub fn gocache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("gocache"),
            guest: self.layout.go_cache_dir.clone().into(),
        }
    }

    pub fn gomodcache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("gomodcache"),
            guest: self.layout.go_mod_cache_dir.clone().into(),
        }
    }

    pub fn cargocache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("cargocache"),
            guest: self.layout.cargo_cache_dir.clone().into(),
        }
    }

    pub fn zigcache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("zigcache"),
            guest: self.layout.zig_cache_dir.clone().into(),
        }
    }

    pub fn sccache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("sccache"),
            guest: self.layout.sccache_dir.clone().into(),
        }
    }

    /// Cache mapping isolated by frozen derivation identity.
    pub fn derivation_cache_host(&self, derivation_id: &str, name: &str) -> PathBuf {
        self.host_root.join("derivations").join(derivation_id).join(name)
    }

    pub fn upstreams(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("upstreams"),
            guest: self.layout.source_dir.clone().into(),
        }
    }

    pub fn recipe(&self) -> Mapping {
        Mapping {
            host: self.recipe_dir.clone(),
            guest: self.layout.recipe_dir.clone().into(),
        }
    }

    pub fn install(&self) -> Mapping {
        let guest = PathBuf::from(&self.layout.install_dir);
        let host = self.guest_host_path(&Mapping {
            host: PathBuf::new(),
            guest: guest.clone(),
        });
        Mapping { host, guest }
    }

    /// For the provided [`Mapping`], return the guest
    /// path as it lives on the host fs
    ///
    /// Example:
    /// - host = "/var/cache/boulder/root/test"
    /// - guest = "/sandbox/build"
    /// - guest_host_path = "/var/cache/boulder/root/test/sandbox/build"
    pub fn guest_host_path(&self, mapping: &Mapping) -> PathBuf {
        let relative = mapping.guest.strip_prefix("/").unwrap_or(&mapping.guest);

        self.rootfs().host.join(relative)
    }

    /// Returns the output directory used for artefact syncing
    pub fn output_dir(&self) -> &PathBuf {
        &self.output_dir
    }

    pub fn layout(&self) -> &BuilderLayout {
        &self.layout
    }
}

pub struct Mapping {
    pub host: PathBuf,
    pub guest: PathBuf,
}

/// Held for the complete destructive setup/build/package/cleanup interval.
/// The kernel releases the flock when this guard is dropped.
#[derive(Debug)]
pub struct ExecutionLock {
    _file: File,
    path: PathBuf,
}

impl ExecutionLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[allow(deprecated)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    flock(file.as_raw_fd(), FlockArg::LockExclusive).map_err(errno_to_io)
}

fn errno_to_io(error: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn invalid_binding(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_derivation_plan;

    fn test_paths(root: &tempfile::TempDir, plan: &DerivationPlan) -> Paths {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let output = root.path().join("output");
        util::ensure_dir_exists(&output).unwrap();
        Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap()
    }

    #[test]
    fn frozen_workspaces_are_keyed_by_the_complete_derivation_id() {
        let root = tempfile::tempdir().unwrap();
        let first_plan = test_derivation_plan();
        let mut second_plan = first_plan.clone();
        second_plan.source_date_epoch += 1;
        second_plan.validate().unwrap();
        assert_eq!(first_plan.package, second_plan.package);
        assert_ne!(first_plan.derivation_id(), second_plan.derivation_id());

        let mut first = test_paths(&root, &first_plan);
        let mut second = test_paths(&root, &second_plan);
        first.bind_to_plan(&first_plan).unwrap();
        second.bind_to_plan(&second_plan).unwrap();

        assert_ne!(first.rootfs().host, second.rootfs().host);
        assert_ne!(first.build().host, second.build().host);
        assert_ne!(first.artefacts().host, second.artefacts().host);
        assert_eq!(
            first.rootfs().host.file_name().and_then(|name| name.to_str()),
            Some(first_plan.derivation_id().as_str())
        );
        assert_eq!(
            second.rootfs().host.file_name().and_then(|name| name.to_str()),
            Some(second_plan.derivation_id().as_str())
        );
        first.require_plan(&first_plan).unwrap();
        assert!(first.require_plan(&second_plan).is_err());
    }

    #[test]
    fn paths_remain_recipe_keyed_until_the_frozen_plan_is_bound() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let paths = test_paths(&root, &plan);

        assert!(matches!(&paths.id, Id::Recipe(_)));
        assert!(paths.require_plan(&plan).is_err());
    }

    #[test]
    fn invalid_recipe_identity_is_rejected_before_host_paths_are_created() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        recipe.declaration.meta.pname = "/tmp/boulder-path-escape".to_owned();
        let output = root.path().join("output");
        util::ensure_dir_exists(&output).unwrap();

        let error = Paths::new(&recipe, plan.layout, root.path(), output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!root.path().join("root").exists());
    }

    #[test]
    #[allow(deprecated)]
    fn execution_guard_exclusively_locks_the_derivation_path() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();

        let guard = paths.acquire_execution_lock(&plan).unwrap();
        assert_eq!(guard.path(), paths.execution_lock_path(&plan).unwrap());
        let contender = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(guard.path())
            .unwrap();
        assert_eq!(
            flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock),
            Err(nix::errno::Errno::EWOULDBLOCK)
        );

        drop(guard);
        flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
    }
}
