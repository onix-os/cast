// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeMap, io, os::unix::fs::PermissionsExt as _, path::PathBuf, slice};

use thiserror::Error;
use tracing::{Instrument, instrument};

use crate::{
    Installation, Provider, Repository,
    client::{self, Client},
    db, dependency, installation,
    linux_fs::{normalize_new_directory, require_named_directory},
    package, repository, runtime,
    state::{self, Selection},
};

// TODO: Flesh this out before v0 -> v1 upgrade is in place
#[allow(unused, clippy::diverging_sub_expression)]
#[instrument(skip(client))]
pub fn self_upgrade(client: &mut Client, simulate: bool) -> Result<(), Error> {
    client.preflight_active_state_snapshot()?;
    client.require_non_frozen()?;
    // Ensure client is stateful
    if client.is_ephemeral() {
        return todo!("error can't self upgrade with epehemeral client");
    }

    // Get the previously installed Cast package.
    let installed = client
        .with_registry_snapshot(|registry| -> Result<Vec<crate::Package>, Error> { Ok(registry.list_installed()?) })?;
    let Some(previous_cast) = installed.into_iter().find(|p| p.meta.name.as_str() == "cast") else {
        return todo!("error can't self upgrade without cast installed in current state");
    };

    // Get the list of repos that are unsupported by this version of Cast.
    let Some(unsupported_repos) = runtime::block_on(
        async {
            match client.refresh_repositories().await {
                Err(client::Error::Repository(repository::manager::Error::UnsupportedRepos(repos))) => Ok(Some(repos)),
                Err(err) => Err(Error::Client(err)),
                Ok(_) => Ok(None),
            }
        }
        .in_current_span(),
    )?
    else {
        println!("cast already supports the latest format for all repositories");
        return Ok(());
    };

    // Tempdir to fetch intermediate `upgrade_via` index files
    // to so we can find the `cast` package from this index
    let temp_dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(Error::CreateTemporaryInstallationRoot)?;
    let temp_anchor =
        normalize_new_directory(temp_dir.path(), 0o700).map_err(|source| Error::PrepareTemporaryInstallationRoot {
            path: temp_dir.path().to_owned(),
            source,
        })?;

    // If multiple repos are unsupported and have Cast, upgrade
    // from the highest priority repo
    let mut cast_priority_map = BTreeMap::new();
    let mut missing_upgrade_via = vec![];
    let mut missing_cast = vec![];

    for unsupported_repo in unsupported_repos {
        match &unsupported_repo.upgrade_via_index_uri {
            Some(uri) => {
                require_named_directory(temp_dir.path(), &temp_anchor, 0o700).map_err(|source| {
                    Error::PrepareTemporaryInstallationRoot {
                        path: temp_dir.path().to_owned(),
                        source,
                    }
                })?;
                let temp_installation =
                    Installation::open(temp_dir.path(), None).map_err(|source| Error::OpenTemporaryInstallation {
                        path: temp_dir.path().to_owned(),
                        source,
                    })?;
                require_named_directory(temp_dir.path(), &temp_anchor, 0o700).map_err(|source| {
                    Error::PrepareTemporaryInstallationRoot {
                        path: temp_dir.path().to_owned(),
                        source,
                    }
                })?;
                let mut temp_client = Client::builder("temp-self-upgrade", temp_installation)
                    .repositories(repository::Map::from_iter([(
                        unsupported_repo.repository.id.clone(),
                        Repository {
                            description: "...".to_owned(),
                            source: repository::Source::DirectIndex(uri.clone()),
                            priority: 0.into(),
                            active: true,
                        },
                    )]))
                    .build()?;

                runtime::block_on(temp_client.ensure_repos_initialized().in_current_span())?;

                let packages = temp_client.lookup_packages_by_provider(
                    &Provider {
                        kind: dependency::Kind::PackageName,
                        name: "cast".to_owned(),
                    },
                    package::Flags::new().with_available(),
                )?;

                if let Some(package) = packages.first() {
                    cast_priority_map.insert(unsupported_repo.repository.repository.priority, package.clone());
                } else {
                    missing_cast.push(unsupported_repo.repository.id.clone());
                }
            }
            None => {
                missing_upgrade_via.push(unsupported_repo.repository.id.clone());
            }
        }
    }

    let Some(new_cast) = cast_priority_map.values().next_back() else {
        if !missing_cast.is_empty() {
            for repo in missing_cast {
                eprintln!("`cast` doesn't exist in repository {repo}");
            }
        }
        if !missing_upgrade_via.is_empty() {
            for repo in missing_upgrade_via {
                eprintln!("Repository {repo} is missing an `upgrade_via` format attribute");
            }
        }

        return todo!("error for no unsupported repo with upgrade_via cast available");
    };

    // Cache new Cast package.
    runtime::block_on(client.cache_packages(slice::from_ref(new_cast)).in_current_span())?;

    // Calculate the new state of packages (previous state - previous Cast + new Cast).
    let new_state_pkgs = {
        let mut previous_selections = match client.active_state_for_planning()? {
            Some(id) => {
                client
                    .state_db
                    .get(id)
                    .map_err(|err| Error::MissingActiveStateFromDb(err, id))?
                    .selections
            }
            _ => vec![],
        };

        let cast_selection = if let Some(idx) = previous_selections
            .iter()
            .position(|selection| selection.package == previous_cast.id)
        {
            // Remove the old Cast selection.
            let old_selection = previous_selections.remove(idx);

            // Update it w/ new id
            Selection {
                package: new_cast.id.clone(),
                ..old_selection
            }
        } else {
            return todo!("internal error, installed cast not listed as state selection");
        };

        // Add it
        previous_selections
            .into_iter()
            .chain(Some(cast_selection))
            .collect::<Vec<_>>()
    };

    // Perfect, apply state.
    client.new_state(&new_state_pkgs, "Self upgrade")?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("client")]
    Client(#[from] client::Error),

    #[error("registry query")]
    Registry(#[from] crate::registry::Error),

    #[error("get state {0} from state_db")]
    MissingActiveStateFromDb(#[source] db::Error, state::Id),

    #[error("create temporary self-upgrade installation root")]
    CreateTemporaryInstallationRoot(#[source] io::Error),

    #[error("prepare temporary self-upgrade installation root `{}`", path.display())]
    PrepareTemporaryInstallationRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("open temporary self-upgrade installation `{}`", path.display())]
    OpenTemporaryInstallation {
        path: PathBuf,
        #[source]
        source: installation::Error,
    },
}
