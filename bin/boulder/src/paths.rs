// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::PathBuf};

use derive_more::Debug;
use moss::util;
use stone_recipe::derivation::BuilderLayout;

use crate::Recipe;

#[derive(Debug, Clone)]
#[debug("{_0:?}")]
pub struct Id(String);

impl Id {
    pub fn new(recipe: &Recipe) -> Self {
        Self(format!(
            "{}-{}-{}",
            recipe.declaration.meta.pname, recipe.declaration.meta.version, recipe.declaration.meta.release
        ))
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
        let id = Id::new(recipe);

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

    pub fn rootfs(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("root").join(&self.id.0),
            guest: "/".into(),
        }
    }

    pub fn artefacts(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("artefacts").join(&self.id.0),
            guest: self.layout.artifacts_dir.clone().into(),
        }
    }

    pub fn build(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("build").join(&self.id.0),
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
