// SPDX-FileCopyrightText: 2025 AerynOS Developers

pub(crate) const VERSION: &str = env!("BUILDINFO_VERSION");

pub(crate) const BUILD_TIME: &str = env!("BUILDINFO_BUILD_TIME");

pub(crate) const SEMANTIC_FINGERPRINT: &str = env!("BUILDINFO_SEMANTIC_FINGERPRINT");

#[cfg(BUILDINFO_IS_GIT_BUILD)]
pub(crate) const GIT_FULL_HASH: &str = env!("BUILDINFO_GIT_FULL_HASH");

#[cfg(BUILDINFO_IS_GIT_BUILD)]
pub(crate) const GIT_SHORT_HASH: &str = env!("BUILDINFO_GIT_SHORT_HASH");

#[cfg(BUILDINFO_IS_GIT_BUILD)]
pub(crate) const GIT_SUMMARY: &str = env!("BUILDINFO_GIT_SUMMARY");
