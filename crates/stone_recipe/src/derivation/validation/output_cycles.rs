use std::collections::{BTreeMap, BTreeSet};

use super::{
    super::{OutputPlan, OutputRelation},
    DerivationValidationError,
};

pub(super) fn validate_planned_output_cycles(outputs: &[OutputPlan]) -> Result<(), DerivationValidationError> {
    let edges = outputs
        .iter()
        .enumerate()
        .map(|(output_index, output)| {
            let dependencies = output
                .runtime_inputs
                .iter()
                .enumerate()
                .filter_map(|(dependency_index, dependency)| match dependency {
                    OutputRelation::Planned { output } => Some((
                        output.as_str(),
                        format!("outputs[{output_index}].runtime_inputs[{dependency_index}]"),
                    )),
                    OutputRelation::Locked { .. } => None,
                })
                .collect();
            (output.name.as_str(), dependencies)
        })
        .collect::<BTreeMap<_, Vec<_>>>();

    let mut visited = BTreeSet::new();
    for output in outputs {
        let mut visiting = BTreeSet::new();
        let mut path = Vec::new();
        visit_planned_output(&output.name, &edges, &mut visiting, &mut visited, &mut path)?;
    }
    Ok(())
}

fn visit_planned_output<'a>(
    output: &'a str,
    edges: &BTreeMap<&'a str, Vec<(&'a str, String)>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Result<(), DerivationValidationError> {
    if visited.contains(output) {
        return Ok(());
    }

    visiting.insert(output);
    path.push(output);
    for (dependency, field) in edges.get(output).into_iter().flatten() {
        if visiting.contains(dependency) {
            let start = path.iter().position(|entry| entry == dependency).unwrap_or(0);
            let mut cycle = path[start..]
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect::<Vec<_>>();
            cycle.push((*dependency).to_owned());
            return Err(DerivationValidationError::PlannedOutputCycle {
                field: field.clone(),
                cycle,
            });
        }
        visit_planned_output(dependency, edges, visiting, visited, path)?;
    }
    path.pop();
    visiting.remove(output);
    visited.insert(output);
    Ok(())
}
