use super::{ActiveReblitRootFilesystemIntentError, RootFilesystemIntentBudget, RootFilesystemIntentValue};

const MAX_ROOT_DIAGNOSTIC_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RootNormalizationCheckpoint {
    Materialized,
}

pub(super) fn materialize_root_argument(
    value: String,
    budget: &mut RootFilesystemIntentBudget,
) -> Result<RootFilesystemIntentValue, ActiveReblitRootFilesystemIntentError> {
    materialize_root_argument_with_checkpoint(value, budget, |_| {})
}

pub(super) fn materialize_root_argument_with_checkpoint<F>(
    value: String,
    budget: &mut RootFilesystemIntentBudget,
    mut checkpoint: F,
) -> Result<RootFilesystemIntentValue, ActiveReblitRootFilesystemIntentError>
where
    F: FnMut(RootNormalizationCheckpoint),
{
    let actual = value.len();
    if actual > budget.policy.max_root_bytes {
        return Err(ActiveReblitRootFilesystemIntentError::RootBytesLimit {
            limit: budget.policy.max_root_bytes,
            actual,
        });
    }
    if value.is_empty() {
        return Err(invalid_root(&value, "root locator must not be empty"));
    }
    if value.starts_with("root=") {
        return Err(invalid_root(
            &value,
            "declare only the locator; Rust owns the single root= prefix",
        ));
    }

    budget.reserve_work(value.len(), "root locator byte validation")?;
    if value
        .bytes()
        .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'\'' | b'"' | b'\\'))
    {
        return Err(invalid_root(
            &value,
            "root locator must be whitespace-free printable ASCII without quotes or backslashes",
        ));
    }

    let output_bytes =
        value
            .len()
            .checked_add(b"root=".len())
            .ok_or(ActiveReblitRootFilesystemIntentError::RootBytesLimit {
                limit: budget.policy.max_root_bytes + b"root=".len(),
                actual: usize::MAX,
            })?;
    let mut argument = String::new();
    argument
        .try_reserve_exact(output_bytes)
        .map_err(|source| ActiveReblitRootFilesystemIntentError::Allocation {
            resource: "root-filesystem kernel argument",
            source,
        })?;
    argument.push_str("root=");
    argument.push_str(&value);

    let root = value.into_boxed_str();
    let kernel_argument = argument.into_boxed_str();
    checkpoint(RootNormalizationCheckpoint::Materialized);
    budget.require_deadline()?;
    Ok(RootFilesystemIntentValue { root, kernel_argument })
}

fn invalid_root(value: &str, reason: &'static str) -> ActiveReblitRootFilesystemIntentError {
    let mut preview_bytes = value.len().min(MAX_ROOT_DIAGNOSTIC_BYTES);
    while !value.is_char_boundary(preview_bytes) {
        preview_bytes -= 1;
    }
    ActiveReblitRootFilesystemIntentError::InvalidRoot {
        value_preview: value[..preview_bytes].to_owned().into_boxed_str(),
        actual_bytes: value.len(),
        reason,
    }
}
