// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;

use dag::Dag;
use thiserror::Error;

use crate::{Provider, Registry, package};

enum ProviderFilter {
    /// Must be installed
    Installed(Provider),

    /// Filter the lookup to current selection scope
    Selections(Provider),

    // Available in upstream repositories
    Available(Provider),
}

/// Dependency lookup strategy
#[derive(Clone, Copy, Debug, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Lookup {
    /// Lookup only installed packages
    InstalledOnly,
    /// Lookup only available packages
    AvailableOnly,
    /// Lookup installed packages first
    PreferInstalled,
    /// Lookup available packages first
    PreferAvailable,
}

/// A Transaction is used to modify one system state to another
#[derive(Clone, Debug)]
pub struct Transaction<'a> {
    /// Bound to a registry
    registry: &'a Registry,

    /// unique set of package ids
    packages: Dag<package::Id>,

    /// Dependency lookup strategy
    lookup: Lookup,

    /// Used as a cache to quickly resolve providers for things we've
    /// already added to the transaction so we don't have to hit the
    /// registry again
    selection_providers: HashMap<Provider, package::Id>,
}

/// Construct a new Transaction wrapped around the underlying [`Registry`].
///
/// At this point the registry is initialised and we can probe the installed
/// set.
pub fn new(registry: &Registry, lookup: Lookup) -> Result<Transaction<'_>, Error> {
    tracing::debug!("creating new transaction");
    Ok(Transaction {
        registry,
        packages: Dag::default(),
        lookup,
        selection_providers: HashMap::default(),
    })
}

