// SPDX-FileCopyrightText: 2024 AerynOS Developers

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum Architecture {
    X86_64,
    X86,
    Aarch64,
    Riscv64,
}
