fn append_checked(
    outputs: &mut [Option<String>],
    output: usize,
    value: &str,
    limit: usize,
    budget: &ResolutionBudget,
    total_limit: usize,
) -> Result<(), ContextError> {
    let buffer = outputs[output].as_mut().expect("output buffer is live");
    let bytes = buffer.len().saturating_add(value.len());
    if bytes > limit {
        return Err(ContextError::ResolvedTextBytesLimit { bytes, limit });
    }
    budget.claim_output_bytes(value.len(), total_limit)?;
    buffer
        .try_reserve(value.len())
        .map_err(|_| ContextError::TextCapacity { requested: bytes })?;
    buffer.push_str(value);
    Ok(())
}

fn append_joined_checked(
    outputs: &mut [Option<String>],
    output: usize,
    value: &str,
    separator: bool,
    limit: usize,
    budget: &ResolutionBudget,
    total_limit: usize,
) -> Result<(), ContextError> {
    let buffer = outputs[output].as_mut().expect("output buffer is live");
    let added = value.len().saturating_add(usize::from(separator));
    let bytes = buffer.len().saturating_add(added);
    if bytes > limit {
        return Err(ContextError::ResolvedTextBytesLimit { bytes, limit });
    }
    budget.claim_output_bytes(added, total_limit)?;
    buffer
        .try_reserve(added)
        .map_err(|_| ContextError::TextCapacity { requested: bytes })?;
    if separator {
        buffer.push(' ');
    }
    buffer.push_str(value);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error(transparent)]
    PolicyValidation(#[from] BuildPolicyConversionError),
    #[error("selected target is not the exact member of the validated policy")]
    TargetNotInPolicy,
    #[error("policy text requires missing finite context value {value:?}")]
    MissingContext { value: ContextValue },
    #[error("policy text contains a recursive context reference: {chain:?}")]
    RecursiveContext { chain: Vec<ContextValue> },
    #[error("resolved policy text has at least {nodes} nodes, limit is {limit}")]
    TextNodeLimit { nodes: usize, limit: usize },
    #[error("resolved policy text depth is {depth}, limit is {limit}")]
    TextDepthLimit { depth: usize, limit: usize },
    #[error("resolved policy text literal has {bytes} bytes, limit is {limit}")]
    TextLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("resolved policy text literals contain {bytes} bytes in total, limit is {limit}")]
    TextTotalLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("resolved policy text has {bytes} output bytes, limit is {limit}")]
    ResolvedTextBytesLimit { bytes: usize, limit: usize },
    #[error("resolved operation contains {count} items, limit is {limit}")]
    ResolvedItemLimit { count: usize, limit: usize },
    #[error("resolved operation contains {nodes} policy-text nodes, limit is {limit}")]
    TotalTextNodeLimit { nodes: usize, limit: usize },
    #[error("resolved operation appended {bytes} output bytes, limit is {limit}")]
    TotalResolvedTextBytesLimit { bytes: usize, limit: usize },
    #[error("resolved compiler flags contain {count} entries, limit is {limit}")]
    FlagCollectionLimit { count: usize, limit: usize },
    #[error("policy text resolver used {steps} steps, limit is {limit}")]
    ResolverStepLimit { steps: usize, limit: usize },
    #[error("unable to reserve bounded policy-text capacity for {requested} items or bytes")]
    TextCapacity { requested: usize },
    #[error("policy builders.{builder}.setup has no final builder-directory argument")]
    MissingBuilderDirectoryArgument { builder: &'static str },
}
