// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub mod pep517 {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        match file.file_name() {
            "pyproject.toml" | "setup.cfg" if file.depth() == 0 => state.increment_confidence(100),
            _ => {}
        }

        Ok(())
    }
}

pub mod setup_tools {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        if file.depth() == 0 && file.file_name() == "setup.py" {
            state.increment_confidence(100);
        }

        Ok(())
    }
}
