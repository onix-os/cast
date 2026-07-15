// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

mod gluon;
mod rooted_gluon;

pub use self::gluon::{
    DecodedGluon, DeleteGluonError, GENERATED_GLUON_MARKER, GluonCodec, GluonCodecError, GluonConversionError,
    LoadGluonError, LoadedGluonConfig, SaveGluonError,
};
pub use self::rooted_gluon::load_gluon_rooted;

pub trait Config {
    fn domain() -> String;
}

#[derive(Debug, Clone)]
pub struct Manager {
    scope: Scope,
}

impl Manager {
    /// Load vendor and administrator `.glu` configuration relative to `root`.
    pub fn system(root: impl Into<PathBuf>, program: impl ToString) -> Self {
        Self {
            scope: Scope::System {
                root: root.into(),
                program: program.to_string(),
            },
        }
    }

    /// Load system and user `.glu` configuration, writing generated fragments
    /// beneath the user's configuration directory.
    pub fn user(program: impl ToString) -> Result<Self, CreateUserError> {
        Ok(Self {
            scope: Scope::User {
                root: PathBuf::from("/"),
                config: dirs::config_dir().ok_or(CreateUserError)?,
                program: program.to_string(),
            },
        })
    }

    /// Load and save `.glu` fragments directly beneath `path`.
    pub fn custom(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: Scope::Custom(path.into()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CreateUserError;

impl fmt::Display for CreateUserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("$HOME or $XDG_CONFIG_HOME env not set")
    }
}

impl Error for CreateUserError {}

#[derive(Debug, Clone)]
enum Scope {
    System {
        program: String,
        root: PathBuf,
    },
    User {
        root: PathBuf,
        program: String,
        config: PathBuf,
    },
    Custom(PathBuf),
}

impl Scope {
    fn save_dir<'a>(&'a self, domain: &'a str) -> PathBuf {
        match &self {
            Scope::System { root, program } => Resolve::System {
                root,
                base: SystemBase::Admin,
                program,
            },
            Scope::User { config, program, .. } => Resolve::User { config, program },
            Scope::Custom(dir) => Resolve::Custom(dir),
        }
        .dir(domain)
    }

    fn load_with(&self) -> Vec<(Entry, Resolve<'_>)> {
        match &self {
            // System configuration merges vendor then administrator layers.
            Scope::System { root, program } => vec![
                (
                    Entry::File,
                    Resolve::System {
                        root,
                        base: SystemBase::Vendor,
                        program,
                    },
                ),
                (
                    Entry::Directory,
                    Resolve::System {
                        root,
                        base: SystemBase::Vendor,
                        program,
                    },
                ),
                (
                    Entry::File,
                    Resolve::System {
                        root,
                        base: SystemBase::Admin,
                        program,
                    },
                ),
                (
                    Entry::Directory,
                    Resolve::System {
                        root,
                        base: SystemBase::Admin,
                        program,
                    },
                ),
            ],
            // User configuration adds its layer after vendor and administrator.
            Scope::User { root, config, program } => vec![
                (
                    Entry::File,
                    Resolve::System {
                        root,
                        base: SystemBase::Vendor,
                        program,
                    },
                ),
                (
                    Entry::Directory,
                    Resolve::System {
                        root,
                        base: SystemBase::Vendor,
                        program,
                    },
                ),
                (
                    Entry::File,
                    Resolve::System {
                        root,
                        base: SystemBase::Admin,
                        program,
                    },
                ),
                (
                    Entry::Directory,
                    Resolve::System {
                        root,
                        base: SystemBase::Admin,
                        program,
                    },
                ),
                (Entry::File, Resolve::User { config, program }),
                (Entry::Directory, Resolve::User { config, program }),
            ],
            Scope::Custom(root) => vec![
                (Entry::File, Resolve::Custom(root)),
                (Entry::Directory, Resolve::Custom(root)),
            ],
        }
    }
}

#[derive(Clone, Copy)]
enum SystemBase {
    Admin,
    Vendor,
}

impl SystemBase {
    fn path(&self) -> &'static str {
        match self {
            SystemBase::Admin => "etc",
            SystemBase::Vendor => "usr/share",
        }
    }
}

enum Entry {
    File,
    Directory,
}

enum Resolve<'a> {
    System {
        root: &'a Path,
        base: SystemBase,
        program: &'a str,
    },
    User {
        config: &'a Path,
        program: &'a str,
    },
    Custom(&'a Path),
}

impl Resolve<'_> {
    fn config_dir(&self) -> PathBuf {
        match self {
            Resolve::System { root, base, program } => root.join(base.path()).join(program),
            Resolve::User { config, program } => config.join(program),
            Resolve::Custom(dir) => dir.to_path_buf(),
        }
    }

    fn file(&self, domain: &str, extension: &str) -> PathBuf {
        self.config_dir().join(format!("{domain}.{extension}"))
    }

    fn dir(&self, domain: &str) -> PathBuf {
        self.config_dir().join(format!("{domain}.d"))
    }
}
