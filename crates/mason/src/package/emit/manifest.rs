// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, io, path::PathBuf};

use fs_err as fs;
use snafu::{ResultExt, Snafu};
use stone::relation::Dependency;
use stone_recipe::derivation::{DerivationId, PackageIdentity};

use crate::{Architecture, Paths};

use super::Package;

mod binary;
mod json;

#[derive(Debug)]
pub struct Manifest<'a> {
    identity: &'a PackageIdentity,
    recipe_fingerprint: &'a str,
    arch: Architecture,
    output_dir: PathBuf,
    build_deps: BTreeSet<Dependency>,
    packages: Vec<&'a Package<'a>>,
    derivation_id: DerivationId,
}

impl<'a> Manifest<'a> {
    pub fn new(
        paths: &Paths,
        identity: &'a PackageIdentity,
        recipe_fingerprint: &'a str,
        build_deps: impl IntoIterator<Item = Dependency>,
        arch: Architecture,
        derivation_id: &DerivationId,
    ) -> Self {
        let output_dir = paths.artefacts().guest;

        Self {
            identity,
            recipe_fingerprint,
            output_dir,
            arch,
            build_deps: build_deps.into_iter().collect(),
            packages: Vec::new(),
            derivation_id: derivation_id.clone(),
        }
    }

    pub fn add_package(&mut self, package: &'a Package<'_>) {
        debug_assert!(self.packages.iter().all(|existing| existing.name != package.name));
        self.packages.push(package);
        self.packages.sort_by_key(|package| package.name);
    }

    pub fn write_binary(&self) -> Result<(), Error> {
        let mut output = fs::File::create(self.output_dir.join(super::super::binary_manifest_filename(self.arch)))
            .context(IoSnafu)?;

        binary::write(
            &mut output,
            &self.packages,
            &self.build_deps,
            self.recipe_fingerprint,
            &self.derivation_id,
        )
        .context(BinarySnafu)
    }

    pub fn write_json(&self) -> Result<(), Error> {
        json::write(
            &self.output_dir.join(super::super::jsonc_manifest_filename(self.arch)),
            self.identity,
            self.recipe_fingerprint,
            &self.packages,
            &self.build_deps,
            &self.derivation_id,
        )
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("encode binary manifest"))]
    Binary { source: binary::Error },
    #[snafu(display("encode json"))]
    Json { source: serde_json::Error },
    #[snafu(display("io"))]
    Io { source: io::Error },
}
