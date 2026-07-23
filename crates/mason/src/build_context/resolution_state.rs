#[derive(Default)]
struct ResolutionBudget {
    items: Cell<usize>,
    text_nodes: Cell<usize>,
    output_bytes: Cell<usize>,
    steps: Cell<usize>,
}

impl ResolutionBudget {
    fn ensure_items(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let count = self.items.get().saturating_add(additional);
        if count > limit {
            Err(ContextError::ResolvedItemLimit { count, limit })
        } else {
            Ok(())
        }
    }

    fn claim_items(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        self.ensure_items(additional, limit)?;
        self.items.set(self.items.get().saturating_add(additional));
        Ok(())
    }

    fn ensure_text_nodes(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let nodes = self.text_nodes.get().saturating_add(additional);
        if nodes > limit {
            Err(ContextError::TotalTextNodeLimit { nodes, limit })
        } else {
            Ok(())
        }
    }

    fn claim_text_node(&self, limit: usize) -> Result<(), ContextError> {
        self.ensure_text_nodes(1, limit)?;
        self.text_nodes.set(self.text_nodes.get().saturating_add(1));
        Ok(())
    }

    fn claim_output_bytes(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let bytes = self.output_bytes.get().saturating_add(additional);
        if bytes > limit {
            return Err(ContextError::TotalResolvedTextBytesLimit { bytes, limit });
        }
        self.output_bytes.set(bytes);
        Ok(())
    }

    fn ensure_steps(&self, additional: usize, limit: usize) -> Result<(), ContextError> {
        let steps = self.steps.get().saturating_add(additional);
        if steps > limit {
            Err(ContextError::ResolverStepLimit { steps, limit })
        } else {
            Ok(())
        }
    }

    fn claim_step(&self, limit: usize) -> Result<(), ContextError> {
        self.ensure_steps(1, limit)?;
        self.steps.set(self.steps.get().saturating_add(1));
        Ok(())
    }
}

struct TextResolver<'a> {
    policy: &'a BuildPolicySpec,
    target: &'a TargetPolicySpec,
    inputs: &'a TypedContextInputs,
    overlay: TextContextOverlay,
    limits: BuildPolicyValidationLimits,
    budget: ResolutionBudget,
}

enum ResolveAction<'a> {
    Text {
        value: &'a TextSpec,
        depth: usize,
        output: usize,
    },
    Context {
        value: ContextValue,
        depth: usize,
        output: usize,
    },
    LeaveContext(ContextValue),
    Append {
        value: &'a str,
        output: usize,
    },
    AppendOwned {
        value: String,
        output: usize,
    },
    Flags {
        selected: &'a [TextSpec],
        mold: &'a [TextSpec],
        index: usize,
        output: usize,
        emitted: bool,
        depth: usize,
    },
    FinishFlag {
        selected: &'a [TextSpec],
        mold: &'a [TextSpec],
        next_index: usize,
        output: usize,
        child: usize,
        emitted: bool,
        depth: usize,
    },
}

enum ContextExpansion<'a> {
    Text(&'a TextSpec),
    Flags(&'a [TextSpec], &'a [TextSpec]),
    Borrowed(&'a str),
    Owned(String),
}
