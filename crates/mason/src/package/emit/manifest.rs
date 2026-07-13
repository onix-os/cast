// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, io, io::Write};

use snafu::{ResultExt, Snafu};
use stone::relation::Dependency;
use stone_recipe::derivation::{DerivationId, PackageIdentity};

use super::Package;

mod binary;
mod json;

#[derive(Debug)]
pub struct Manifest<'a> {
    identity: &'a PackageIdentity,
    recipe_fingerprint: &'a str,
    build_deps: BTreeSet<Dependency>,
    packages: Vec<&'a Package<'a>>,
    derivation_id: DerivationId,
}

impl<'a> Manifest<'a> {
    pub fn new(
        identity: &'a PackageIdentity,
        recipe_fingerprint: &'a str,
        build_deps: impl IntoIterator<Item = Dependency>,
        derivation_id: &DerivationId,
    ) -> Self {
        Self {
            identity,
            recipe_fingerprint,
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

    pub fn write_binary<W: Write>(&self, output: &mut W) -> Result<(), Error> {
        binary::write(
            output,
            &self.packages,
            &self.build_deps,
            self.recipe_fingerprint,
            &self.derivation_id,
        )
        .context(BinarySnafu)
    }

    pub fn write_json<W: Write>(&self, output: &mut W) -> Result<(), Error> {
        json::write(
            output,
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
