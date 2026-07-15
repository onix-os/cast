// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeMap, path::PathBuf};

#[cfg(test)]
use crate::system_model;
use crate::{
    Installation, Repository,
    client::{self, Client},
    environment,
    repository::{self, Priority},
    runtime,
};
use clap::{Arg, ArgAction, ArgMatches, Command, arg, builder::ValueParser};
use itertools::Itertools;
use thiserror::Error;
use tui::Styled;
use url::Url;

/// Control flow for the subcommands
enum Action {
    // Root
    List,
    // Root, Id, Url, Comment, Root index enabled options
    Add(String, Url, String, Priority, Option<RootIndexOptions>),
    // Root, Id
    Remove(String),
    // Root, Id
    Update(Option<String>),
    Enable(String),
    Disable(String),
}

/// Return a command for handling `repo` subcommands
pub fn command() -> Command {
    Command::new("repo")
        .about("Manage software repositories")
        .long_about("Manage the available software repositories visible to the installed system")
        .subcommand_required(true)
        .subcommand(
            Command::new("add")
                .visible_alias("ar")
                .arg(arg!(<NAME> "repo name").value_parser(clap::value_parser!(String)))
                .arg(arg!(<URI> "repo uri").value_parser(clap::value_parser!(Url)))
                .arg(
                    Arg::new("comment")
                        .short('c')
                        .default_value("...")
                        .action(ArgAction::Set)
                        .help("Set the comment for the repository")
                        .value_parser(clap::value_parser!(String)),
                )
                .arg(
                    Arg::new("priority")
                        .short('p')
                        .help("Repository priority")
                        .action(ArgAction::Set)
                        .default_value("0")
                        .value_parser(clap::value_parser!(u64)),
                )
                .next_help_heading("Root index")
                // TODO: Completely overhaul this CLI API, this is temporary to add support
                // initially for adding the new root index repo source without breaking
                // the current API
                .arg(
                    Arg::new("root-index")
                        .long("root-index")
                        .value_name("root-index-options")
                        .help(concat!(
                            "Defines the repo via root index options where <URI> is the base-uri ",
                            "and all other options are passed to this flag\n\n",
                            "Example: --root-index version=stream/unstable\n",
                            "Example: --root-index channel=testing,version=tag/some-bug",
                        ))
                        .action(ArgAction::Set)
                        .num_args(1)
                        .value_parser(ValueParser::new(parse_root_index_options)),
                ),
        )
        .subcommand(
            Command::new("list")
                .visible_alias("lr")
                .about("List system software repositories")
                .long_about("List all of the system repositories and their status"),
        )
        .subcommand(
            Command::new("remove")
                .visible_alias("rr")
                .about("Remove a repository for the system")
                .arg(arg!(<NAME> "repo name").value_parser(clap::value_parser!(String))),
        )
        .subcommand(
            Command::new("update")
                .visible_alias("ur")
                .about("Update the system repositories")
                .long_about("If no repository is named, update them all")
                .arg(arg!([NAME] "repo name").value_parser(clap::value_parser!(String))),
        )
        .subcommand(
            Command::new("enable")
                .visible_alias("er")
                .about("Enable the system repositories")
                .arg(arg!([NAME] "repo name").value_parser(clap::value_parser!(String))),
        )
        .subcommand(
            Command::new("disable")
                .visible_alias("dr")
                .about("Disable the system repositories")
                .arg(arg!([NAME] "repo name").value_parser(clap::value_parser!(String))),
        )
}

