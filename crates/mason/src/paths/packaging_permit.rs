#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenPackagingBinding {
    workspace: PathBuf,
    workspace_identity: (u64, u64),
    derivation_id: DerivationId,
    lock_path: PathBuf,
}

/// Descriptor-free authority for one exact frozen packaging phase while the
/// supervising process retains the corresponding kernel lock.
///
/// The lifetime ties this value to that parent-owned guard. The child must
/// never receive the guard's host descriptors; it compares only this immutable
/// binding after payload descriptor sanitization.
#[derive(Debug)]
#[must_use = "a frozen packaging permit must be consumed inside the synchronous payload boundary"]
pub(crate) struct FrozenPackagingPermit<'lock> {
    binding: FrozenPackagingBinding,
    _lock: std::marker::PhantomData<&'lock ExecutionLock>,
}

impl<'lock> FrozenPackagingPermit<'lock> {
    fn new(binding: FrozenPackagingBinding) -> Self {
        Self {
            binding,
            _lock: std::marker::PhantomData,
        }
    }

    pub(crate) fn require_for(&self, binding: &FrozenPackagingBinding) -> io::Result<()> {
        if self.binding != *binding {
            return Err(invalid_binding(
                "frozen packaging permit does not authorize the requested workspace and derivation".to_owned(),
            ));
        }
        Ok(())
    }
}
