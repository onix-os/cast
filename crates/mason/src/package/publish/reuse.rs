#[derive(Debug, Clone, Copy)]
struct ReuseContext<'a> {
    expected: &'a [Vec<u8>],
    output: &'a DirectoryHandle,
    final_name: &'a [u8],
    source_date_epoch: i64,
    deadline: &'a Deadline,
}

fn verify_reuse<F>(
    staged: &mut VerifiedBundle,
    published: &mut VerifiedBundle,
    context: ReuseContext<'_>,
    hook: &mut F,
) -> Result<(), PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    let ReuseContext {
        expected,
        output,
        final_name,
        source_date_epoch,
        deadline,
    } = context;
    let staged_first = digest_round(staged, expected, deadline)?;
    let published_first = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_first, &published_first)?;
    hook(PublishCheckpoint::BeforeReuseConfirmation)?;
    let staged_second = digest_round(staged, expected, deadline)?;
    let published_second = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_second, &published_second)?;
    if staged_first != staged_second || published_first != published_second {
        return Err(PublishError::ArtifactChanged {
            path: published.root.path.clone(),
        });
    }
    for entry in &published.entries {
        deadline.check("sync reused published artefact")?;
        entry.file.sync_all().map_err(|source| PublishError::SyncFile {
            path: entry.path.clone(),
            source,
        })?;
    }
    published.root.sync("reused published")?;
    output.sync("output after reuse confirmation")?;
    hook(PublishCheckpoint::AfterReuseDurabilitySync)?;
    let staged_durable = digest_round(staged, expected, deadline)?;
    let published_durable = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_durable, &published_durable)?;
    if staged_second != staged_durable || published_second != published_durable {
        return Err(PublishError::ArtifactChanged {
            path: published.root.path.clone(),
        });
    }
    published.root.require_path_identity("published")?;
    output.require_named_directory(
        final_name,
        published.root.identity,
        PUBLISHED_BUNDLE_MODE,
        Some(source_date_epoch),
    )?;
    staged.root.require_path_identity("staged")?;
    output.require_path_identity("output")
}

fn compare_digests(
    staged: &VerifiedBundle,
    published: &VerifiedBundle,
    staged_digests: &[[u8; 32]],
    published_digests: &[[u8; 32]],
) -> Result<(), PublishError> {
    for index in 0..staged_digests.len() {
        if staged.entries[index].witness.length != published.entries[index].witness.length
            || staged_digests[index] != published_digests[index]
        {
            return Err(PublishError::ContentMismatch {
                staged: staged.entries[index].path.clone(),
                published: published.entries[index].path.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Deadline {
    start: Instant,
    duration: Duration,
}

impl Deadline {
    fn new(duration: Duration) -> Self {
        Self {
            start: Instant::now(),
            duration,
        }
    }

    fn check(&self, operation: &'static str) -> Result<(), PublishError> {
        if self.start.elapsed() >= self.duration {
            Err(PublishError::Deadline {
                operation,
                limit: self.duration,
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
struct BundleSpec {
    name: Vec<u8>,
    maximum: u64,
}

fn bundle_specs(plan: &DerivationPlan, limits: PublishLimits) -> Result<Vec<BundleSpec>, PublishError> {
    let count = plan.outputs.len().checked_add(2).ok_or(PublishError::ResourceLimit {
        resource: "published artefact count",
        limit: limits.max_artefacts,
    })?;
    if count > limits.max_artefacts {
        return Err(PublishError::ResourceLimit {
            resource: "published artefact count",
            limit: limits.max_artefacts,
        });
    }
    let architecture = frozen_architecture(&plan.package.architecture);
    let mut specs = Vec::new();
    specs
        .try_reserve_exact(count)
        .map_err(|source| PublishError::Allocation {
            resource: "published artefact specifications",
            requested: count,
            detail: source.to_string(),
        })?;
    for output in &plan.outputs {
        specs.push(BundleSpec {
            name: copy_bytes(
                stone_artefact_filename(
                    &output.package_name,
                    &plan.package.version,
                    plan.package.source_release,
                    plan.package.build_release,
                    architecture,
                )
                .as_bytes(),
                "published Stone name",
            )?,
            maximum: limits.max_stone_bytes,
        });
    }
    for name in [
        binary_manifest_filename(architecture),
        jsonc_manifest_filename(architecture),
    ] {
        specs.push(BundleSpec {
            name: copy_bytes(name.as_bytes(), "published manifest name")?,
            maximum: limits.max_manifest_bytes,
        });
    }
    specs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    for spec in &specs {
        validate_component(&spec.name, "published artefact")?;
    }
    for pair in specs.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(PublishError::DuplicateName {
                name: OsString::from_vec(copy_bytes(&pair[0].name, "duplicate artefact name")?),
            });
        }
    }
    Ok(specs)
}

fn expected_names(specs: &[BundleSpec]) -> Result<Vec<Vec<u8>>, PublishError> {
    let mut names = Vec::new();
    names
        .try_reserve_exact(specs.len())
        .map_err(|source| PublishError::Allocation {
            resource: "expected published artefact names",
            requested: specs.len(),
            detail: source.to_string(),
        })?;
    for spec in specs {
        names.push(copy_bytes(&spec.name, "expected published artefact name")?);
    }
    Ok(names)
}

#[cfg(test)]
pub(super) fn expected_bundle_files(plan: &DerivationPlan) -> std::collections::BTreeSet<OsString> {
    bundle_specs(plan, PublishLimits::default())
        .expect("test plan has valid publication specifications")
        .into_iter()
        .map(|spec| OsString::from_vec(spec.name))
        .collect()
}
