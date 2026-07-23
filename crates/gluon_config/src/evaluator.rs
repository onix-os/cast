use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
};

use gluon::{
    RootedThread, ThreadExt,
    import::{Import, add_extern_module},
    query::CompilationBase,
    vm::{
        api::{Getable, VmType},
        thread::ThreadInternal,
    },
};
use declarative_config::EvaluationDeadline;

use crate::{
    Diagnostic, EvaluationFingerprint, ImportPolicy, LimitKind, Limits, Source, SourceRoot,
    diagnostic::from_gluon,
    import::{PreparedImports, RestrictedImporter, prepare_imports},
};

const WATCHDOG_RUNNING: u8 = 0;
const WATCHDOG_COMPLETED: u8 = 1;
const WATCHDOG_TIMED_OUT: u8 = 2;

fn claim_watchdog_terminal_state(state: &AtomicU8, terminal_state: u8) -> bool {
    debug_assert!(matches!(terminal_state, WATCHDOG_COMPLETED | WATCHDOG_TIMED_OUT));
    state
        .compare_exchange(WATCHDOG_RUNNING, terminal_state, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation<T> {
    pub value: T,
    pub fingerprint: EvaluationFingerprint,
}

#[derive(Debug, Clone)]
pub struct Evaluator {
    limits: Limits,
    source_root: Option<SourceRoot>,
    import_policy: ImportPolicy,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Evaluator {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            source_root: None,
            import_policy: ImportPolicy::default(),
        }
    }

    pub fn with_source_root(mut self, source_root: SourceRoot) -> Self {
        self.source_root = Some(source_root);
        self
    }

    pub fn with_import_policy(mut self, import_policy: ImportPolicy) -> Self {
        self.import_policy = import_policy;
        self
    }

    pub fn import_policy(&self) -> &ImportPolicy {
        &self.import_policy
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn evaluate<T>(&self, source: &Source) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let deadline = EvaluationDeadline::start(self.limits.timeout);
        self.evaluate_with_inputs_until(source, &[], deadline)
    }

    pub fn evaluate_with_inputs<T>(&self, source: &Source, explicit_inputs: &[u8]) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let deadline = EvaluationDeadline::start(self.limits.timeout);
        self.evaluate_with_inputs_until(source, explicit_inputs, deadline)
    }

    fn evaluate_with_inputs_until<T>(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let source_name = source.logical_name();
        deadline.check(source_name)?;
        if source.text().len() > self.limits.max_source_bytes {
            return Err(Diagnostic::limit(
                LimitKind::SourceSize,
                Some(source_name.to_owned()),
                format!("source exceeds the {}-byte limit", self.limits.max_source_bytes),
            ));
        }
        if explicit_inputs.len() > self.limits.max_explicit_input_bytes {
            return Err(Diagnostic::limit(
                LimitKind::ExplicitInputSize,
                Some(source_name.to_owned()),
                format!(
                    "explicit evaluation inputs exceed the {}-byte limit",
                    self.limits.max_explicit_input_bytes
                ),
            ));
        }

        let parser_vm = self.build_vm(&PreparedImports::empty());
        deadline.check(source_name)?;
        let imports = prepare_imports(
            &parser_vm,
            &self.import_policy,
            self.source_root.as_ref(),
            self.limits,
            source,
            deadline,
        );
        // A parse/import error which completed after the deadline is still a
        // timeout. Check before exposing any competing diagnostic.
        deadline.check(source_name)?;
        let imports = imports?;
        let mut fingerprint_checkpoint = || deadline.check(source_name);
        let fingerprint = EvaluationFingerprint::new_checked(
            source,
            imports.fingerprints(),
            explicit_inputs,
            &mut fingerprint_checkpoint,
        )?;
        deadline.check(source_name)?;
        let vm = self.build_vm(&imports);
        deadline.check(source_name)?;
        self.run_until_deadline(vm, source, fingerprint, deadline)
    }

    fn run_until_deadline<T>(
        &self,
        vm: RootedThread,
        source: &Source,
        fingerprint: EvaluationFingerprint,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let source_name = source.logical_name();
        deadline.check(source_name)?;
        let state = Arc::new(AtomicU8::new(WATCHDOG_RUNNING));
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let watchdog_vm = vm.clone();
        let watchdog_state = Arc::clone(&state);
        let watchdog = thread::Builder::new()
            .name("gluon-config-watchdog".to_owned())
            .spawn(move || {
                let timed_out = match deadline.remaining_duration() {
                    Some(remaining) => {
                        matches!(done_rx.recv_timeout(remaining), Err(mpsc::RecvTimeoutError::Timeout))
                    }
                    None => true,
                };
                if timed_out && claim_watchdog_terminal_state(&watchdog_state, WATCHDOG_TIMED_OUT) {
                    watchdog_vm.interrupt();
                }
            });
        // Thread creation is part of the same budget, and a late spawn error
        // must not replace the configured time-limit diagnostic.
        let completed_spawn_in_time = !deadline.expired();
        let watchdog = match watchdog {
            Ok(watchdog) if completed_spawn_in_time => watchdog,
            Ok(watchdog) => {
                claim_watchdog_terminal_state(&state, WATCHDOG_TIMED_OUT);
                drop(done_tx);
                watchdog
                    .join()
                    .map_err(|_| Diagnostic::internal("evaluation watchdog panicked"))?;
                return Err(deadline.exceeded(source_name));
            }
            Err(_) if !completed_spawn_in_time => return Err(deadline.exceeded(source_name)),
            Err(error) => return Err(Diagnostic::internal(format!("start evaluation watchdog: {error}"))),
        };

        let result = catch_unwind(AssertUnwindSafe(|| vm.run_expr::<T>(source_name, source.text())));
        // Claim completion before watchdog cleanup. The compare-exchange makes
        // completion versus timeout a single race with one winner: cleanup
        // latency cannot relabel a completed evaluation, and a watchdog which
        // already won cannot be overwritten by a late result.
        let completed_state = if deadline.expired() {
            WATCHDOG_TIMED_OUT
        } else {
            WATCHDOG_COMPLETED
        };
        claim_watchdog_terminal_state(&state, completed_state);
        let _ = done_tx.send(());
        let watchdog_result = watchdog.join();
        let completed_state = state.load(Ordering::Acquire);

        if completed_state == WATCHDOG_TIMED_OUT {
            return Err(deadline.exceeded(source_name));
        }
        watchdog_result.map_err(|_| Diagnostic::internal("evaluation watchdog panicked"))?;
        if completed_state != WATCHDOG_COMPLETED {
            return Err(Diagnostic::internal(
                "evaluation watchdog ended without a terminal state",
            ));
        }

        match result {
            Ok(Ok((value, _))) => Ok(Evaluation { value, fingerprint }),
            Ok(Err(error)) => Err(from_gluon(error, false)),
            Err(_) => Err(Diagnostic::internal(format!(
                "Gluon panicked while evaluating {source_name}"
            ))),
        }
    }

    pub fn evaluate_file<T>(&self, relative: impl AsRef<std::path::Path>) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let deadline = EvaluationDeadline::start(self.limits.timeout);
        let relative = relative.as_ref();
        let requested_name = relative.to_string_lossy();
        deadline.check(&requested_name)?;
        let source_root = self
            .source_root
            .as_ref()
            .ok_or_else(|| Diagnostic::internal("evaluate_file requires an explicit SourceRoot"))?;
        let source = source_root.load(relative, self.limits.max_source_bytes);
        // Loading the root file is part of evaluate_file's one total budget.
        deadline.check(&requested_name)?;
        let source = source?;
        self.evaluate_with_inputs_until(&source, &[], deadline)
    }

    fn build_vm(&self, imports: &PreparedImports) -> RootedThread {
        let vm = RootedThread::new();
        let import = Import::new(RestrictedImporter::allowing(imports.allowed_modules()));
        import.set_paths(Vec::new());
        vm.get_macros().insert("import".to_owned(), import);
        add_extern_module(&vm, "std.array.prim", gluon::vm::primitives::load_array);
        add_extern_module(&vm, "std.string.prim", gluon::vm::primitives::load_string);
        {
            let mut database = vm.get_database_mut();
            database.set_implicit_prelude(false);
            database.set_use_standard_lib(false);
            database.set_run_io(false);
            for (logical_name, source) in imports.module_sources() {
                database.add_module(logical_name.to_owned(), source);
            }
        }
        vm.set_memory_limit(self.limits.memory_bytes);
        vm.context().set_max_stack_size(self.limits.max_stack_size);
        vm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_watchdog_terminal_state_wins_without_being_overwritten() {
        let completed_first = AtomicU8::new(WATCHDOG_RUNNING);
        assert!(claim_watchdog_terminal_state(&completed_first, WATCHDOG_COMPLETED));
        assert!(!claim_watchdog_terminal_state(&completed_first, WATCHDOG_TIMED_OUT));
        assert_eq!(completed_first.load(Ordering::Acquire), WATCHDOG_COMPLETED);

        let timeout_first = AtomicU8::new(WATCHDOG_RUNNING);
        assert!(claim_watchdog_terminal_state(&timeout_first, WATCHDOG_TIMED_OUT));
        assert!(!claim_watchdog_terminal_state(&timeout_first, WATCHDOG_COMPLETED));
        assert_eq!(timeout_first.load(Ordering::Acquire), WATCHDOG_TIMED_OUT);
    }
}
