//! Read-only package closure resolution for build planning.

use thiserror::Error;

use crate::{Client, Package, Provider, dependency, package, registry::transaction};

/// Exact available package closure selected by Cast's ordinary transaction
/// resolver.
#[derive(Debug, Clone)]
pub struct AvailableClosure {
    pub requests: Vec<ResolvedRequest>,
    pub packages: Vec<ResolvedPackage>,
    /// Exact repository generations held stable for the complete resolution.
    pub repository_snapshots: Vec<crate::repository::IndexSnapshot>,
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
        // Retain one live active-state proof around every registry query and
        // the complete transaction. The transaction borrows the registry, so
        // copy its graph and metadata into owned values before releasing that
        // proof. In particular, do not call `resolve_package` here: it would
        // try to acquire a nested registry snapshot lease and deadlock on the
        // cooperating-writer coordinator.
        let (mut requests, resolved, stable_view) = self.with_registry_snapshot(|registry| -> Result<_, Error> {
            let stable_view = self.repositories.stable_snapshot_view()?;
            let mut roots = Vec::with_capacity(requested.len());
            let mut requests = Vec::with_capacity(requested.len());
            for request in requested {
                let provider = Provider::from_name(request)?;
                let package = registry
                    .by_provider(&provider, package::Flags::new().with_available())?
                    .into_iter()
                    .next()
                    .ok_or_else(|| Error::NoCandidate((*request).to_owned()))?;
                roots.push(package.id.clone());
                requests.push(ResolvedRequest {
                    request: (*request).to_owned(),
                    package: package.id,
                });
            }

            let mut transaction = registry.transaction(transaction::Lookup::AvailableOnly)?;
            transaction.add(roots)?;
            let ids = transaction.finalize().cloned().collect::<Vec<_>>();
            let mut resolved = Vec::with_capacity(ids.len());
            for id in ids {
                let package = registry
                    .by_id(&id)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| super::Error::MissingMetadata(id.clone()))?;
                let dependencies = transaction.dependencies(&id).cloned().collect::<Vec<_>>();
                resolved.push((package, dependencies));
            }

            Ok((requests, resolved, stable_view))
        })?;

        let repository_snapshots = stable_view.snapshots().to_vec();
        let mut packages = Vec::with_capacity(resolved.len());
        for (package, dependencies) in resolved {
            let id = package.id.clone();
            let repository = self
                .repositories
                .repository_for_package(&id)?
                .ok_or_else(|| Error::MissingRepository(id.clone()))?;
            packages.push(ResolvedPackage {
                package,
                repository,
                dependencies,
            });
        }
        packages.sort_by(|left, right| left.package.id.cmp(&right.package.id));

        requests.sort_by(|left, right| left.request.cmp(&right.request));
        Ok(AvailableClosure {
            requests,
            packages,
            repository_snapshots,
        })
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
    #[error(transparent)]
    Registry(#[from] crate::registry::Error),
}
