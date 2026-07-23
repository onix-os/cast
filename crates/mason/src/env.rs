// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use forge::util;
use nix::NixPath;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Env {
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub forge_dir: PathBuf,
    pub config: config::Manager,
    /// Exact cache roots selected during environment construction.
    ///
    /// These remain private so an `Env` cannot be fabricated with pathnames
    /// that have no matching authority. `Arc<File>` keeps cloning cheap while
    /// every clone retains the same kernel-pinned directory identity.
    pub(crate) cache_dir_anchor: Arc<File>,
    pub(crate) forge_dir_anchor: Arc<File>,
    /// Parallel `O_PATH` witnesses used to authenticate child-namespace
    /// reopening without discarding the retained policy handles above.
    pub(crate) cache_dir_path_anchor: Arc<File>,
    pub(crate) forge_dir_path_anchor: Arc<File>,
}

impl Env {
    pub fn new(
        cache_dir: Option<PathBuf>,
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        forge_root: Option<PathBuf>,
    ) -> Result<Self, Error> {
        let is_root = util::is_root();

        let config = if let Some(dir) = config_dir {
            config::Manager::custom(dir)
        } else if is_root {
            config::Manager::system("/", "cast")
        } else {
            config::Manager::user("cast")?
        };

        let cache_dir = resolve_cache_dir(is_root, cache_dir)?;
        let data_dir = resolve_data_dir(is_root, data_dir)?;
        let forge_dir = resolve_forge_root(is_root, forge_root)?;

        // Frozen build workspaces sit below a Mason-owned private cache root,
        // so safe existing cache roots are normalized to exact mode 0700.
        // Mason owns only creation of Forge's isolated resolution root: an
        // existing Forge root is left byte-for-byte and mode-for-mode unchanged
        // for Forge's broader root-owned/read-only policy to validate later.
        let (cache_dir, cache_dir_anchor) = crate::paths::prepare_private_workspace_root_pinned(&cache_dir)?;
        let (forge_dir, forge_dir_anchor) = crate::paths::prepare_missing_private_workspace_root_pinned(&forge_dir)?;
        let cache_dir_path_anchor = crate::paths::pin_matching_workspace_root(&cache_dir_anchor, &cache_dir)?;
        let forge_dir_path_anchor = crate::paths::pin_matching_workspace_root(&forge_dir_anchor, &forge_dir)?;
        util::ensure_dir_exists(&data_dir)?;

        Ok(Self {
            config,
            cache_dir,
            data_dir,
            forge_dir,
            cache_dir_anchor: Arc::new(cache_dir_anchor),
            forge_dir_anchor: Arc::new(forge_dir_anchor),
            cache_dir_path_anchor: Arc::new(cache_dir_path_anchor),
            forge_dir_path_anchor: Arc::new(forge_dir_path_anchor),
        })
    }
}

fn resolve_cache_dir(is_root: bool, custom: Option<PathBuf>) -> Result<PathBuf, Error> {
    if let Some(dir) = custom {
        Ok(dir)
    } else if is_root {
        Ok(PathBuf::from("/var/cache/cast/build"))
    } else {
        Ok(dirs::cache_dir().ok_or(Error::UserCache)?.join("cast/build"))
    }
}

fn resolve_data_dir(is_root: bool, custom: Option<PathBuf>) -> Result<PathBuf, Error> {
    let root_dir = PathBuf::from("/usr/share/cast");
    if let Some(dir) = custom {
        Ok(dir)
    } else if is_root {
        Ok(root_dir)
    } else {
        let user_datadir = dirs::data_dir().ok_or(Error::UserData)?.join("cast");
        if user_datadir.exists() && !user_datadir.is_empty() {
            Ok(user_datadir)
        } else {
            Ok(root_dir)
        }
    }
}

