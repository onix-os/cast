use super::CanonicalEncoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRuleKind {
    Any,
    Executable,
    Symlink,
    Special,
}

/// One collector rule in exact matching precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRulePlan {
    pub output: String,
    pub kind: PathRuleKind,
    pub pattern: String,
}

impl CollectionRulePlan {
    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.output);
        encoder.variant(match self.kind {
            PathRuleKind::Any => 0,
            PathRuleKind::Executable => 1,
            PathRuleKind::Symlink => 2,
            PathRuleKind::Special => 3,
        });
        encoder.string(&self.pattern);
    }
}
