// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use regex::Regex;
use std::path::Path;

use crate::draft::File;
use crate::draft::build::{Error, State};
use forge::{Dependency, dependency};

pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
    // Depth too great
    if file.depth() > 0 {
        return Ok(());
    }

    if file.file_name() == "CMakeLists.txt" {
        state.increment_confidence(20);
        scan_cmake(state, &file.path)?;
    }

    Ok(())
}

fn scan_cmake(state: &mut State<'_>, path: &Path) -> Result<(), Error> {
    let contents = state.read_analysis_text(path)?;

    let regex_cmake = Regex::new(r"find_package\(([^ )]+)")?;

    for captures in regex_cmake.captures_iter(&contents) {
        if let Some(capture) = captures.get(1) {
            let name = capture.as_str().to_owned();

            state.add_dependency(Dependency {
                kind: dependency::Kind::CMake,
                name,
            })?;
        }
    }
    Ok(())
}
