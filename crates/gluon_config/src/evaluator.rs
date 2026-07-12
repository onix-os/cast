// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use gluon::{
    RootedThread, ThreadExt,
    import::Import,
    vm::{
        api::{Getable, VmType},
        thread::ThreadInternal,
    },
};

use crate::{Diagnostic, EvaluationFingerprint, LimitKind, Limits, Source, SourceRoot, import::RestrictedImporter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation<T> {
    pub value: T,
    pub fingerprint: EvaluationFingerprint,
}

#[derive(Debug, Clone)]
pub struct Evaluator {
    limits: Limits,
    source_root: Option<SourceRoot>,
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
        }
    }

    pub fn with_source_root(mut self, source_root: SourceRoot) -> Self {
        self.source_root = Some(source_root);
        self
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn evaluate<T>(&self, source: &Source) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        self.evaluate_with_inputs(source, &[])
    }

    pub fn evaluate_with_inputs<T>(&self, source: &Source, explicit_inputs: &[u8]) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        if source.text().len() > self.limits.max_source_bytes {
            return Err(Diagnostic::limit(
                LimitKind::SourceSize,
                Some(source.logical_name().to_owned()),
                format!("source exceeds the {}-byte limit", self.limits.max_source_bytes),
            ));
        }

        let fingerprint = EvaluationFingerprint::new(source, explicit_inputs);
        let vm = self.build_vm();
        let timed_out = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let watchdog_vm = vm.clone();
        let watchdog_timed_out = Arc::clone(&timed_out);
        let timeout = self.limits.timeout;
        let watchdog = thread::Builder::new()
            .name("gluon-config-watchdog".to_owned())
            .spawn(move || {
                if done_rx.recv_timeout(timeout).is_err() {
                    watchdog_timed_out.store(true, Ordering::Release);
                    watchdog_vm.interrupt();
                }
            })
            .map_err(|error| Diagnostic::internal(format!("start evaluation watchdog: {error}")))?;

        let result = catch_unwind(AssertUnwindSafe(|| {
            vm.run_expr::<T>(source.logical_name(), source.text())
        }));
        let _ = done_tx.send(());
        watchdog
            .join()
            .map_err(|_| Diagnostic::internal("evaluation watchdog panicked"))?;
        let timed_out = timed_out.load(Ordering::Acquire);

        if timed_out {
            return Err(Diagnostic::limit(
                LimitKind::Time,
                Some(source.logical_name().to_owned()),
                format!("evaluation exceeded its {timeout:?} deadline"),
            ));
        }

        match result {
            Ok(Ok((value, _))) => Ok(Evaluation { value, fingerprint }),
            Ok(Err(error)) => Err(Diagnostic::from_gluon(error, false)),
            Err(_) => Err(Diagnostic::internal(format!(
                "Gluon panicked while evaluating {}",
                source.logical_name()
            ))),
        }
    }

    pub fn evaluate_file<T>(&self, relative: impl AsRef<std::path::Path>) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        let source_root = self
            .source_root
            .as_ref()
            .ok_or_else(|| Diagnostic::internal("evaluate_file requires an explicit SourceRoot"))?;
        let source = source_root.load(relative, self.limits.max_source_bytes)?;
        self.evaluate(&source)
    }

    fn build_vm(&self) -> RootedThread {
        let vm = RootedThread::new();
        let import = Import::new(RestrictedImporter::closed());
        import.set_paths(Vec::new());
        vm.get_macros().insert("import".to_owned(), import);
        {
            let mut database = vm.get_database_mut();
            database.set_implicit_prelude(false);
            database.set_use_standard_lib(false);
            database.set_run_io(false);
        }
        vm.set_memory_limit(self.limits.memory_bytes);
        vm.context().set_max_stack_size(self.limits.max_stack_size);
        vm
    }
}
