// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Embeddable command fragments for Cast's package-management surface.
//!
//! This module deliberately does not own argument collection, top-level
//! parsing, logging, documentation generation, version output, or process
//! termination.  The external `cast` binary composes these fragments with the
//! Mason fragments and supplies the execution context explicitly.

use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use thiserror::Error;
use tui::Styled;

use crate::{Installation, installation};

mod boot;
mod cache;
mod extract;
mod fetch;
mod index;
mod info;
mod inspect;
mod install;
mod list;
mod remove;
mod repo;
mod search;
mod search_file;
mod state;
mod sync;

/// Canonical top-level command names owned exclusively by Forge.
pub const COMMAND_NAMES: &[&str] = &[
    "boot",
    "extract",
    "fetch",
    "index",
    "info",
    "inspect",
    "install",
    "list",
    "remove",
    "repo",
    "search",
    "search-file",
    "state",
    "sync",
];

/// Runtime options supplied by the external Cast command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    root: PathBuf,
    cache_dir: Option<PathBuf>,
    verbose: bool,
    assume_yes: bool,
}

impl Context {
    pub fn new(root: impl Into<PathBuf>, cache_dir: Option<PathBuf>, verbose: bool, assume_yes: bool) -> Self {
        Self {
            root: root.into(),
            cache_dir,
            verbose,
            assume_yes,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }

    pub const fn verbose(&self) -> bool {
        self.verbose
    }

    pub const fn assume_yes(&self) -> bool {
        self.assume_yes
    }
}

/// Return Forge's non-colliding top-level command fragments.
pub fn command_fragments() -> Vec<Command> {
    vec![
        boot::command(),
        extract::command(),
        fetch::command(),
        index::command(),
        info::command(),
        inspect::command(),
        install::command(),
        list::command(),
        remove::command(),
        repo::command(),
        search::command(),
        search_file::command(),
        state::command(),
        sync::command(),
    ]
}

/// Return Forge's `cache prune` subcommand for Cast's shared `cache` tree.
pub fn cache_prune_fragment() -> Command {
    cache::prune_command()
}

/// Dispatch one canonical Forge-owned top-level command.
///
/// Returns `Ok(false)` without touching the filesystem when `command` belongs
/// to another command domain.
pub fn try_dispatch(command: &str, args: &ArgMatches, context: &Context) -> Result<bool, Error> {
    if !COMMAND_NAMES.contains(&command) {
        return Ok(false);
    }

    // These commands operate only on their explicit file arguments. Opening
    // the configured system installation first would make an unrelated root
    // policy, lock, or declarative-system warning affect a rootless operation.
    match command {
        "extract" => {
            extract::handle(args).map_err(Error::Extract)?;
            return Ok(true);
        }
        "index" => {
            index::handle(args).map_err(Error::Index)?;
            return Ok(true);
        }
        "inspect" => {
            inspect::handle(args).map_err(Error::Inspect)?;
            return Ok(true);
        }
        _ => {}
    }

    let installation = open_installation(context)?;

    match command {
        "boot" => boot::handle(args, installation).map_err(Error::Boot)?,
        "fetch" => fetch::handle(args, installation, context.verbose).map_err(Error::Fetch)?,
        "info" => info::handle(args, installation).map_err(Error::Info)?,
        "install" => install::handle(args, installation, context.assume_yes).map_err(Error::Install)?,
        "list" => list::handle(args, installation).map_err(Error::List)?,
        "remove" => remove::handle(args, installation, context.assume_yes).map_err(Error::Remove)?,
        "repo" => repo::handle(args, installation).map_err(Error::Repo)?,
        "search" => search::handle(args, installation).map_err(Error::Search)?,
        "search-file" => search_file::handle(args, installation).map_err(Error::SearchFile)?,
        "state" => state::handle(args, installation, context.assume_yes, context.verbose).map_err(Error::State)?,
        "sync" => sync::handle(args, installation, context.assume_yes).map_err(Error::Sync)?,
        "extract" | "index" | "inspect" => unreachable!("rootless commands return before opening an installation"),
        _ => unreachable!("Cast command names and package dispatch must stay aligned"),
    }

    Ok(true)
}

/// Dispatch Forge's part of Cast's shared `cache` command.
///
/// `command` is the selected cache subcommand name and `args` are its matches.
pub fn try_dispatch_cache(command: &str, args: &ArgMatches, context: &Context) -> Result<bool, Error> {
    if command != "prune" {
        return Ok(false);
    }

    let installation = open_installation(context)?;
    cache::handle_prune(args, installation).map_err(|error| Error::Cache(Box::new(error)))?;
    Ok(true)
}

fn open_installation(context: &Context) -> Result<Installation, Error> {
    let installation = Installation::open(context.root.clone(), context.cache_dir.clone())?;

    if let Some(system_model) = installation.system_model.as_ref() {
        if !system_model.disable_warning {
            print_system_model_warning(&installation, false);
        } else if context.verbose {
            print_system_model_warning(&installation, true);
        }
    }

    Ok(installation)
}

fn print_system_model_warning(installation: &Installation, first_line_only: bool) {
    let path = installation.system_intent_path();

    eprintln!(
        "{}: authored Gluon system intent at {path:?} is active.",
        "INFO".green()
    );

    if !first_line_only {
        eprintln!(
            "Hence:
- This system intent is the source of truth and defines all
  repositories & installed packages.
- Any changes made via `cast` commands will be temporary
  until the authored intent is updated.
- The system state can be reverted to match the declared intent
  by doing a `cast sync`.
- Each state stores a generated `/usr/lib/system-model.glu` snapshot;
  it is not the authored source and should not be edited.
- To disable declarative system intent, remove or rename {path:?}.",
        );
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("boot")]
    Boot(#[source] boot::Error),

    #[error("cache")]
    Cache(#[source] Box<cache::Error>),

    #[error("index")]
    Index(#[source] index::Error),

    #[error("info")]
    Info(#[source] info::Error),

    #[error("install")]
    Install(#[source] install::Error),

    #[error("list")]
    List(#[source] list::Error),

    #[error("inspect")]
    Inspect(#[source] inspect::Error),

    #[error("extract")]
    Extract(#[source] extract::Error),

    #[error("fetch")]
    Fetch(#[source] fetch::Error),

    #[error("remove")]
    Remove(#[source] remove::Error),

    #[error("repo")]
    Repo(#[source] repo::Error),

    #[error("search")]
    Search(#[source] search::Error),

    #[error("search-file")]
    SearchFile(#[source] search_file::Error),

    #[error("state")]
    State(#[source] state::Error),

    #[error("sync")]
    Sync(#[source] sync::Error),

    #[error("installation")]
    Installation(#[from] installation::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_are_noncolliding_and_exclude_shared_commands() {
        let names = command_fragments()
            .into_iter()
            .map(|command| command.get_name().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(names, COMMAND_NAMES);
        assert!(!names.iter().any(|name| name == "cache" || name == "version"));

        let mut command = Command::new("cast");
        for fragment in command_fragments() {
            command = command.subcommand(fragment);
        }
        command.debug_assert();
    }

    #[test]
    fn cache_fragment_is_explicit() {
        assert_eq!(cache_prune_fragment().get_name(), "prune");
    }

    #[test]
    fn unknown_dispatch_does_not_open_the_context_root() {
        let context = Context::new("/definitely/not/a/cast/root", None, false, false);
        let matches = Command::new("other").get_matches_from(["other"]);
        assert!(!try_dispatch("other", &matches, &context).unwrap());
        assert!(!try_dispatch_cache("other", &matches, &context).unwrap());
    }

    #[test]
    fn rootless_dispatch_does_not_open_the_context_root() {
        let stone =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");
        let matches = inspect::command()
            .try_get_matches_from([
                "inspect".into(),
                "--check".into(),
                "--quiet".into(),
                stone.into_os_string(),
            ])
            .unwrap();
        let context = Context::new("/definitely/not/a/cast/root", None, false, false);

        assert!(try_dispatch("inspect", &matches, &context).unwrap());
    }
}
