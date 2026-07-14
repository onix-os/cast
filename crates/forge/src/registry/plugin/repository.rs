// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use thiserror::Error;

use crate::{
    Provider, db,
    package::{self, Package},
    repository::{self, manager},
};

#[derive(Debug)]
pub struct Repository {
    active: repository::Cached,
}

impl Repository {
    pub fn new(active: repository::Cached) -> Self {
        Self { active }
    }

    pub fn priority(&self) -> u64 {
        self.active.repository.priority.into()
    }

    pub fn package(&self, id: &package::Id) -> Result<Option<Package>, QueryError> {
        let (snapshot, package) = self.active.db.get_with_active_snapshot(id)?;
        let snapshot = manager::verify_active_snapshot(&self.active, snapshot)?;

        Ok(match package {
            Some(meta) => Some(Package {
                id: id.clone(),
                meta: package::Meta {
                    // TODO: Is there a more type-safe way to do this vs mutation? Can
                    // a new type help here?
                    uri: meta
                        .uri
                        .and_then(|relative| snapshot.index_uri().join(&relative).ok())
                        .map(|url| url.to_string()),
                    ..meta
                },
                flags: package::Flags::new().with_available(),
            }),
            None => None,
        })
    }

    fn query(&self, flags: package::Flags, filter: Option<db::meta::Filter<'_>>) -> Result<Vec<Package>, QueryError> {
        if flags.available || flags == package::Flags::default() {
            let (snapshot, packages) = self.active.db.query_with_active_snapshot(filter)?;
            manager::verify_active_snapshot(&self.active, snapshot)?;

            Ok(packages
                .into_iter()
                .map(|(id, meta)| Package {
                    id,
                    meta,
                    flags: package::Flags::new().with_available(),
                })
                .collect())
        } else {
            Ok(vec![])
        }
    }

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

    pub fn query_name(&self, package_name: &package::Name, flags: package::Flags) -> Result<Vec<Package>, QueryError> {
        self.query(flags, Some(db::meta::Filter::Name(package_name.clone())))
    }

    pub fn query_provider_id_only(
        &self,
        provider: &Provider,
        flags: package::Flags,
    ) -> Result<Vec<package::Id>, QueryError> {
        if flags.available || flags == package::Flags::default() {
            let (snapshot, packages) = self.active.db.provider_packages_with_active_snapshot(provider)?;
            manager::verify_active_snapshot(&self.active, snapshot)?;
            Ok(packages)
        } else {
            Ok(vec![])
        }
    }
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("repository metadata query failed")]
    Database(#[from] db::meta::Error),
    #[error("repository immutable snapshot verification failed")]
    Integrity(#[from] manager::Error),
}

impl PartialEq for Repository {
    fn eq(&self, other: &Self) -> bool {
        self.active.id.eq(&other.active.id)
    }
}

impl Eq for Repository {}