/// Handle subcommands to `repo`
pub fn handle(args: &ArgMatches, installation: Installation, verbose: bool) -> Result<(), Error> {
    let client = Client::for_cli(environment::NAME, installation, verbose)?;
    let system_intent_path = client.system_intent().map(|intent| intent.path().to_owned());
    let manager = client.into_repository_manager();

    let handler = match args.subcommand() {
        Some(("list", _)) => Action::List,
        Some(("update", cmd_args)) => Action::Update(cmd_args.get_one::<String>("NAME").cloned()),
        Some((command, _)) if system_intent_path.is_some() => {
            return Err(Error::SystemIntentDisallowed {
                command: command.to_owned(),
                path: system_intent_path.expect("guarded by is_some"),
            });
        }
        Some(("add", cmd_args)) => Action::Add(
            cmd_args.get_one::<String>("NAME").cloned().unwrap(),
            cmd_args.get_one::<Url>("URI").cloned().unwrap(),
            cmd_args.get_one::<String>("comment").cloned().unwrap(),
            Priority::new(*cmd_args.get_one::<u64>("priority").unwrap()),
            cmd_args.get_one::<RootIndexOptions>("root-index").cloned(),
        ),
        Some(("remove", cmd_args)) => Action::Remove(cmd_args.get_one::<String>("NAME").cloned().unwrap()),
        Some(("enable", cmd_args)) => Action::Enable(cmd_args.get_one::<String>("NAME").cloned().unwrap()),
        Some(("disable", cmd_args)) => Action::Disable(cmd_args.get_one::<String>("NAME").cloned().unwrap()),
        _ => unreachable!(),
    };

    // dispatch to runtime handler function
    match handler {
        Action::List => list(manager),
        Action::Add(name, uri, comment, priority, root_index_options) => {
            add(manager, name, uri, comment, priority, root_index_options)
        }
        Action::Remove(name) => remove(manager, name),
        Action::Update(name) => update(manager, name),
        Action::Enable(name) => enable(manager, name),
        Action::Disable(name) => disable(manager, name),
    }
}

// Actual implementation of Cast repo add
fn add(
    mut manager: repository::Manager,
    name: String,
    uri: Url,
    comment: String,
    priority: Priority,
    root_index_options: Option<RootIndexOptions>,
) -> Result<(), Error> {
    let id = repository::Id::new(&name);

    let source = if let Some(RootIndexOptions { channel, version, arch }) = root_index_options {
        repository::Source::RootIndex(repository::RootIndexSource {
            base_uri: uri,
            channel,
            version,
            arch,
        })
    } else {
        repository::Source::DirectIndex(uri)
    };

    manager.add_repository(
        id.clone(),
        Repository {
            description: comment,
            source,
            priority,
            active: true,
        },
    )?;

    runtime::block_on(manager.refresh(&id))?;

    println!("{id} added");

    Ok(())
}

/// List the repositories and pretty print them
fn list(manager: repository::Manager) -> Result<(), Error> {
    let configured_repos = manager.list();
    if configured_repos.len() == 0 {
        println!("No repositories have been configured yet");
        return Ok(());
    }

    for (id, repo) in configured_repos.sorted_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).reverse()) {
        let disabled = if !repo.active {
            " (disabled)".dim().to_string()
        } else {
            String::new()
        };

        // TODO: Print the canonical Gluon fragment for each repository. The
        // human-readable summary remains useful, but is not round-trippable.
        match &repo.source {
            repository::Source::DirectIndex(uri) => println!(" - {id} = {uri} [{}]{disabled}", repo.priority),
            repository::Source::RootIndex(repository::RootIndexSource {
                base_uri,
                channel,
                version,
                arch,
            }) => println!(
                " - {id} = (base-uri={base_uri}, channel={channel}, version={version}, arch={arch}) [{}]{disabled}",
                repo.priority
            ),
        }
    }

    Ok(())
}

/// Update specific repos or all
fn update(manager: repository::Manager, which: Option<String>) -> Result<(), Error> {
    runtime::block_on(async {
        match which {
            Some(repo) => manager.refresh(&repository::Id::new(&repo)).await,
            None => manager.refresh_all().await,
        }
    })?;

    Ok(())
}

