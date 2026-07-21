//! Reserved names owned by descriptor-retained boot-file publication.
//!
//! Canonical plans and low-level requests must never enter this namespace.
//! Reserving the whole prefix, rather than only currently generated suffixes,
//! prevents a canonical leaf from aliasing another request's deterministic
//! private stage on case-insensitive boot filesystems.

pub(crate) const RETAINED_BOOT_FILE_PRIVATE_PREFIX: &str = ".cast-payload-";

pub(crate) fn is_retained_boot_file_private_component(component: &str) -> bool {
    component
        .as_bytes()
        .get(..RETAINED_BOOT_FILE_PRIVATE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RETAINED_BOOT_FILE_PRIVATE_PREFIX.as_bytes()))
}
