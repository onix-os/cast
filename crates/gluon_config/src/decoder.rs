use std::marker::PhantomData;

use declarative_config::{Diagnostic, EvaluationDeadline, Source, TypedDecoder};
use gluon::{
    RootedThread, ThreadExt,
    vm::api::{Getable, VmType},
};

use crate::diagnostic::from_gluon;

pub(crate) struct GluonGetable<T>(PhantomData<fn() -> T>);

impl<T> GluonGetable<T> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> TypedDecoder<RootedThread> for GluonGetable<T>
where
    T: VmType + Send,
    for<'vm, 'value> T: Getable<'vm, 'value>,
{
    type Output = T;

    fn decode(
        self,
        runtime: &RootedThread,
        source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic> {
        runtime
            .run_expr::<T>(source.logical_name(), source.text())
            .map(|(value, _)| value)
            .map_err(|error| from_gluon(error, false))
    }
}
