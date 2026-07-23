/// Explicit resource budget for one evaluated package declaration.
///
/// Gluon evaluation already has VM and source limits, but pure functions can
/// still construct values much larger than their authored source. These
/// limits keep conversion, regex/glob compilation, dependency resolution, and
/// plan construction bounded after the value crosses into Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageValidationLimits {
    pub max_metadata_bytes: usize,
    pub max_text_bytes: usize,
    pub max_collection_items: usize,
    pub max_outputs: usize,
    pub max_profiles: usize,
    pub max_sources: usize,
    pub max_total_items: usize,
    pub max_total_text_bytes: usize,
}

impl Default for PackageValidationLimits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 4 * 1024,
            max_text_bytes: 64 * 1024,
            max_collection_items: 4 * 1024,
            max_outputs: 128,
            max_profiles: 128,
            max_sources: 256,
            max_total_items: 32 * 1024,
            max_total_text_bytes: 2 * 1024 * 1024,
        }
    }
}
