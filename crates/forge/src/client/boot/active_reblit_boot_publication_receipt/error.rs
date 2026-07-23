use std::{collections::TryReserveError, time::Instant};

use thiserror::Error;

use crate::{
    boot_publication::{BootPublicationReceiptBodyError, BootPublicationReceiptCodecError},
    transition_journal::CodecError,
};

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPublicationReceiptError {
    #[error("receipt deadline {actual:?} differs from the bound publication-plan deadline {expected:?}")]
    DeadlineMismatch { expected: Instant, actual: Instant },
    #[error("boot-publication receipt mapping exceeded its retained deadline at {checkpoint}")]
    DeadlineExceeded { checkpoint: &'static str },
    #[error("journal record is not the exact predecessor of BootSyncStarted: {0}")]
    InvalidPredecessor(CodecError),
    #[error("encode the exact boot-publication predecessor journal record: {0}")]
    PredecessorEncoding(CodecError),
    #[error("the desired inventory does not match the bound publication plan at {field}")]
    DesiredInventoryMismatch { field: &'static str },
    #[error("the bound publication plan's collision domains no longer match its retained scalar topology")]
    CollisionDomainDrift,
    #[error("receipt provenance claim count {actual} differs from canonical output count {expected}")]
    ProvenanceClaimCountMismatch { expected: usize, actual: usize },
    #[error("receipt provenance claim binding {index} does not match its exact canonical desired output")]
    ProvenanceClaimBindingMismatch { index: usize },
    #[error("the desired destination layout and retained mounted topology have different shapes")]
    TopologyLayoutMismatch,
    #[error("{destination} declarative PARTUUID differs from the retained authenticated partition UUID")]
    TopologyPartuuidMismatch { destination: &'static str },
    #[error("{destination} retained filesystem witness differs from its destination identity")]
    TopologyFilesystemWitnessMismatch { destination: &'static str },
    #[error("canonical desired output {index} is not UTF-8")]
    OutputPathNotUtf8 { index: usize },
    #[error("allocate {resource} while mapping the boot-publication receipt")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
    #[error(transparent)]
    ReceiptBody(#[from] BootPublicationReceiptBodyError),
    #[error(transparent)]
    ReceiptCodec(#[from] BootPublicationReceiptCodecError),
}
