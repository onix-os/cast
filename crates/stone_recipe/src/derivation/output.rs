use super::{CanonicalEncoder, LockedOutputRef, RelationPlan, encode_optional_string};

/// One declared package output after template and package composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub package_name: String,
    pub include_in_manifest: bool,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_exclude: Vec<String>,
    pub runtime_inputs: Vec<OutputRelation>,
    pub conflicts: Vec<RelationPlan>,
}

impl OutputPlan {
    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.string(&self.package_name);
        encoder.bool(self.include_in_manifest);
        encode_optional_string(encoder, self.summary.as_deref());
        encode_optional_string(encoder, self.description.as_deref());
        let mut provides_exclude = self.provides_exclude.clone();
        provides_exclude.sort();
        encoder.strings(&provides_exclude);
        let mut runtime_exclude = self.runtime_exclude.clone();
        runtime_exclude.sort();
        encoder.strings(&runtime_exclude);

        let mut runtime_inputs = self.runtime_inputs.iter().collect::<Vec<_>>();
        runtime_inputs.sort();
        encoder.sequence(&runtime_inputs, |encoder, dependency| dependency.encode(encoder));
        let mut conflicts = self.conflicts.clone();
        conflicts.sort();
        encoder.sequence(&conflicts, |encoder, relation| relation.encode(encoder));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputRelation {
    Locked {
        relation: RelationPlan,
        reference: LockedOutputRef,
    },
    Planned {
        output: String,
    },
}

impl OutputRelation {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Locked { relation, reference } => {
                encoder.variant(0);
                relation.encode(encoder);
                reference.encode(encoder);
            }
            Self::Planned { output } => {
                encoder.variant(1);
                encoder.string(output);
            }
        }
    }
}
