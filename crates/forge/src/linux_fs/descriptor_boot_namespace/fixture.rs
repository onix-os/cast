use std::time::Instant;

use super::observer::{
    BootNamespaceDirectoryEntryObservation, BootNamespaceLookup, BootNamespaceNodeIdentity, BootNamespaceNodeKind,
    BootNamespaceObservationBoundary, BootNamespaceObserver, BootNamespaceObserverError, BootNamespaceRegularWitness,
    ObserverResult,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDirectoryEntry {
    pub(crate) raw_name: Vec<u8>,
    pub(crate) identity: BootNamespaceNodeIdentity,
    pub(crate) kind: BootNamespaceNodeKind,
}

impl FixtureDirectoryEntry {
    pub(crate) fn new(
        raw_name: impl Into<Vec<u8>>,
        identity: BootNamespaceNodeIdentity,
        kind: BootNamespaceNodeKind,
    ) -> Self {
        Self {
            raw_name: raw_name.into(),
            identity,
            kind,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureLookup {
    pub(crate) requested_name: Vec<u8>,
    pub(crate) opening: BootNamespaceLookup,
    pub(crate) closing: BootNamespaceLookup,
}

impl FixtureLookup {
    pub(crate) fn stable(requested_name: impl Into<Vec<u8>>, lookup: BootNamespaceLookup) -> Self {
        Self {
            requested_name: requested_name.into(),
            opening: lookup,
            closing: lookup,
        }
    }

    pub(crate) fn changing(
        requested_name: impl Into<Vec<u8>>,
        opening: BootNamespaceLookup,
        closing: BootNamespaceLookup,
    ) -> Self {
        Self {
            requested_name: requested_name.into(),
            opening,
            closing,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDirectory {
    pub(crate) identity: BootNamespaceNodeIdentity,
    pub(crate) opening_entries: Vec<FixtureDirectoryEntry>,
    pub(crate) closing_entries: Vec<FixtureDirectoryEntry>,
    pub(crate) lookups: Vec<FixtureLookup>,
}

impl FixtureDirectory {
    pub(crate) fn stable(identity: BootNamespaceNodeIdentity, entries: Vec<FixtureDirectoryEntry>) -> Self {
        Self {
            identity,
            closing_entries: entries.clone(),
            opening_entries: entries,
            lookups: Vec::new(),
        }
    }

    pub(crate) fn changing(
        identity: BootNamespaceNodeIdentity,
        opening_entries: Vec<FixtureDirectoryEntry>,
        closing_entries: Vec<FixtureDirectoryEntry>,
    ) -> Self {
        Self {
            identity,
            opening_entries,
            closing_entries,
            lookups: Vec::new(),
        }
    }

    pub(crate) fn with_lookup(mut self, lookup: FixtureLookup) -> Self {
        self.lookups.push(lookup);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureRegularFile {
    pub(crate) identity: BootNamespaceNodeIdentity,
    pub(crate) opening_witness: BootNamespaceRegularWitness,
    pub(crate) closing_witness: BootNamespaceRegularWitness,
    pub(crate) content: Vec<u8>,
    pub(crate) max_chunk: usize,
    pub(crate) stall_at: Option<u64>,
}

impl FixtureRegularFile {
    pub(crate) fn stable(
        identity: BootNamespaceNodeIdentity,
        witness: BootNamespaceRegularWitness,
        content: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            identity,
            opening_witness: witness,
            closing_witness: witness,
            content: content.into(),
            max_chunk: usize::MAX,
            stall_at: None,
        }
    }

    pub(crate) fn with_closing_witness(mut self, witness: BootNamespaceRegularWitness) -> Self {
        self.closing_witness = witness;
        self
    }

    pub(crate) fn with_max_chunk(mut self, max_chunk: usize) -> Self {
        self.max_chunk = max_chunk;
        self
    }

    pub(crate) fn with_stall_at(mut self, offset: u64) -> Self {
        self.stall_at = Some(offset);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureExpectedStream {
    pub(crate) content: Vec<u8>,
    pub(crate) max_chunk: usize,
    pub(crate) stall_at: Option<u64>,
}

impl FixtureExpectedStream {
    pub(crate) fn new(content: impl Into<Vec<u8>>) -> Self {
        Self {
            content: content.into(),
            max_chunk: usize::MAX,
            stall_at: None,
        }
    }

    pub(crate) fn with_max_chunk(mut self, max_chunk: usize) -> Self {
        self.max_chunk = max_chunk;
        self
    }

    pub(crate) fn with_stall_at(mut self, offset: u64) -> Self {
        self.stall_at = Some(offset);
        self
    }
}

#[derive(Debug)]
pub(crate) struct FixtureBootNamespace {
    pub(crate) root: BootNamespaceNodeIdentity,
    pub(crate) directories: Vec<FixtureDirectory>,
    pub(crate) regular_files: Vec<FixtureRegularFile>,
    pub(crate) expected_streams: Vec<FixtureExpectedStream>,
    now: Instant,
    expired_now: Instant,
    expire_after_now_call: Option<usize>,
    now_calls: usize,
    allocation_failure: Option<usize>,
    observation_failure: Option<usize>,
    observation_calls: usize,
    actual_read_calls: usize,
    expected_read_calls: usize,
}

impl FixtureBootNamespace {
    pub(crate) fn new(
        root: BootNamespaceNodeIdentity,
        directories: Vec<FixtureDirectory>,
        regular_files: Vec<FixtureRegularFile>,
        expected_streams: Vec<FixtureExpectedStream>,
        now: Instant,
    ) -> Self {
        Self {
            root,
            directories,
            regular_files,
            expected_streams,
            now,
            expired_now: now,
            expire_after_now_call: None,
            now_calls: 0,
            allocation_failure: None,
            observation_failure: None,
            observation_calls: 0,
            actual_read_calls: 0,
            expected_read_calls: 0,
        }
    }

    pub(crate) fn fail_allocation_at(mut self, attempt: usize) -> Self {
        self.allocation_failure = Some(attempt);
        self
    }

    pub(crate) fn fail_observation_at(mut self, attempt: usize) -> Self {
        self.observation_failure = Some(attempt);
        self
    }

    pub(crate) fn expire_after_now_call(mut self, call: usize, expired_now: Instant) -> Self {
        self.expire_after_now_call = Some(call);
        self.expired_now = expired_now;
        self
    }

    pub(crate) const fn now_calls(&self) -> usize {
        self.now_calls
    }

    pub(crate) const fn read_calls(&self) -> (usize, usize) {
        (self.actual_read_calls, self.expected_read_calls)
    }

    fn observe<T>(&mut self, value: T) -> ObserverResult<T> {
        self.observation_calls = self.observation_calls.saturating_add(1);
        if self.observation_failure == Some(self.observation_calls) {
            Err(BootNamespaceObserverError)
        } else {
            Ok(value)
        }
    }

    fn directory(&self, identity: BootNamespaceNodeIdentity) -> ObserverResult<&FixtureDirectory> {
        self.directories
            .iter()
            .find(|directory| directory.identity == identity)
            .ok_or(BootNamespaceObserverError)
    }

    fn regular_file(&self, identity: BootNamespaceNodeIdentity) -> ObserverResult<&FixtureRegularFile> {
        self.regular_files
            .iter()
            .find(|regular| regular.identity == identity)
            .ok_or(BootNamespaceObserverError)
    }
}

impl BootNamespaceObserver for FixtureBootNamespace {
    fn now(&mut self) -> Instant {
        self.now_calls = self.now_calls.saturating_add(1);
        if self.expire_after_now_call.is_some_and(|call| self.now_calls > call) {
            self.expired_now
        } else {
            self.now
        }
    }

    fn before_allocation(&mut self, attempt: usize) -> ObserverResult<()> {
        if self.allocation_failure == Some(attempt) {
            Err(BootNamespaceObserverError)
        } else {
            Ok(())
        }
    }

    fn root_identity(&mut self) -> ObserverResult<BootNamespaceNodeIdentity> {
        let root = self.root;
        self.observe(root)
    }

    fn directory_entry_count(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<usize> {
        let directory = self.directory(directory)?;
        let count = match boundary {
            BootNamespaceObservationBoundary::Opening => directory.opening_entries.len(),
            BootNamespaceObservationBoundary::Closing => directory.closing_entries.len(),
        };
        self.observe(count)
    }

    fn directory_entry(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
        index: usize,
        raw_name: &mut [u8],
    ) -> ObserverResult<BootNamespaceDirectoryEntryObservation> {
        let observation = {
            let directory = self.directory(directory)?;
            let entries = match boundary {
                BootNamespaceObservationBoundary::Opening => &directory.opening_entries,
                BootNamespaceObservationBoundary::Closing => &directory.closing_entries,
            };
            let entry = entries.get(index).ok_or(BootNamespaceObserverError)?;
            let copied = entry.raw_name.len().min(raw_name.len());
            raw_name[..copied].copy_from_slice(&entry.raw_name[..copied]);
            BootNamespaceDirectoryEntryObservation {
                name_length: entry.raw_name.len(),
                identity: entry.identity,
                kind: entry.kind,
            }
        };
        self.observe(observation)
    }

    fn lookup(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        requested_name: &[u8],
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<BootNamespaceLookup> {
        let lookup = {
            let directory = self.directory(directory)?;
            if let Some(rule) = directory
                .lookups
                .iter()
                .find(|rule| rule.requested_name == requested_name)
            {
                match boundary {
                    BootNamespaceObservationBoundary::Opening => rule.opening,
                    BootNamespaceObservationBoundary::Closing => rule.closing,
                }
            } else {
                let entries = match boundary {
                    BootNamespaceObservationBoundary::Opening => &directory.opening_entries,
                    BootNamespaceObservationBoundary::Closing => &directory.closing_entries,
                };
                entries.iter().find(|entry| entry.raw_name == requested_name).map_or(
                    BootNamespaceLookup::Absent,
                    |entry| BootNamespaceLookup::Present {
                        identity: entry.identity,
                        kind: entry.kind,
                    },
                )
            }
        };
        self.observe(lookup)
    }

    fn regular_witness(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<BootNamespaceRegularWitness> {
        let witness = {
            let regular = self.regular_file(identity)?;
            match boundary {
                BootNamespaceObservationBoundary::Opening => regular.opening_witness,
                BootNamespaceObservationBoundary::Closing => regular.closing_witness,
            }
        };
        self.observe(witness)
    }

    fn read_actual(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        offset: u64,
        output: &mut [u8],
    ) -> ObserverResult<usize> {
        self.actual_read_calls = self.actual_read_calls.saturating_add(1);
        let read = {
            let regular = self.regular_file(identity)?;
            copy_stream(&regular.content, regular.max_chunk, regular.stall_at, offset, output)?
        };
        self.observe(read)
    }

    fn read_expected(&mut self, request_index: usize, offset: u64, output: &mut [u8]) -> ObserverResult<usize> {
        self.expected_read_calls = self.expected_read_calls.saturating_add(1);
        let read = {
            let expected = self
                .expected_streams
                .get(request_index)
                .ok_or(BootNamespaceObserverError)?;
            copy_stream(&expected.content, expected.max_chunk, expected.stall_at, offset, output)?
        };
        self.observe(read)
    }
}

fn copy_stream(
    content: &[u8],
    max_chunk: usize,
    stall_at: Option<u64>,
    offset: u64,
    output: &mut [u8],
) -> ObserverResult<usize> {
    if stall_at == Some(offset) {
        return Ok(0);
    }
    let offset = usize::try_from(offset).map_err(|_| BootNamespaceObserverError)?;
    if offset >= content.len() {
        return Ok(0);
    }
    let read = (content.len() - offset).min(output.len()).min(max_chunk);
    output[..read].copy_from_slice(&content[offset..offset + read]);
    Ok(read)
}