impl Transaction<'_> {
    /// Remove a set of packages and their reverse dependencies
    pub fn remove(&mut self, packages: Vec<package::Id>) {
        // Get transposed subgraph
        let transposed = self.packages.transpose();
        let subgraph = transposed.subgraph(&packages);

        // For each node, remove it from transaction graph
        for package in subgraph.iter_nodes() {
            // Remove that package
            self.packages.remove_node(package);
        }
    }

    /// Return the package IDs in the fully baked configuration
    pub fn finalize(&self) -> impl Iterator<Item = &package::Id> + '_ {
        self.packages.topo()
    }

    /// Return the exact packages selected as direct dependencies of `package`.
    pub fn dependencies<'a>(&'a self, package: &package::Id) -> impl Iterator<Item = &'a package::Id> + 'a {
        self.packages.successors(package)
    }

    /// Update internal package graph with all incoming packages & their deps
    #[tracing::instrument(skip_all, fields(lookup = %self.lookup))]
    pub fn add(&mut self, incoming: Vec<package::Id>) -> Result<(), Error> {
        let mut items = incoming;

        while !items.is_empty() {
            let mut next = vec![];
            for check_id in items {
                self.add_step(check_id, &mut next)?;
            }
            items = next;
        }

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(%check_id, check_name))]
    fn add_step(&mut self, check_id: package::Id, next: &mut Vec<package::Id>) -> Result<(), Error> {
        // Ensure node is added and get its index
        let check_node = self.packages.add_node_or_get_index(&check_id);

        // Grab this package in question
        let package = self.registry.by_id(&check_id)?.into_iter().next();
        let package = package.ok_or(Error::NoCandidate(check_id.to_string()))?;

        tracing::Span::current().record("check_name", package.meta.name.as_str());
        tracing::debug!(
            num_dependencies = package.meta.dependencies.len(),
            "added package to transaction"
        );

        // Cache each provider for the package being added to our transaction
        for provider in package.meta.providers {
            self.selection_providers.insert(provider, check_id.clone());
        }

        for dependency in package.meta.dependencies {
            let provider = Provider {
                kind: dependency.kind,
                name: dependency.name,
            };

            // Now get it resolved
            let search_id = self.resolve_provider(provider.clone())?;

            // Add dependency node
            let need_search = !self.packages.node_exists(&search_id);
            let dep_node = self.packages.add_node_or_get_index(&search_id);

            // No dag node for it previously
            if need_search {
                tracing::debug!(?search_id, "adding package to next");

                // Add this provider to the cache
                self.selection_providers.insert(provider, search_id.clone());

                next.push(search_id);
            }

            // Connect dependencies without silently collapsing a package
            // cycle into the same result as an already-present edge.
            self.packages
                .try_add_edge(check_node, dep_node)
                .map_err(|cycle| Error::DependencyCycle {
                    cycle: cycle.path.into_iter().map(|package| package.to_string()).collect(),
                })?;
        }

        Ok(())
    }

    // Try all strategies to resolve a provider for installation
    fn resolve_provider(&self, provider: Provider) -> Result<package::Id, Error> {
        match self.lookup {
            Lookup::InstalledOnly => self.resolve_filters([
                ProviderFilter::Selections(provider.clone()),
                ProviderFilter::Installed(provider),
            ]),
            Lookup::AvailableOnly => self.resolve_filters([
                ProviderFilter::Selections(provider.clone()),
                ProviderFilter::Available(provider),
            ]),
            Lookup::PreferInstalled => self.resolve_filters([
                ProviderFilter::Selections(provider.clone()),
                ProviderFilter::Installed(provider.clone()),
                ProviderFilter::Available(provider),
            ]),
            Lookup::PreferAvailable => self.resolve_filters([
                ProviderFilter::Selections(provider.clone()),
                ProviderFilter::Available(provider.clone()),
                ProviderFilter::Installed(provider),
            ]),
        }
    }

    fn resolve_filters<const N: usize>(&self, filters: [ProviderFilter; N]) -> Result<package::Id, Error> {
        let mut missing = None;
        for filter in filters {
            match self.resolve_provider_with_filter(filter) {
                Ok(package) => return Ok(package),
                Err(Error::NoCandidate(provider)) => missing = Some(provider),
                Err(error) => return Err(error),
            }
        }
        Err(Error::NoCandidate(missing.unwrap_or_default()))
    }

    /// Attempt to resolve the filterered provider
    fn resolve_provider_with_filter(&self, filter: ProviderFilter) -> Result<package::Id, Error> {
        match filter {
            ProviderFilter::Available(provider) => self
                .registry
                .by_provider_id_only(&provider, package::Flags::new().with_available())?
                .into_iter()
                .next()
                .ok_or(Error::NoCandidate(provider.to_string())),
            ProviderFilter::Installed(provider) => self
                .registry
                .by_provider_id_only(&provider, package::Flags::new().with_installed())?
                .into_iter()
                .next()
                .ok_or(Error::NoCandidate(provider.to_string())),
            ProviderFilter::Selections(provider) => self
                .selection_providers
                .get(&provider)
                .cloned()
                .ok_or(Error::NoCandidate(provider.to_string())),
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("No such name: {0}")]
    NoCandidate(String),

    #[error("Not yet implemented")]
    NotImplemented,

    #[error("package dependency cycle: {}", cycle.join(" -> "))]
    DependencyCycle { cycle: Vec<String> },

    #[error("meta db")]
    Database(#[from] crate::db::meta::Error),

    #[error("registry query")]
    Registry(#[from] super::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{Dependency, Package, registry::Plugin};

    fn package(name: &'static str, dependency: &'static str) -> Package {
        Package {
            id: package::Id::from(name),
            meta: package::Meta {
                name: package::Name::from(name.to_owned()),
                version_identifier: "1".to_owned(),
                source_release: 1,
                build_release: 1,
                architecture: "x86_64".to_owned(),
                summary: String::new(),
                description: String::new(),
                source_id: name.to_owned(),
                homepage: String::new(),
                licenses: Vec::new(),
                dependencies: BTreeSet::from([Dependency::from_name(dependency).unwrap()]),
                providers: BTreeSet::from([Provider::from_name(name).unwrap()]),
                conflicts: BTreeSet::new(),
                uri: None,
                hash: None,
                download_size: None,
            },
            flags: package::Flags::new().with_available(),
        }
    }

    #[test]
    fn dependency_cycle_reports_the_closing_package_path() {
        let mut registry = Registry::default();
        registry.add_plugin(Plugin::Test(crate::registry::plugin::Test::new(
            1,
            vec![package("a", "b"), package("b", "c"), package("c", "a")],
        )));
        let mut transaction = registry.transaction(Lookup::AvailableOnly).unwrap();

        let error = transaction.add(vec![package::Id::from("a")]).unwrap_err();

        assert!(matches!(
            error,
            Error::DependencyCycle { ref cycle }
                if cycle.iter().map(String::as_str).eq(["c", "a", "b", "c"])
        ));
        assert_eq!(error.to_string(), "package dependency cycle: c -> a -> b -> c");
    }
}
