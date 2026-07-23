// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use crate::draft::File;
use crate::draft::build::{Error, State};

pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
    if file.depth() == 0 && file.file_name() == "Cargo.toml" {
        state.increment_confidence(100);
    }

    Ok(())
}
