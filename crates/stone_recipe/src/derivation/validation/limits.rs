/// Resource limits for process-facing data in one frozen derivation.
///
/// These limits are enforced again at the freeze boundary even though an
/// evaluated package has its own limits. A
/// [`DerivationPlan`](crate::derivation::DerivationPlan) can also be constructed
/// programmatically, and planning expands policy commands and environments
/// which are not all present in the authored package value. Keeping the final
/// argv, environment, path, and step budgets here prevents a validly encoded
/// plan from failing late in `execve(2)` or from making the executor traverse
/// unbounded process data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivationValidationLimits {
    pub max_jobs: usize,
    pub max_phases_per_job: usize,
    pub max_steps_per_section: usize,
    pub max_total_steps: usize,
    pub max_arguments_per_step: usize,
    pub max_declared_programs_per_step: usize,
    pub max_environment_entries: usize,
    pub max_environment_name_bytes: usize,
    pub max_process_string_bytes: usize,
    pub max_path_bytes: usize,
    pub max_execve_bytes: usize,
    pub max_total_process_items: usize,
    pub max_total_process_text_bytes: usize,
}

impl Default for DerivationValidationLimits {
    fn default() -> Self {
        Self {
            max_jobs: 64,
            // A job can contain each of the six supported phases at most once.
            max_phases_per_job: 6,
            max_steps_per_section: 4 * 1024,
            max_total_steps: 16 * 1024,
            max_arguments_per_step: 1024,
            max_declared_programs_per_step: 256,
            max_environment_entries: 1024,
            max_environment_name_bytes: 255,
            // Linux accepts a larger single argv/environment string, but the
            // smaller frozen ABI limit leaves room for the complete vector.
            max_process_string_bytes: 64 * 1024,
            // Leave one byte for the terminating NUL beneath Linux PATH_MAX.
            max_path_bytes: 4095,
            // Includes string terminators and argv/envp pointer storage. Linux
            // guarantees at least 32 pages for argv+envp; 96 KiB remains below
            // that floor on the supported 4 KiB-or-larger page sizes.
            max_execve_bytes: 96 * 1024,
            max_total_process_items: 128 * 1024,
            max_total_process_text_bytes: 8 * 1024 * 1024,
        }
    }
}
