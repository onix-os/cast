// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    path::{Path, PathBuf},
};

use forge::util;
use nix::NixPath;
use thiserror::Error;

pub struct Env {
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub forge_dir: PathBuf,
    pub config: config::Manager,
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

        // Frozen build workspaces sit below this dedicated root. Bootstrap it
        // through a descriptor and normalize only safe/new directories so an
        // ambient umask cannot create a group-writable workspace and an
        // existing unsafe cache cannot be silently "repaired" in place.
        let cache_dir = crate::paths::prepare_private_workspace_root(&cache_dir)?;
        util::ensure_dir_exists(&data_dir)?;
        util::ensure_dir_exists(&forge_dir)?;

        Ok(Self {
            config,
            cache_dir,
            data_dir,
            forge_dir,
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
    fn dedicated_build_cache_root_is_exact_private_under_umask_0002() {
        if let Some(path) = std::env::var_os(UMASK_CHILD) {
            // This branch runs in an isolated test subprocess because umask is
            // process-global and changing it in the parallel test runner would
            // make unrelated filesystem tests nondeterministic.
            // SAFETY: the subprocess performs no concurrent setup before exit.
            unsafe { nix::libc::umask(0o002) };
            let root = crate::paths::prepare_private_workspace_root(Path::new(&path)).unwrap();
            assert_eq!(std::fs::metadata(root).unwrap().permissions().mode() & 0o7777, 0o700);
            return;
        }

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = temporary.path().join("cast/build");
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("env::test::dedicated_build_cache_root_is_exact_private_under_umask_0002")
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
}
