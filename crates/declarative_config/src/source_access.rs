use std::path::Path;

use crate::{Diagnostic, Source, SourceRoot};

/// Temporary adapter bridge until module-graph resolution moves into this
/// crate. Import reads deliberately retain their distinct error and limit
/// classification instead of using the public root-source loader.
pub fn load_import(root: &SourceRoot, relative: &Path, max_bytes: usize) -> Result<Source, Diagnostic> {
    root.load_import(relative, max_bytes)
}
