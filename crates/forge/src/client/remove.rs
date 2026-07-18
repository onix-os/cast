// SPDX-FileCopyrightText: 2026 AerynOS Developers

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use itertools::{Either, Itertools};
use thiserror::Error;
use tracing::{debug, info, instrument};
use tui::{
    Styled,
    dialoguer::{Confirm, theme::ColorfulTheme},
    pretty::autoprint_columns,
};

use crate::{Client, Provider, client, db, dependency, package, registry::transaction, state::Selection};

/// Remove a set of packages.
#[instrument(skip(client), fields(ephemeral = client.is_ephemeral()))]
pub fn remove(client: &mut Client, pkgs: &[&str], yes: bool, simulate: bool) -> Result<Timing, Error> {
    let mut timing = Timing::default();
    let mut instant = Instant::now();

    let requested = pkgs
        .iter()
        .map(|name| Provider::from_name(name))
        .collect::<Result<Vec<_>, _>>()?;

    let installed = client
        .with_registry_snapshot(|registry| -> Result<Vec<crate::Package>, Error> { Ok(registry.list_installed()?) })?;
    let installed_ids = installed.iter().map(|p| p.id.clone()).collect::<BTreeSet<_>>();

    // Separate packages between installed / not installed (or invalid)
    let (for_removal, not_installed): (Vec<_>, Vec<_>) = requested.into_iter().partition_map(|provider| {
        installed
            .iter()
            .find(|i| i.meta.providers.contains(&provider))
            .map(|i| Either::Left(i.id.clone()))
            .unwrap_or(Either::Right(provider.clone()))
    });

    // Reject the complete missing set as structured data instead of printing a
    // partial diagnostic and returning an uninformative unit error.
    if !not_installed.is_empty() {
        return Err(Error::PackagesNotInstalled(not_installed));
    }

    // First resolve a transaction where all requested packages are removed from the install
    //
    // This will remove those packages & any package that depends on it. This will not remove
    // the packages it depends on if they are orphaned (see next step).
    let tx_with_removed = client.with_registry_snapshot(|registry| -> Result<BTreeSet<package::Id>, Error> {
        // Add all installed packages to transaction
        let mut transaction = registry.transaction(transaction::Lookup::InstalledOnly)?;
        transaction.add(installed_ids.clone().into_iter().collect())?;

        // Remove all pkgs for removal
        transaction.remove(for_removal);

        // Finalized tx has all reverse deps removed
        Ok(transaction.finalize().cloned().collect())
    })?;

    // Build a new transaction w/ the leftover "explicit" packages. This will cause all orphaned
    // transitive dependencies to get dropped. These are packages that were depended on by removed
    // packages that are no longer depended on.
    let finalized = client.with_registry_snapshot(|registry| -> Result<BTreeSet<package::Id>, Error> {
        // Is an explicit package that still exists after removals
        let explicit_pkgs = installed
            .iter()
            .filter(|p| tx_with_removed.contains(&p.id) && p.flags.explicit)
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();

        let mut transaction = registry.transaction(transaction::Lookup::InstalledOnly)?;
        transaction.add(explicit_pkgs)?;

        Ok(transaction.finalize().cloned().collect())
    })?;

    // Resolve all removed packages, where removed is (installed - finalized)
    let removed = client.resolve_packages(installed_ids.difference(&finalized))?;

    // Prove that every retained transaction member is backed by the exact
    // active-state selection before presenting or simulating the operation.
    // A registry/database mismatch must never print a false removal success.
    let previous_selections = match client.active_state_for_planning()? {
        Some(id) => client.state_db.get(id)?.selections,
        None => vec![],
    };
    let new_state_pkgs = retain_previous_selections(finalized, &previous_selections)?;

    timing.resolve = instant.elapsed();
    info!(
        total_packages = removed.len(),
        packages_to_remove = removed.len(),
        resolve_time_ms = timing.resolve.as_millis(),
        "Package resolution for removal completed"
    );

    for package in &removed {
        debug!(
            name = %package.meta.name,
            version = %package.meta.version_identifier,
            source_release = package.meta.source_release,
            build_release = package.meta.build_release,
            "Package marked for removal"
        );
    }

    println!("The following package(s) will be removed:");
    println!();
    autoprint_columns(&removed);
    println!();

    if simulate {
        return Ok(timing);
    }

    let result = if yes {
        true
    } else {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(" Do you wish to continue? ")
            .default(false)
            .interact()?
    };
    if !result {
        return Err(Error::Cancelled);
    }

    instant = Instant::now();

    // Print each package to stdout
    for package in &removed {
        println!("{} {}", "Removed".red(), package.meta.name.as_str().bold());
    }

    // Apply state
    client.new_state(&new_state_pkgs, "Remove")?;

    timing.blit = instant.elapsed();

    info!(
        blit_time_ms = timing.blit.as_millis(),
        total_time_ms = (timing.resolve + timing.blit).as_millis(),
        "Removal completed successfully"
    );

    Ok(timing)
}

fn retain_previous_selections(
    finalized: impl IntoIterator<Item = package::Id>,
    previous: &[Selection],
) -> Result<Vec<Selection>, Error> {
    finalized
        .into_iter()
        .map(|id| {
            previous
                .iter()
                .find(|selection| selection.package == id)
                .cloned()
                .ok_or(Error::MissingPreviousSelection(id))
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cancelled")]
    Cancelled,

    #[error("requested packages are not installed: {0:?}")]
    PackagesNotInstalled(Vec<Provider>),

    #[error("client")]
    Client(#[from] client::Error),

    #[error("transaction")]
    Transaction(#[from] transaction::Error),

    #[error("registry query")]
    Registry(#[from] crate::registry::Error),

    #[error(transparent)]
    Provider(#[from] dependency::ParseError),

    #[error("db")]
    DB(#[from] db::Error),

    #[error("io")]
    Io(#[from] std::io::Error),

    #[error("string processing")]
    Dialog(#[from] tui::dialoguer::Error),

    #[error("resolved removal retained package {0}, but the active state has no matching selection")]
    MissingPreviousSelection(package::Id),
}

/// Simple timing information for Remove
#[derive(Default)]
pub struct Timing {
    pub resolve: Duration,
    pub blit: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inconsistent_removal_selection_fails_closed() {
        let retained = package::Id::from("retained-package");

        let error = retain_previous_selections([retained.clone()], &[]).unwrap_err();

        assert!(matches!(error, Error::MissingPreviousSelection(found) if found == retained));
    }
}
