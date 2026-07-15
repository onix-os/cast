use std::collections::{BTreeMap, BTreeSet};

use crate::package::PackageSpec;

use super::PackageConversionError;

pub(super) fn validate_output_cycles(
    package: &PackageSpec,
    outputs: &BTreeMap<&str, usize>,
) -> Result<(), PackageConversionError> {
    let mut edges = BTreeMap::<&str, Vec<(&str, String)>>::new();
    for (index, output) in package.outputs.iter().enumerate() {
        let dependencies = output
            .runtime_inputs
            .iter()
            .enumerate()
            .filter_map(|(dependency_index, dependency)| {
                let (dependency_package, target) = dependency.package_and_output()?;
                (dependency_package == package.meta.pname && outputs.contains_key(target))
                    .then(|| (target, format!("outputs[{index}].runtime_inputs[{dependency_index}]")))
            })
            .collect();
        edges.insert(&output.name, dependencies);
    }

    for output in &package.outputs {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        if let Some((field, cycle)) = find_cycle(&output.name, &edges, &mut visiting, &mut visited, &mut path) {
            return Err(PackageConversionError::OutputDependencyCycle { field, cycle });
        }
    }
    Ok(())
}

fn find_cycle<'a>(
    node: &'a str,
    edges: &BTreeMap<&'a str, Vec<(&'a str, String)>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<(String, String)> {
    if visited.contains(node) {
        return None;
    }
    if !visiting.insert(node) {
        let start = path.iter().position(|entry| *entry == node).unwrap_or(0);
        let mut cycle = path[start..].to_vec();
        cycle.push(node);
        return Some(("outputs".to_owned(), cycle.join(" -> ")));
    }

    path.push(node);
    for (target, field) in edges.get(node).into_iter().flatten() {
        if visiting.contains(target) {
            let start = path.iter().position(|entry| entry == target).unwrap_or(0);
            let mut cycle = path[start..].to_vec();
            cycle.push(target);
            return Some((field.clone(), cycle.join(" -> ")));
        }
        if let Some(cycle) = find_cycle(target, edges, visiting, visited, path) {
            return Some(cycle);
        }
    }
    path.pop();
    visiting.remove(node);
    visited.insert(node);
    None
}
