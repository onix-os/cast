// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub mod extutils_makefile {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        if file.depth() == 0 && file.file_name() == "Makefile.PL" {
            state.increment_confidence(100);
        }

        Ok(())
    }
}

pub mod module_build {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        if file.depth() == 0 && file.file_name() == "Build.PL" {
            // We prefer Makefile.PL if available
            state.increment_confidence(95);
        }

        Ok(())
    }
}
