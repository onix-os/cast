// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use gluon::{
    Error, ModuleCompiler, Thread,
    base::types::ArcType,
    compiler_pipeline::SalvageResult,
    import::{DefaultImporter, Importer},
};

const FORBIDDEN_MODULE_PREFIXES: &[&str] = &[
    "std.fs",
    "std.io",
    "std.process",
    "std.env",
    "std.random",
    "std.http",
    "std.thread",
    "std.channel",
    "std.reference",
    "std.st.reference",
    "std.effect",
    "std.debug",
    "std.path",
];

#[derive(Clone, Default)]
pub(crate) struct RestrictedImporter {
    allowed_modules: Arc<BTreeSet<String>>,
}

impl RestrictedImporter {
    pub(crate) fn closed() -> Self {
        Self::default()
    }

    fn is_forbidden(module: &str) -> bool {
        FORBIDDEN_MODULE_PREFIXES
            .iter()
            .any(|prefix| module == *prefix || module.starts_with(&format!("{prefix}.")))
    }
}

#[async_trait]
impl Importer for RestrictedImporter {
    async fn import(
        &self,
        compiler: &mut ModuleCompiler<'_, '_>,
        vm: &Thread,
        module: &str,
    ) -> SalvageResult<ArcType, Error> {
        if Self::is_forbidden(module) || !self.allowed_modules.contains(module) {
            return Err(Error::from(format!("configuration import denied: {module}")).into());
        }

        DefaultImporter.import(compiler, vm, module).await
    }
}
