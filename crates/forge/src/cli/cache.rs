// SPDX-FileCopyrightText: 2025 AerynOS Developers

use crate::{Client, Installation, client, environment};
use clap::{ArgMatches, Command};
use thiserror::Error;

pub(super) fn prune_command() -> Command {
    Command::new("prune").about("Prune cached artefacts").long_about(
        "Prune cached artefacts

This will remove all downloaded stones & unpacked asset data for packages not in any state or active repository.",
    )
}

pub(super) fn handle_prune(_args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose).map_err(Error::SetupClient)?;

    let num_removed_files = client.prune_cache().map_err(Error::PruneCache)?;

    if num_removed_files > 0 {
        let s = if num_removed_files > 1 { "s" } else { "" };

        println!("{num_removed_files} file{s} removed");
    } else {
        println!("No files to remove");
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to setup Cast client")]
    SetupClient(#[source] client::Error),
    #[error("failed to prune cache")]
    PruneCache(#[source] client::Error),
}
