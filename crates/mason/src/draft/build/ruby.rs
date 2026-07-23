// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

// filename.gem
pub mod gemfile {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        match file.file_name() {
            "checksums.yaml.gz" if file.depth() == 0 => state.increment_confidence(50),
            "data.tar.gz" if file.depth() == 0 => state.increment_confidence(80),
            "metadata.gz" if file.depth() == 0 => state.increment_confidence(100),
            _ => {}
        }

        Ok(())
    }
}

// A normal tarball
pub mod tarball {
    use crate::draft::File;
    use crate::draft::build::{Error, State};

    pub fn process(state: &mut State<'_>, file: &File) -> Result<(), Error> {
        match file.file_name() {
            _ if file.depth() == 0 && file.file_name().ends_with(".gemspec") => {
                state.increment_confidence(100);
            }
            _ => {}
        }

        Ok(())
    }
}
