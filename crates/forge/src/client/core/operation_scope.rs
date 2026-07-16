#[derive(Debug)]
enum Scope {
    Stateful,
    Ephemeral {
        destination: ExternalMaterializationAdmission,
    },
    Frozen {
        destination: FrozenRootDestination,
    },
}

#[derive(Debug)]
struct FrozenRootDestination {
    root_path: PathBuf,
    parent_path: PathBuf,
    name: CString,
    parent: fs::File,
    parent_identity: FrozenRootIdentity,
}

/// Exclusive cooperating-writer guard for one frozen publication namespace.
///
/// Linux rename and unlink syscalls cannot make their source operation
/// conditional on a previously observed inode. Forge clients therefore hold
/// this advisory directory lock across every preflight, namespace mutation,
/// reconciliation, and durability barrier. The separately opened descriptor
/// is intentionally owned by the guard: closing it releases the lock even
/// while the client's retained parent capability remains alive.
#[derive(Debug)]
struct FrozenDestinationLock {
    _directory: fs::File,
}

impl Scope {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Ephemeral { .. } | Self::Frozen { .. })
    }
}

#[cfg(test)]
std::thread_local! {
    static OBSERVED_TRIGGER_SCOPES: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EPHEMERAL_SYSTEM_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_EPHEMERAL_TRANSACTION_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_EPHEMERAL_SYSTEM_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_ephemeral_transaction_triggers(hook: impl FnOnce() + 'static) {
    BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn arm_before_ephemeral_system_triggers(hook: impl FnOnce() + 'static) {
    BEFORE_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_ephemeral_transaction_triggers() {
    BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_ephemeral_transaction_triggers() {}

#[cfg(test)]
fn before_ephemeral_system_triggers() {
    BEFORE_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_ephemeral_system_triggers() {}

#[cfg(test)]
fn arm_after_ephemeral_transaction_triggers(hook: impl FnOnce() + 'static) {
    AFTER_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn arm_after_ephemeral_system_triggers(hook: impl FnOnce() + 'static) {
    AFTER_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_ephemeral_transaction_triggers() {
    AFTER_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_ephemeral_transaction_triggers() {}

#[cfg(test)]
fn after_ephemeral_system_triggers() {
    AFTER_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_ephemeral_system_triggers() {}

#[cfg(test)]
fn observe_trigger_scope(scope: &TriggerScope<'_>) {
    let name = match scope {
        TriggerScope::Transaction(..) => "transaction",
        TriggerScope::RetainedTransaction {
            kind: postblit::RetainedTransactionKind::Stateful,
            ..
        } => "transaction",
        TriggerScope::RetainedTransaction {
            kind: postblit::RetainedTransactionKind::ArchivedRepair,
            ..
        } => "retained-transaction",
        TriggerScope::RetainedEphemeral {
            phase: postblit::RetainedEphemeralPhase::Transaction,
            ..
        } => "transaction",
        TriggerScope::RetainedEphemeral {
            phase: postblit::RetainedEphemeralPhase::System,
            ..
        } => "system",
        TriggerScope::System(..) => "system",
    };
    OBSERVED_TRIGGER_SCOPES.with(|observed| observed.borrow_mut().push(name));
}

#[cfg(test)]
fn take_observed_trigger_scopes() -> Vec<&'static str> {
    OBSERVED_TRIGGER_SCOPES.with(|observed| std::mem::take(&mut *observed.borrow_mut()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetMaterialization {
    HardLink,
    IndependentCopy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlitExecution {
    Parallel,
    Sequential,
}
