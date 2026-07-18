// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::path::Path;

use forge::{Dependency, dependency};
use regex::Regex;

use crate::draft::File;
use crate::draft::build::{Error, State};

pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
    match file.file_name() {
        "meson.build" if file.depth() == 0 => {
            state.increment_confidence(100);
            scan_meson(state, &file.path)?;
        }
        "meson_options.txt" if file.depth() == 0 => {
            state.increment_confidence(100);
        }
        _ => {}
    }

    Ok(())
}

fn scan_meson(state: &mut State<'_>, path: &Path) -> Result<(), Error> {
    let regex_dependency = Regex::new(r"dependency\s?\(\s?'\s?([A-Za-z0-9+-_]+)")?;
    let regex_program = Regex::new(r"find_program\s?\(\s?'\s?([A-Za-z0-9+-_]+)")?;

    let contents = state.read_analysis_text(path)?;

    // Check all meson dependency() calls
    for captures in regex_dependency.captures_iter(&contents) {
        if let Some(capture) = captures.get(1) {
            let name = capture.as_str().to_owned();

            state.add_dependency(Dependency {
                kind: dependency::Kind::PkgConfig,
                name,
            })?;
        }
    }

    // Check all meson find_program() calls
    for captures in regex_program.captures_iter(&contents) {
        if let Some(capture) = captures.get(1) {
            let name = capture.as_str().to_owned();

            // Relative programs are a no go
            if name.contains('/') {
                continue;
            }

            state.add_dependency(Dependency {
                kind: dependency::Kind::Binary,
                name,
            })?;
        }
    }

    Ok(())
}
