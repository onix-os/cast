// SPDX-FileCopyrightText: 2026 AerynOS Developers

use std::error::Error as _;

use tracing::error;
use tui::Styled;

fn main() {
    if let Err(error) = cast::run_from(std::env::args_os()) {
        match error {
            cast::Error::Clap(error) => error.exit(),
            error => {
                if !handle_manual_error(&error) {
                    report_error(&error);
                }
                std::process::exit(1);
            }
        }
    }
}

fn handle_manual_error(error: &cast::Error) -> bool {
    let Some(forge::repository::manager::Error::OutdatedRepos(source, repositories)) =
        find_source::<forge::repository::manager::Error>(error)
    else {
        return false;
    };

    forge::repository::handle_outdated_index_uris(source, repositories.clone());
    true
}

fn find_source<E: std::error::Error + 'static>(error: &dyn std::error::Error) -> Option<&E> {
    let source = error.source()?;
    source.downcast_ref::<E>().or_else(|| find_source(source))
}

fn report_error(error: &cast::Error) {
    let sources = sources(error);
    let message = sources.join(": ");
    error!(message, "Cast command failed");
    eprintln!("{}: {message}", "Error".red());
}

fn sources(error: &cast::Error) -> Vec<String> {
    let mut sources = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        sources.push(error.to_string());
        source = error.source();
    }
    sources
}