fn resolve_forge_root(is_root: bool, custom: Option<PathBuf>) -> Result<PathBuf, Error> {
    if let Some(dir) = custom {
        if dir == Path::new("/") {
            Err(Error::ForgeSystemRoot)
        } else {
            Ok(dir)
        }
    } else if is_root {
        Ok(PathBuf::from("/var/cache/cast/resolver"))
    } else {
        Ok(dirs::cache_dir().ok_or(Error::UserCache)?.join("cast/resolver"))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot find cache dir, $XDG_CACHE_HOME or $HOME env not set")]
    UserCache,
    #[error("cannot find config dir, $XDG_CONFIG_HOME or $HOME env not set")]
    UserConfig,
    #[error("cannot find data dir, $XDG_DATA_HOME or $HOME env not set")]
    UserData,
    #[error("Cast cannot use `/` as its isolated package-resolution root")]
    ForgeSystemRoot,
    #[error("io")]
    Io(#[from] io::Error),
}

impl From<config::CreateUserError> for Error {
    fn from(_: config::CreateUserError) -> Self {
        Error::UserConfig
    }
}

#[cfg(test)]
mod test {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    use super::*;

    const UMASK_CHILD: &str = "CAST_PRIVATE_CACHE_UMASK_CHILD";
    const FORGE_UMASK_CHILD: &str = "CAST_PRIVATE_FORGE_UMASK_CHILD";

    #[test]
    fn reject_forge_system_root() {
        assert!(matches!(
            resolve_forge_root(false, Some(PathBuf::from("/"))),
            Err(Error::ForgeSystemRoot)
        ));
        assert!(matches!(
            resolve_forge_root(true, Some(PathBuf::from("/"))),
            Err(Error::ForgeSystemRoot)
        ));
    }

    #[test]
    fn dedicated_build_cache_root_is_exact_private_under_umask_0777() {
        if let Some(path) = std::env::var_os(UMASK_CHILD) {
            // This branch runs in an isolated test subprocess because umask is
            // process-global and changing it in the parallel test runner would
            // make unrelated filesystem tests nondeterministic.
            // SAFETY: the subprocess performs no concurrent setup before exit.
            unsafe { nix::libc::umask(0o777) };
            let root = crate::paths::prepare_private_workspace_root(Path::new(&path)).unwrap();
            assert_eq!(std::fs::metadata(root).unwrap().permissions().mode() & 0o7777, 0o700);
            return;
        }

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = temporary.path().join("cast/build");
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("env::test::dedicated_build_cache_root_is_exact_private_under_umask_0777")
            .arg("--nocapture")
            .env(UMASK_CHILD, &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for path in [temporary.path().join("cast"), root] {
            assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o7777, 0o700);
        }
    }

    #[test]
    fn dedicated_build_cache_root_rejects_existing_unsafe_leaf_and_intermediate() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("cast");
        let leaf = parent.join("build");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&leaf).unwrap();
        std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o770)).unwrap();

        let error = crate::paths::prepare_private_workspace_root(&leaf).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o7777, 0o770);

        std::fs::remove_dir(&leaf).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o770)).unwrap();
        let error = crate::paths::prepare_private_workspace_root(&leaf).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!leaf.exists());
        assert_eq!(std::fs::metadata(parent).unwrap().permissions().mode() & 0o7777, 0o770);
    }

    #[test]
    fn env_prepares_missing_forge_root_as_private_under_umask_0777() {
        if let Some(path) = std::env::var_os(FORGE_UMASK_CHILD) {
            // umask is process-global, so exercise Env in one isolated exact
            // test process and exit without running concurrent setup.
            // SAFETY: this child runs only this test branch.
            unsafe { nix::libc::umask(0o777) };
            let parent = PathBuf::from(path);
            let env = Env::new(
                Some(parent.join("build")),
                Some(parent.join("config")),
                Some(parent.join("data")),
                Some(parent.join("forge")),
            )
            .unwrap();
            assert_eq!(
                std::fs::metadata(&env.forge_dir).unwrap().permissions().mode() & 0o7777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&env.cache_dir).unwrap().permissions().mode() & 0o7777,
                0o700
            );
            return;
        }

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("env::test::env_prepares_missing_forge_root_as_private_under_umask_0777")
            .arg("--nocapture")
            .env(FORGE_UMASK_CHILD, temporary.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::metadata(temporary.path().join("forge"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(temporary.path().join("build"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
    }

    #[test]
    fn env_leaves_existing_forge_root_completely_unchanged() {
        use std::os::unix::fs::MetadataExt;

        for mode in [0o755, 0o555] {
            let temporary = tempfile::tempdir().unwrap();
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let forge = temporary.path().join("forge");
            std::fs::create_dir(&forge).unwrap();
            std::fs::set_permissions(&forge, std::fs::Permissions::from_mode(mode)).unwrap();
            let before = std::fs::symlink_metadata(&forge).unwrap();

            let env = Env::new(
                Some(temporary.path().join("build")),
                Some(temporary.path().join("config")),
                Some(temporary.path().join("data")),
                Some(forge.clone()),
            )
            .unwrap();

            let after = std::fs::symlink_metadata(&forge).unwrap();
            assert_eq!(env.forge_dir, forge);
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            assert_eq!(after.permissions().mode() & 0o7777, mode);
        }
    }
}