/// Remove repo
fn remove(mut manager: repository::Manager, repo: String) -> Result<(), Error> {
    let id = repository::Id::new(&repo);

    match manager.remove(id.clone())? {
        repository::manager::Removal::NotFound => return Err(Error::RepositoryNotFound(id.to_string())),
        repository::manager::Removal::ConfigDeleted(false) => {
            return Err(Error::ManualConfigurationDeletion(id.to_string()));
        }
        repository::manager::Removal::ConfigDeleted(true) => {
            println!("{id} removed");
        }
    }

    Ok(())
}

fn enable(mut manager: repository::Manager, repo: String) -> Result<(), Error> {
    let id = repository::Id::new(&repo);

    runtime::block_on(manager.enable(&id))?;

    println!("{id} enabled");

    Ok(())
}

fn disable(mut manager: repository::Manager, repo: String) -> Result<(), Error> {
    let id = repository::Id::new(&repo);

    runtime::block_on(manager.disable(&id))?;

    println!("{id} disabled");

    Ok(())
}

#[derive(Debug, Clone)]
struct RootIndexOptions {
    channel: repository::format::Identifier,
    version: repository::format::ScopedIdentifier,
    arch: String,
}

fn parse_root_index_options(s: &str) -> Result<RootIndexOptions, String> {
    if !s.contains('=') {
        return Err("options must be key=value[,key=value]*".to_owned());
    }

    let mut key_values = s
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .collect::<BTreeMap<_, _>>();

    let channel =
        repository::format::Identifier::try_from(key_values.remove("channel").unwrap_or(repository::DEFAULT_CHANNEL))
            .map_err(|err| err.to_string())?;
    let version = key_values
        .remove("version")
        .ok_or("version is required")?
        .parse::<repository::format::ScopedIdentifier>()
        .map_err(|err| format!("invalid version identifier: {err}"))?;
    let arch = key_values.remove("arch").unwrap_or(repository::DEFAULT_ARCH).to_owned();

    if let Some(key) = key_values.into_keys().next() {
        return Err(format!("unknown key: {key}"));
    }

    Ok(RootIndexOptions { channel, version, arch })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("setup client")]
    Client(#[from] client::Error),
    #[error("repo manager")]
    RepositoryManager(#[from] repository::manager::Error),
    #[error(
        "`cast repo {command}` is not allowed while authored Gluon system intent is active; edit repositories in {path:?}"
    )]
    SystemIntentDisallowed { command: String, path: PathBuf },
    #[error("repository {0} was not found")]
    RepositoryNotFound(String),
    #[error("repository {0} must be manually deleted because it does not have its own configuration file")]
    ManualConfigurationDeletion(String),
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;

    #[test]
    fn authored_system_intent_rejects_imperative_repository_changes() {
        let temporary = crate::test_support::private_installation_tempdir();
        let intent_path = system_model::intent_path(temporary.path());
        fs::create_dir_all(intent_path.parent().unwrap()).unwrap();
        let authored = r#"// Repository intent remains administrator-owned.
let cast = import! cast.system.v1
{
    repositories = [
        cast.repository.direct "local" "file:///var/cache/cast/local.index",
    ],
    .. cast.system
}
"#;
        fs::write(&intent_path, authored).unwrap();

        let cases = [
            vec!["repo", "add", "extra", "file:///var/cache/cast/extra.index"],
            vec!["repo", "remove", "local"],
            vec!["repo", "enable", "local"],
            vec!["repo", "disable", "local"],
        ];

        for args in cases {
            let matches = command().try_get_matches_from(args).unwrap();
            let installation = Installation::open(temporary.path(), None).unwrap();
            let error = handle(&matches, installation, false).unwrap_err();
            let Error::SystemIntentDisallowed { command, path } = error else {
                panic!("expected system-intent rejection, got {error}");
            };

            assert!(matches!(command.as_str(), "add" | "remove" | "enable" | "disable"));
            assert_eq!(path, intent_path);
            assert_eq!(fs::read_to_string(&intent_path).unwrap(), authored);
        }
    }
}
