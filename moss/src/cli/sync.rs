// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};
use moss::{Installation, client::Client, environment, runtime};
use tracing::instrument;

pub use moss::client::Error;

pub fn command() -> clap::Command {
    Command::command()
}

#[derive(Debug, Parser)]
#[command(
    name = "sync",
    visible_alias = "up",
    about = "Sync packages",
    long_about = "Sync package selections with candidates from the highest priority repository"
)]
pub struct Command {
    /// Update repositories before syncing
    #[arg(short, long)]
    update: bool,
    /// Blit this sync to the provided directory instead of the root
    ///
    /// This operation won't be captured as a new state
    #[arg(value_name = "dir", long = "to")]
    blit_target: Option<PathBuf>,

    /// Simulate the sync (dry-run)
    #[arg(long)]
    dry_run: bool,

    /// Sync against the provided Gluon system intent
    ///
    /// The supplied .glu expression is evaluated, and only its repositories and packages
    /// will be used to create the new state
    #[arg(value_name = "system.glu", long)]
    import: Option<PathBuf>,
}

#[instrument(skip_all)]
pub fn handle(args: &ArgMatches, installation: Installation) -> Result<(), Error> {
    let command = Command::from_arg_matches(args).expect("validated by clap");

    let yes = *args.get_one::<bool>("yes").unwrap();
    let simulate = command.dry_run;
    let update = command.update;

    let mut client_builder = Client::builder(environment::NAME, installation);

    if let Some(path) = &command.import {
        client_builder = client_builder.system_intent_path(path);
    }

    // Make ephemeral if a blit target was provided
    if let Some(blit_target) = command.blit_target {
        client_builder = client_builder.ephemeral(blit_target);
    }

    let mut client = client_builder.build()?;

    // Update repos if requested
    if update {
        runtime::block_on(client.refresh_repositories())?;
    }

    client.sync(yes, simulate)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use fs_err as fs;
    use moss::system_model;

    use super::*;

    #[test]
    fn sync_import_cli_evaluates_authored_intent_for_an_ephemeral_target() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installation");
        let target = temporary.path().join("ephemeral-target");
        let intent = temporary.path().join("import.glu");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&target).unwrap();
        let authored = r#"// This source is owned by the caller.
let moss = import! moss.system.v1
{
    disable_warning = moss.boolean.true,
    .. moss.system
}
"#;
        fs::write(&intent, authored).unwrap();

        let matches = super::super::command()
            .try_get_matches_from([
                "moss",
                "--directory",
                root.to_str().unwrap(),
                "sync",
                "--import",
                intent.to_str().unwrap(),
                "--to",
                target.to_str().unwrap(),
                "--dry-run",
            ])
            .unwrap();
        let sync = matches.subcommand_matches("sync").unwrap();
        let installation = Installation::open(&root, None).unwrap();

        handle(sync, installation).unwrap();

        assert_eq!(fs::read_to_string(&intent).unwrap(), authored);
        assert!(!system_model::snapshot_path(&root).exists());
        assert!(!system_model::snapshot_path(&target).exists());
    }
}
