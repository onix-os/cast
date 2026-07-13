// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Read-only package closure resolution for build planning.

use thiserror::Error;

use crate::{Client, Package, Provider, dependency, package, registry::transaction};

/// Exact available package closure selected by Cast's ordinary transaction
/// resolver.
#[derive(Debug, Clone)]
pub struct AvailableClosure {
    pub requests: Vec<ResolvedRequest>,
    pub packages: Vec<ResolvedPackage>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRequest {
    pub request: String,
    pub package: package::Id,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub package: Package,
    pub repository: crate::repository::Id,
    pub dependencies: Vec<package::Id>,
}

impl Client {
    /// Resolve provider requests without installing, fetching, or creating a
    /// state. The same registry transaction used by installation chooses the
    /// exact closure.
    pub fn resolve_available_closure(&self, requested: &[&str]) -> Result<AvailableClosure, Error> {
        let mut roots = Vec::with_capacity(requested.len());
        let mut requests = Vec::with_capacity(requested.len());
        for request in requested {
            let provider = Provider::from_name(request)?;
            let package = self
                .registry
                .by_provider(&provider, package::Flags::new().with_available())
                .next()
                .ok_or_else(|| Error::NoCandidate((*request).to_owned()))?;
            roots.push(package.id.clone());
            requests.push(ResolvedRequest {
                request: (*request).to_owned(),
                package: package.id,
            });
        }

        let mut transaction = self.registry.transaction(transaction::Lookup::AvailableOnly)?;
        transaction.add(roots)?;
        let ids = transaction.finalize().cloned().collect::<Vec<_>>();
        let mut packages = Vec::with_capacity(ids.len());
        for id in ids {
            let package = self.resolve_package(&id)?;
            let repository = self
                .repositories
                .repository_for_package(&id)?
                .ok_or_else(|| Error::MissingRepository(id.clone()))?;
            let dependencies = transaction.dependencies(&id).cloned().collect();
            packages.push(ResolvedPackage {
                package,
                repository,
                dependencies,
            });
        }
        packages.sort_by(|left, right| left.package.id.cmp(&right.package.id));

        requests.sort_by(|left, right| left.request.cmp(&right.request));
        Ok(AvailableClosure { requests, packages })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Provider(#[from] dependency::ParseError),
    #[error("no available package provides `{0}`")]
    NoCandidate(String),
    #[error("resolved package `{0}` has no active repository provenance")]
    MissingRepository(package::Id),
    #[error(transparent)]
    Transaction(#[from] transaction::Error),
    #[error(transparent)]
    Client(#[from] super::Error),
    #[error(transparent)]
    Repository(#[from] crate::repository::manager::Error),
}
