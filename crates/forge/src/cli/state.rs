// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    Installation, State, SystemModel,
    client::{self, Client, prune},
    environment, state,
};
use chrono::Local;
use clap::{ArgAction, ArgMatches, Command, CommandFactory, FromArgMatches, Parser, arg};
use fs_err as fs;
use nix::unistd::gethostname;
use thiserror::Error;
use tui::Styled;

pub fn command() -> Command {
    Command::new("state")
        .about("Manage state")
        .long_about("Manage state ...")
        .subcommand_required(true)
        .subcommand(Command::new("active").about("List the active state"))
        .subcommand(Command::new("list").about("List all states"))
        .subcommand(
            Command::new("activate")
                .about("Activate a state")
                .arg(
                    arg!(<ID> "State id to be activated")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64)),
                )
                .arg(arg!(--"skip-triggers" "Do not run triggers on activation").action(ArgAction::SetTrue))
                .arg(arg!(--"skip-boot" "Do not sync boot on activation").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("query").about("Query information for a state").arg(
                arg!(<ID> "State id to query")
                    .action(ArgAction::Set)
                    .value_parser(clap::value_parser!(u64)),
            ),
        )
        .subcommand(
            Command::new("prune")
                .about("Prune archived states")
                .arg(
                    arg!(-k --keep "Keep this many states")
                        .action(ArgAction::Set)
                        .default_value("10")
                        .value_parser(clap::value_parser!(u64).range(1..)),
                )
                .arg(
                    arg!(--"include-newer" "Include states newer than the active state when pruning")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove archived state(s)")
                .arg(arg!(<ID> ... "State id(s) to be removed").value_parser(clap::value_parser!(String))),
        )
        .subcommand(
            Command::new("verify")
                .about("Verify and fix system states and assets")
                .arg(arg!(--verbose "Vebose output").action(ArgAction::SetTrue)),
        )
        .subcommand(Export::command())
        // For profiling only, hence hidden.
        //
        // Builds a VFS of the currently-active state, and throws it away again.
        // Run this through hyperfine / valgrind / heaptrack to profile the VFS
        // code.
        .subcommand(Command::new("build-vfs").hide(true))
}

#[derive(Debug, Parser)]
#[command(
    name = "export",
    about = "Export a state as a standalone generated system-model.glu snapshot"
)]
struct Export {
    /// State id to export or current state if omitted
    id: Option<i32>,
    /// Export to the provided path or stdout if not supplied
    ///
    /// If supplied without a path or path is a directory, outputs to "system-model-{hostname}-fstxn-{id}.glu"
    #[arg(short, long)]
    output: Option<Option<PathBuf>>,
}

pub fn handle(args: &ArgMatches, installation: Installation, yes: bool, verbose: bool) -> Result<(), Error> {
    match args.subcommand() {
        Some(("active", _)) => active(installation, verbose),
        Some(("list", _)) => list(installation, verbose),
        Some(("activate", args)) => activate(args, installation, verbose),
        Some(("build-vfs", _)) => build_vfs(installation, verbose),
        Some(("query", args)) => query(args, installation, verbose),
        Some(("prune", args)) => prune(args, installation, yes, verbose),
        Some(("remove", args)) => remove(args, installation, yes, verbose),
        Some(("verify", args)) => verify(args, installation, yes, verbose),
        Some(("export", args)) => export(args, installation, verbose),
        _ => unreachable!(),
    }
}

pub fn parse_id_or_range(s: &str) -> Result<Vec<u64>, String> {
    if let Some((start, end)) = s.split_once('-') {
        let start = start.parse::<u64>().map_err(|_| "invalid start")?;
        let end = end.parse::<u64>().map_err(|_| "invalid end")?;

        if start > end {
            return Err("range start must be <= end".into());
        }

        Ok((start..=end).collect())
    } else {
        Ok(vec![s.parse().map_err(|_| "invalid number")?])
    }
}

/// List the active state
pub fn active(installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    if let Some(state) = client.get_active_state()? {
        print_state(state);
    }

    Ok(())
}

/// List all known states, newest first
pub fn list(installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    for state in client.list_states()?.into_iter().rev() {
        print_state(state);
    }

    Ok(())
}

pub fn activate(args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let new_id = *args.get_one::<u64>("ID").unwrap() as i32;
    let skip_triggers = args.get_flag("skip-triggers");
    let skip_boot = args.get_flag("skip-boot");

    let client = Client::for_cli(environment::NAME, installation, verbose)?;
    let old_id = client.activate_state(new_id.into(), skip_triggers, skip_boot)?;

    println!(
        "State {} activated {}",
        new_id.to_string().bold(),
        format!("({old_id} archived)").dim()
    );

    Ok(())
}

pub fn build_vfs(installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    if let Some(state) = client.get_active_state()? {
        let fstree = client.vfs(state.selections.iter().map(|selection| &selection.package))?;

        std::hint::black_box(fstree);
    }

    Ok(())
}

pub fn query(args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let id = *args.get_one::<u64>("ID").unwrap() as i32;

    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    let state = client.get_state(id.into())?;

    print_state(state.clone());
    print_state_selections(state, &client)?;

    Ok(())
}

pub fn prune(args: &ArgMatches, installation: Installation, yes: bool, verbose: bool) -> Result<(), Error> {
    let keep = *args.get_one::<u64>("keep").unwrap();
    let include_newer = args.get_flag("include-newer");
    let client = Client::for_cli(environment::NAME, installation, verbose)?;
    client.prune_states(prune::Strategy::KeepRecent { keep, include_newer }, yes)?;

    Ok(())
}

