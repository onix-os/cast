/// Finite resource ceilings applied while accepting repository build policy.
///
/// The defaults are intentionally generous for a repository policy, while
/// still preventing a decoded value from driving unbounded allocation or
/// recursive traversal. Callers which accept policy from a less trusted
/// boundary can select tighter limits and pass the same value to Mason's
/// resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildPolicyValidationLimits {
    pub max_targets: usize,
    pub max_retired_targets: usize,
    pub max_environment_bindings: usize,
    pub max_tuning_flags: usize,
    pub max_tuning_groups: usize,
    pub max_tuning_choices: usize,
    pub max_tuning_default_groups: usize,
    pub max_tuning_option_flags: usize,
    pub max_build_root_tools: usize,
    pub max_compiler_flags: usize,
    pub max_builder_arguments: usize,
    pub max_analyzers: usize,
    pub max_pgo_arguments: usize,
    pub max_pgo_inputs: usize,
    pub max_string_bytes: usize,
    pub max_total_collection_items: usize,
    pub max_total_string_bytes: usize,
    pub max_text_nodes: usize,
    pub max_text_depth: usize,
    pub max_text_literal_bytes: usize,
    pub max_text_total_literal_bytes: usize,
    pub max_total_text_nodes: usize,
    pub max_total_text_literal_bytes: usize,
    pub max_resolved_text_bytes: usize,
    pub max_resolved_items: usize,
    pub max_total_resolved_text_nodes: usize,
    pub max_total_resolved_text_bytes: usize,
    pub max_resolver_steps: usize,
}

impl Default for BuildPolicyValidationLimits {
    fn default() -> Self {
        Self {
            max_targets: 64,
            max_retired_targets: 256,
            max_environment_bindings: 1_024,
            max_tuning_flags: 1_024,
            max_tuning_groups: 1_024,
            max_tuning_choices: 1_024,
            max_tuning_default_groups: 1_024,
            max_tuning_option_flags: 4_096,
            max_build_root_tools: 4_096,
            max_compiler_flags: 8_192,
            max_builder_arguments: 4_096,
            max_analyzers: 64,
            max_pgo_arguments: 4_096,
            max_pgo_inputs: 4_096,
            max_string_bytes: 256 * 1024,
            max_total_collection_items: 131_072,
            max_total_string_bytes: 64 * 1024 * 1024,
            max_text_nodes: 65_536,
            max_text_depth: 512,
            max_text_literal_bytes: 256 * 1024,
            max_text_total_literal_bytes: 8 * 1024 * 1024,
            max_total_text_nodes: 1_000_000,
            max_total_text_literal_bytes: 64 * 1024 * 1024,
            max_resolved_text_bytes: 8 * 1024 * 1024,
            max_resolved_items: 131_072,
            max_total_resolved_text_nodes: 1_000_000,
            max_total_resolved_text_bytes: 64 * 1024 * 1024,
            max_resolver_steps: 2_000_000,
        }
    }
}
