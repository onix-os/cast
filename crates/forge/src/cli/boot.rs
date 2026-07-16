// SPDX-FileCopyrightText: 2024 AerynOS Developers

use clap::{ArgMatches, Command};
use thiserror::Error;

use crate::{Client, Installation, client, environment};

pub fn command() -> Command {
    Command::new("boot")
        .about("Boot management")
        .long_about("Manage boot configuration")
        .subcommand_required(true)
        .subcommand(Command::new("status").about("Status of boot configuration"))
        .subcommand(Command::new("sync").about("Synchronize boot configuration"))
}

/// Handle status for now
pub fn handle(args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    match args.subcommand() {
        Some(("status", args)) => status(args, installation, verbose),
        Some(("sync", args)) => sync(args, installation, verbose),
        _ => unreachable!(),
    }
}

fn status(_args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose).map_err(Error::Client)?;

    client.print_boot_status()?;

    Ok(())
}

fn sync(_args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    client.synchronize_boot()?;

    println!("Boot updated\n");

    client.print_boot_status()?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("client")]
    Client(#[from] client::Error),
}
