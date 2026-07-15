use super::*;

pub(super) fn resolve_build_lock(
    builder: &Builder,
    requested: &[RequestedInput],
    request_fingerprint: &str,
    expected: &build_lock::ExpectedBuildLockContext,
    refresh: bool,
) -> Result<BuildLock, Error> {
    let installation = Installation::open(&builder.env.forge_dir, None)?;
    let mut client = forge::Client::builder(build::BUILD_REPOSITORY_CACHE_IDENTITY, installation)
        .repositories(builder.repositories().clone())
        .build()?;
    if refresh {
        runtime::block_on(client.refresh_repositories())?;
    } else {
        runtime::block_on(client.ensure_repos_initialized())?;
    }
    let references = requested.iter().map(|input| input.request.as_str()).collect::<Vec<_>>();
    let closure = client.resolve_available_closure(&references)?;
    let mut snapshots = closure
        .repository_snapshots
        .iter()
        .map(|snapshot| RepositorySnapshot {
            id: snapshot.id.to_string(),
            index_uri: snapshot.index_uri.to_string(),
            snapshot: snapshot.sha256.clone(),
        })
        .collect::<Vec<_>>();
    let packages = closure
        .packages
        .iter()
        .map(|resolved| LockedPackage {
            package_id: resolved.package.id.to_string(),
            name: resolved.package.meta.name.to_string(),
            version: format!(
                "{}-{}-{}",
                resolved.package.meta.version_identifier,
                resolved.package.meta.source_release,
                resolved.package.meta.build_release
            ),
            architecture: resolved.package.meta.architecture.clone(),
            repository: resolved.repository.to_string(),
            outputs: vec![LockedOutput { name: "out".to_owned() }],
            dependencies: resolved
                .dependencies
                .iter()
                .map(|dependency| LockedOutputRef {
                    package_id: dependency.to_string(),
                    output: "out".to_owned(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let used_repositories = packages
        .iter()
        .map(|package| package.repository.as_str())
        .collect::<BTreeSet<_>>();
    snapshots.retain(|snapshot| used_repositories.contains(snapshot.id.as_str()));
    let requested_origins = requested
        .iter()
        .map(|input| (input.request.as_str(), &input.origins))
        .collect::<BTreeMap<_, _>>();
    let requests = closure
        .requests
        .into_iter()
        .map(|request| {
            let origins =
                requested_origins
                    .get(request.request.as_str())
                    .ok_or_else(|| Error::UnclassifiedResolvedInput {
                        request: request.request.clone(),
                    })?;
            Ok(LockedRequest {
                request: request.request,
                package_id: request.package.to_string(),
                output: "out".to_owned(),
                origins: (*origins).clone(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let resolved_requests = requests
        .iter()
        .map(|request| request.request.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(missing) = requested
        .iter()
        .find(|input| !resolved_requests.contains(input.request.as_str()))
    {
        return Err(Error::MissingResolvedInput {
            request: missing.request.clone(),
        });
    }
    let mut lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: request_fingerprint.to_owned(),
        repositories: snapshots,
        requests,
        packages,
        build_platform: expected.build_platform.clone(),
        host_platform: expected.host_platform.clone(),
        target_platform: expected.target_platform.clone(),
        policy: expected.policy.clone(),
        target: expected.target.clone(),
        profile: expected.profile.clone(),
        toolchain: expected.toolchain.clone(),
        builder: expected.builder.clone(),
    };
    lock.normalize();
    lock.validate()?;
    Ok(lock)
}
