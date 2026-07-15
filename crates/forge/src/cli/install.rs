// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::{Installation, client::Client, environment};
use tracing::instrument;

pub use crate::client::Error;

pub fn command() -> clap::Command {
    Command::command()
}

#[derive(Debug, Parser)]
#[command(
    name = "install",
    visible_alias = "it",
    about = "Install packages",
    long_about = "Install packages by name"
)]
pub struct Command {
    /// Packages to install
    packages: Vec<String>,

    /// Simulate the operation (dry-run)
    #[arg(long)]
    dry_run: bool,

    /// Blit this sync to the provided directory instead of the root
    ///
    /// This operation won't be captured as a new state
    #[arg(value_name = "dir", long = "to")]
    blit_target: Option<PathBuf>,
}

/// Handle execution of `cast install`
#[instrument(skip_all)]
pub fn handle(args: &ArgMatches, installation: Installation, yes: bool, verbose: bool) -> Result<(), Error> {
    let command = Command::from_arg_matches(args).expect("validated by clap");

    let pkgs = command.packages.iter().map(String::as_str).collect::<Vec<_>>();
    let simulate = command.dry_run;

    // Grab a client for the root
    let mut client = Client::for_cli(environment::NAME, installation, verbose)?;

    // Make ephemeral if a blit target was provided
    if let Some(blit_target) = command.blit_target {
        client = client.ephemeral(blit_target)?;
    }

    client.install(&pkgs, yes, simulate)?;

    Ok(())
}
