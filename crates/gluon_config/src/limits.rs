// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_explicit_input_bytes: usize,
    pub max_imported_file_bytes: usize,
    pub max_imports: usize,
    pub max_import_graph_bytes: usize,
    pub memory_bytes: usize,
    pub max_stack_size: u32,
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1024 * 1024,
            max_explicit_input_bytes: 1024 * 1024,
            max_imported_file_bytes: 256 * 1024,
            max_imports: 64,
            max_import_graph_bytes: 2 * 1024 * 1024,
            memory_bytes: 32 * 1024 * 1024,
            max_stack_size: 64 * 1024,
            timeout: Duration::from_secs(2),
        }
    }
}
