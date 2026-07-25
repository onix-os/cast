use std::path::PathBuf;

use declarative_config::LanguageSpec;

/// One evaluated declaration after whole-fragment precedence has been
/// applied, retaining both its typed value and engine-produced identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDeclaration<T, I> {
    pub logical_name: String,
    pub path: PathBuf,
    pub language: LanguageSpec,
    pub value: T,
    pub identity: I,
}
