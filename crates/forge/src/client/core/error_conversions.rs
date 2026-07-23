impl From<crate::transition_identity::Error> for Error {
    fn from(source: crate::transition_identity::Error) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedExchangeFailure> for Error {
    fn from(source: RetainedExchangeFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedPreviousMoveFailure> for Error {
    fn from(source: RetainedPreviousMoveFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<ArchivedCandidateError> for Error {
    fn from(source: ArchivedCandidateError) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedArchivedCandidateMoveFailure> for Error {
    fn from(source: RetainedArchivedCandidateMoveFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedStagingWrapperRotationFailure> for Error {
    fn from(source: RetainedStagingWrapperRotationFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}
