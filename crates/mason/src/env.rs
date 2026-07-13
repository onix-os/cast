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

        util::ensure_dir_exists(&cache_dir)?;
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
    use super::*;

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
}
