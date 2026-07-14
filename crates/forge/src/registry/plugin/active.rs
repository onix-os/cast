// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use thiserror::Error;

use crate::{Package, Provider, State, db, package};

// TODO:
#[derive(Debug, Clone)]
pub struct Active {
    state: Option<State>,
    db: db::meta::Database,
}

impl PartialEq for Active {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

impl Eq for Active {}

impl Active {
    /// Return a new Active plugin for the given state + install database
    pub fn new(state: Option<State>, db: db::meta::Database) -> Self {
        Self { state, db }
    }

    /// Query the given package
    pub fn package(&self, id: &package::Id) -> Result<Option<Package>, QueryError> {
        match self.db.get(id) {
            Ok(meta) => Ok(self
                .installed_package(id.clone())
                .map(|(id, flags)| Package { id, meta, flags })),
            Err(db::meta::Error::RowNotFound) => Ok(None),
            Err(error) => Err(QueryError::Database(error)),
        }
    }

    /// Query, restricted to state
    fn query(&self, flags: package::Flags, filter: Option<db::meta::Filter<'_>>) -> Result<Vec<Package>, QueryError> {
        if flags.installed || flags == package::Flags::default() {
            // TODO: Error handling
            let packages = self.db.query(filter)?;

            Ok(packages
                .into_iter()
                .filter_map(|(id, meta)| {
                    self.installed_package(id)
                        .map(|(id, flags)| Package { id, meta, flags })
                })
                // Filter for explicit only packages, if applicable
                .filter(|package| if flags.explicit { package.flags.explicit } else { true })
                .collect())
        } else {
            Ok(vec![])
        }
    }

    /// List, restricted to state
    pub fn list(&self, flags: package::Flags) -> Result<Vec<Package>, QueryError> {
        self.query(flags, None)
    }

    pub fn query_keyword(&self, keyword: &str, flags: package::Flags) -> Result<Vec<Package>, QueryError> {
        self.query(flags, Some(db::meta::Filter::Keyword(keyword)))
    }

    /// Query all packages that match the given provider identity
    pub fn query_provider(&self, provider: &Provider, flags: package::Flags) -> Result<Vec<Package>, QueryError> {
        self.query(flags, Some(db::meta::Filter::Provider(provider.clone())))
    }

    /// Query matching by name
    pub fn query_name(&self, package_name: &package::Name, flags: package::Flags) -> Result<Vec<Package>, QueryError> {
        self.query(flags, Some(db::meta::Filter::Name(package_name.clone())))
    }

    pub fn query_provider_id_only(
        &self,
        provider: &Provider,
        flags: package::Flags,
    ) -> Result<Vec<package::Id>, QueryError> {
        if flags.installed || flags == package::Flags::default() {
            // TODO: Error handling
            let packages = self.db.provider_packages(provider)?;

            Ok(packages
                .into_iter()
                .filter_map(|id| {
                    let (id, package_flags) = self.installed_package(id)?;
                    // Filter for explicit only packages, if applicable
                    if flags.explicit {
                        package_flags.explicit.then_some(id)
                    } else {
                        Some(id)
                    }
                })
                .collect())
        } else {
            Ok(vec![])
        }
    }

    pub fn priority(&self) -> u64 {
        u64::MAX
    }

    fn installed_package(&self, id: package::Id) -> Option<(package::Id, package::Flags)> {
        match &self.state {
            Some(st) => st
                .selections
                .iter()
                .find(|selection| selection.package == id)
                .map(|selection| {
                    (
                        id,
                        if selection.explicit {
                            package::Flags::new().with_installed().with_explicit()
                        } else {
                            package::Flags::new().with_installed()
                        },
                    )
                }),
            None => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("installed package metadata query failed")]
    Database(#[from] db::meta::Error),
}
