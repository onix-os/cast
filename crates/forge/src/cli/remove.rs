// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::{Installation, client::Client, environment};
use tracing::instrument;

pub use crate::client::Error;

pub fn command() -> clap::Command {
    Command::command()
}

#[derive(Debug, Parser)]
#[command(
    name = "remove",
    visible_alias = "rm",
    about = "Remove packages",
    long_about = "Remove packages by name"
)]
pub struct Command {
    /// Packages to remove
    packages: Vec<String>,

    /// Simulate the operation (dry-run)
    #[arg(long)]
    dry_run: bool,
}

/// Handle execution of `cast remove`
#[instrument(skip_all)]
pub fn handle(args: &ArgMatches, installation: Installation, yes: bool, verbose: bool) -> Result<(), Error> {
    let command = Command::from_arg_matches(args).expect("validated by clap");

    let pkgs = command.packages.iter().map(String::as_str).collect::<Vec<_>>();
    let simulate = command.dry_run;

    let mut client = Client::for_cli(environment::NAME, installation, verbose)?;

    client.remove(&pkgs, yes, simulate)?;

    Ok(())
}