pub fn remove(args: &ArgMatches, installation: Installation, yes: bool, verbose: bool) -> Result<(), Error> {
    let ids = args
        .get_many::<String>("ID")
        .into_iter()
        .flatten()
        .map(|s| parse_id_or_range(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::InvalidRange)?
        .into_iter()
        .flatten()
        .map(|id| state::Id::from(id as i32))
        .collect::<Vec<state::Id>>();

    let client = Client::for_cli(environment::NAME, installation, verbose)?;
    client.prune_states(prune::Strategy::Remove(&ids), yes)?;

    Ok(())
}

pub fn verify(args: &ArgMatches, installation: Installation, yes: bool, global_verbose: bool) -> Result<(), Error> {
    let verbose = global_verbose || args.get_flag("verbose");

    let client = Client::for_cli(environment::NAME, installation, global_verbose)?;
    client.verify(yes, verbose)?;

    Ok(())
}

fn export(args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let export = Export::from_arg_matches(args).expect("validate by clap");
    let client = Client::for_cli(environment::NAME, installation, verbose)?;

    let id = match export.id {
        Some(id) => state::Id::from(id),
        None => client.get_active_state()?.ok_or(Error::NoActiveState)?.id,
    };

    let system_model = client.export_state(id)?;

    match export.output {
        Some(maybe_path) => {
            let hostname = gethostname().ok().and_then(|hostname| hostname.into_string().ok());
            let filename = export_filename(id, hostname.as_deref());

            let path = match maybe_path {
                Some(path) => {
                    if path.is_dir() {
                        path.join(&filename)
                    } else {
                        path
                    }
                }
                None => Path::new(".").join(filename),
            };

            fs::write(&path, snapshot_content(&system_model))?;

            println!("Exported to {path:?}");
        }
        None => {
            println!("{}", snapshot_content(&system_model));
        }
    }

    Ok(())
}

fn export_filename(id: state::Id, hostname: Option<&str>) -> String {
    match hostname {
        Some(hostname) => format!("system-model-{hostname}-fstxn-{id}.glu"),
        None => format!("system-model-fstxn-{id}.glu"),
    }
}

fn snapshot_content(system_model: &SystemModel) -> &str {
    system_model.encoded()
}

/// Emit a state description for the TUI
fn print_state(state: State) {
    let local_time = state.created.with_timezone(&Local);
    let formatted_time = local_time.format("%Y-%m-%d %H:%M:%S %Z");

    println!(
        "State #{} - {}",
        state.id.to_string().bold(),
        state.summary.unwrap_or_else(|| String::from("system transaction"))
    );
    println!("{} {formatted_time}", "Created:".bold());
    if let Some(desc) = &state.description {
        println!("{} {desc}", "Description:".bold());
    }
    println!("{} {}", "Packages:".bold(), state.selections.len());
    println!();
}

fn print_state_selections(state: State, client: &Client) -> Result<(), Error> {
    let set = state
        .selections
        .into_iter()
        .map(|s| {
            let pkg = client.resolve_package(&s.package)?;

            Ok(Format {
                name: pkg.meta.name.to_string(),
                revision: Revision {
                    version: pkg.meta.version_identifier,
                    release: pkg.meta.source_release,
                },
                explicit: s.explicit,
            })
        })
        .collect::<Result<Vec<_>, client::Error>>()?;

    let max_length = set.iter().map(Format::size).max().unwrap_or_default() + 2;

    for item in set.clone() {
        let width = max_length - item.size() + 2;
        let name = if item.explicit {
            item.name.clone().bold()
        } else {
            item.name.clone().dim()
        };
        print!("{name} {:width$} ", " ");
        println!(
            "{}-{}",
            item.revision.version.magenta(),
            item.revision.release.to_string().dim(),
        );
    }
    println!();

    Ok(())
}

#[derive(Clone, Debug)]
struct Format {
    name: String,
    revision: Revision,
    explicit: bool,
}

impl Format {
    fn size(&self) -> usize {
        self.name.len() + self.revision.size()
    }
}

#[derive(Clone, Debug)]
struct Revision {
    version: String,
    release: u64,
}

impl Revision {
    fn size(&self) -> usize {
        self.version.len() + self.release.to_string().len()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("client")]
    Client(#[from] client::Error),
    #[error("db")]
    DB(#[from] crate::db::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("no active state")]
    NoActiveState,
    #[error("invalid state id or range: {0}")]
    InvalidRange(String),
}

#[cfg(test)]
mod tests {
    use crate::{Provider, repository, system_model};
    use gluon_config::Source;

    use super::*;

    #[test]
    fn export_filename_uses_the_gluon_snapshot_extension() {
        let id = state::Id::from(42);

        assert_eq!(export_filename(id, Some("host")), "system-model-host-fstxn-42.glu");
        assert_eq!(export_filename(id, None), "system-model-fstxn-42.glu");
    }

    #[test]
    fn exported_content_is_a_standalone_generated_snapshot() {
        let model = system_model::create(
            repository::Map::default(),
            [Provider::package_name("alpha")].into_iter().collect(),
        );
        let content = snapshot_content(&model);
        let evaluated =
            system_model::gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", content)).unwrap();

        assert!(content.starts_with(system_model::spec::GENERATED_GLUON_MARKER));
        assert!(!content.contains("import!"));
        assert!(evaluated.packages.contains(&Provider::package_name("alpha")));
    }
}
