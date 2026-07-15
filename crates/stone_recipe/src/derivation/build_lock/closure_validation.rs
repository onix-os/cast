use std::collections::BTreeMap;

use super::{BuildLockValidationError, LockedPackage, LockedRequest};

pub(super) fn require_nonempty(field: &str, value: &str) -> Result<(), BuildLockValidationError> {
    if value.is_empty() {
        Err(BuildLockValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn detect_dependency_cycles(
    packages: &[LockedPackage],
    indexes: &BTreeMap<&str, usize>,
) -> Result<(), BuildLockValidationError> {
    // 0 = unvisited, 1 = in the current DFS stack, 2 = complete.
    let mut states = vec![0_u8; packages.len()];
    let mut stack = Vec::new();
    for index in 0..packages.len() {
        visit_dependency(index, packages, indexes, &mut states, &mut stack)?;
    }
    Ok(())
}

pub(super) fn require_reachable_packages(
    packages: &[LockedPackage],
    requests: &[LockedRequest],
    indexes: &BTreeMap<&str, usize>,
) -> Result<(), BuildLockValidationError> {
    let mut reachable = vec![false; packages.len()];
    let mut pending = requests
        .iter()
        .map(|request| indexes[request.package_id.as_str()])
        .collect::<Vec<_>>();

    while let Some(index) = pending.pop() {
        if std::mem::replace(&mut reachable[index], true) {
            continue;
        }
        pending.extend(
            packages[index]
                .dependencies
                .iter()
                .map(|dependency| indexes[dependency.package_id.as_str()]),
        );
    }

    if let Some((index, package)) = packages.iter().enumerate().find(|(index, _)| !reachable[*index]) {
        return Err(BuildLockValidationError::UnreachablePackage {
            index,
            package: package.package_id.clone(),
        });
    }
    Ok(())
}

fn visit_dependency(
    index: usize,
    packages: &[LockedPackage],
    indexes: &BTreeMap<&str, usize>,
    states: &mut [u8],
    stack: &mut Vec<usize>,
) -> Result<(), BuildLockValidationError> {
    if states[index] == 2 {
        return Ok(());
    }
    states[index] = 1;
    stack.push(index);

    for (dependency_index, dependency) in packages[index].dependencies.iter().enumerate() {
        // Reference validation runs before cycle detection, so every package
        // is known here.
        let target = indexes[dependency.package_id.as_str()];
        if states[target] == 1 {
            let start = stack.iter().position(|candidate| *candidate == target).unwrap_or(0);
            let mut cycle = stack[start..]
                .iter()
                .map(|package_index| packages[*package_index].package_id.clone())
                .collect::<Vec<_>>();
            cycle.push(packages[target].package_id.clone());
            return Err(BuildLockValidationError::DependencyCycle {
                field: format!("packages[{index}].dependencies[{dependency_index}]"),
                cycle,
            });
        }
        if states[target] == 0 {
            visit_dependency(target, packages, indexes, states, stack)?;
        }
    }

    stack.pop();
    states[index] = 2;
    Ok(())
}
