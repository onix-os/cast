use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuildLockValidationError {
    #[error("schema_version: unsupported schema {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("{field}: `{value}` is outside the valid {expected} range")]
    IntegerOutOfRange {
        field: String,
        value: i64,
        expected: &'static str,
    },
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("repositories[{duplicate_index}].id: duplicate `{id}`, first used by repositories[{first_index}]")]
    DuplicateRepository {
        id: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("packages[{duplicate_index}].package_id: duplicate `{id}`, first used by packages[{first_index}]")]
    DuplicatePackage {
        id: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("requests[{duplicate_index}].request: duplicate `{request}`, first used by requests[{first_index}]")]
    DuplicateRequest {
        request: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("request `{request}` has no typed input origin")]
    MissingInputOrigins { request: String },
    #[error("request `{request}` repeats the same input origin at indexes {first_index} and {duplicate_index}")]
    DuplicateInputOrigin {
        request: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("package `{package}` references unknown repository `{repository}`")]
    UnknownRepository { package: String, repository: String },
    #[error("repositories[{index}]: repository `{id}` is unused by the locked package closure")]
    UnusedRepository { index: usize, id: String },
    #[error("package `{package}` has no locked outputs")]
    MissingOutputs { package: String },
    #[error("package `{package}` output `{output}` is duplicated at indexes {first_index} and {duplicate_index}")]
    DuplicateOutput {
        package: String,
        output: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error(
        "package `{package}` dependency `{dependency_package}:{output}` is duplicated at indexes {first_index} and {duplicate_index}"
    )]
    DuplicateDependency {
        package: String,
        dependency_package: String,
        output: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("{field}: unknown locked package `{package}`")]
    UnknownPackage { field: String, package: String },
    #[error("{field}: package `{package}` has no locked output `{output}`")]
    UnknownOutput {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: package dependency cycle: {}", cycle.join(" -> "))]
    DependencyCycle { field: String, cycle: Vec<String> },
    #[error("packages[{index}]: locked package `{package}` is unreachable from every request")]
    UnreachablePackage { index: usize, package: String },
}
