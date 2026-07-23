/// Seed one HTTPS-identified archive into the normal content-addressed cache
/// from a tracked offline fixture.
///
/// This function is deliberately absent from production binaries. Tests can
/// prove real frozen execution without adding `file:` sources or mounting the
/// mutable recipe/fixture tree inside the build container.
pub(crate) fn import_locked_archive_fixture(
    source: &LockedSource,
    storage_dir: &Path,
    fixture: &Path,
) -> Result<(), Error> {
    let upstreams = locked_upstreams(std::slice::from_ref(source))?;
    let [Upstream::Plain(plain)] = upstreams.as_slice() else {
        return Err(Error::FixtureImportRequiresArchive);
    };
    plain.import_fixture(storage_dir, fixture)?;
    Ok(())
}

/// Seed one exact lock-pinned Git source from a bounded offline fixture bundle.
/// The mirror is still stored under the canonical HTTPS-derived cache identity
/// and is admitted only after commit and normalized-tree verification.
pub(crate) fn import_locked_git_fixture(
    source: &LockedSource,
    storage_dir: &Path,
    fixture: &Path,
    source_date_epoch: i64,
) -> Result<(), Error> {
    let mut upstreams = locked_upstreams(std::slice::from_ref(source))?;
    let [Upstream::Git(git)] = upstreams.as_mut_slice() else {
        return Err(Error::FixtureImportRequiresGit);
    };
    git.original_index = usize::try_from(source.order()).map_err(|_| Error::SourceOrderTooLarge(usize::MAX))?;
    Ok(runtime::block_on(git.import_fixture(
        storage_dir,
        fixture,
        source_date_epoch,
    ))?)
}
