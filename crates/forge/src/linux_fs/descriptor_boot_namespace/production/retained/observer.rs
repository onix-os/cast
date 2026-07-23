use std::{fs::File, time::Instant};

use super::super::super::{
    model::BootNamespaceRequest,
    observer::{
        BootNamespaceDirectoryEntryObservation, BootNamespaceLookup, BootNamespaceNodeIdentity, BootNamespaceNodeKind,
        BootNamespaceObservationBoundary, BootNamespaceObserver, BootNamespaceObserverError,
        BootNamespaceRegularWitness, ObserverResult,
    },
};
use super::{
    content::{capture_regular_witness, pread_actual},
    error::RetainedBootNamespaceAssessmentError,
    expected::{
        BoundExpectedSourceEvidence, RetainedBootNamespaceExpectedSource, bind_expected_streams, read_expected,
        terminally_revalidate_expected_streams,
    },
    hook::{FixtureRetainedBootNamespaceProtocolEvent, RetainedBootNamespaceHook},
    inventory::{CachedInventory, capture_inventory},
    limits::{FixtureRetainedBootNamespaceUsage, LiveLedger},
    node::{
        AccountedFile, NodeObservation, NodeStat, invalid_data, observe_retained_path, open_path_component,
        open_regular_reader,
    },
};

enum RetainedFile<'root> {
    Borrowed(&'root File),
    Owned(AccountedFile),
}

impl RetainedFile<'_> {
    fn file(&self) -> &File {
        match self {
            Self::Borrowed(file) => file,
            Self::Owned(file) => file.file(),
        }
    }
}

struct RetainedNode<'root> {
    file: RetainedFile<'root>,
    observation: NodeObservation,
    request_index: Option<usize>,
    component_index: Option<usize>,
    expected_length: Option<u64>,
    reader: Option<AccountedFile>,
    opening_regular: Option<(NodeStat, BootNamespaceRegularWitness)>,
}

impl RetainedNode<'_> {
    fn identity(&self) -> BootNamespaceNodeIdentity {
        self.observation.identity
    }

    fn release(self, ledger: &mut LiveLedger) {
        let Self { file, reader, .. } = self;
        if let Some(reader) = reader {
            reader.close(ledger);
        }
        match file {
            RetainedFile::Owned(file) => file.close(ledger),
            RetainedFile::Borrowed(_) => ledger.release_descriptor_slot(),
        }
        ledger.release_node();
    }
}

pub(super) struct RetainedBootNamespaceObserver<'root, 'request, 'expected, 'source, Hook> {
    retained_root: &'root File,
    requests: &'request [BootNamespaceRequest<'request>],
    expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
    bound_expected: Vec<BoundExpectedSourceEvidence>,
    ledger: LiveLedger,
    nodes: Vec<RetainedNode<'root>>,
    inventory: Option<CachedInventory>,
    observed_root_identity: Option<BootNamespaceNodeIdentity>,
    failure: Option<RetainedBootNamespaceAssessmentError>,
    hook: Hook,
}

impl<'root, 'request, 'expected, 'source, Hook: RetainedBootNamespaceHook>
    RetainedBootNamespaceObserver<'root, 'request, 'expected, 'source, Hook>
{
    pub(super) fn new(
        retained_root: &'root File,
        requests: &'request [BootNamespaceRequest<'request>],
        expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
        limits: super::limits::RetainedBootNamespaceAssessmentLimits,
        deadline: Instant,
        hook: Hook,
    ) -> Result<Self, RetainedBootNamespaceAssessmentError> {
        let mut ledger = LiveLedger::new(limits, deadline)?;
        let bound_expected = bind_expected_streams(requests, expected, &mut ledger)?;
        Ok(Self {
            retained_root,
            requests,
            expected,
            bound_expected,
            ledger,
            nodes: Vec::new(),
            inventory: None,
            observed_root_identity: None,
            failure: None,
            hook,
        })
    }

    pub(super) fn take_failure(&mut self) -> Option<RetainedBootNamespaceAssessmentError> {
        self.failure.take()
    }

    pub(super) const fn observed_root_identity(&self) -> Option<BootNamespaceNodeIdentity> {
        self.observed_root_identity
    }

    pub(super) fn finish(
        &mut self,
        classified: bool,
    ) -> Result<FixtureRetainedBootNamespaceUsage, RetainedBootNamespaceAssessmentError> {
        self.inventory = None;
        let unreleased_nodes = !self.nodes.is_empty();
        if unreleased_nodes {
            self.clear_nodes();
        }
        if self.ledger.has_live_descriptor_slots() {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "production adapter returned with an unreleased descriptor admission slot",
            });
        }
        if unreleased_nodes {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier returned without releasing every retained node",
            });
        }
        if classified && self.requests.is_empty() && self.observed_root_identity.is_some() {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "empty classification unexpectedly observed a retained root",
            });
        }
        if classified && !self.requests.is_empty() && self.observed_root_identity.is_none() {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "successful nonempty classification omitted retained-root evidence",
            });
        }
        self.ledger.checkpoint()?;
        if self.failure.is_some() || !classified {
            return Ok(self.ledger.usage());
        }
        terminally_revalidate_expected_streams(self.requests, self.expected, &self.bound_expected, &mut self.ledger)?;
        self.hook
            .emit(FixtureRetainedBootNamespaceProtocolEvent::Complete)
            .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
                action: "running the terminal retained-namespace protocol hook",
                source,
            })?;
        self.ledger.checkpoint()?;
        Ok(self.ledger.usage())
    }

    fn require_healthy(&self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        if self.failure.is_some() {
            Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "observer was used after a prior production failure",
            })
        } else {
            self.ledger.checkpoint()
        }
    }

    fn return_result<T>(&mut self, result: Result<T, RetainedBootNamespaceAssessmentError>) -> ObserverResult<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if self.failure.is_none() {
                    self.failure = Some(error);
                }
                Err(BootNamespaceObserverError)
            }
        }
    }

    fn reserve_node_slot(&mut self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        self.ledger.acquire_node()?;
        if let Err(error) = self
            .ledger
            .reserve(&mut self.nodes, 1, "allocating one retained namespace node")
        {
            self.ledger.release_node();
            return Err(error);
        }
        Ok(())
    }

    fn push_reserved_node(&mut self, node: RetainedNode<'root>) {
        debug_assert!(self.nodes.len() < self.nodes.capacity());
        self.nodes.push(node);
    }

    fn clear_nodes(&mut self) {
        while let Some(node) = self.nodes.pop() {
            node.release(&mut self.ledger);
        }
    }

    fn root_identity_impl(&mut self) -> Result<BootNamespaceNodeIdentity, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        if !self.nodes.is_empty() || self.observed_root_identity.is_some() {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "root was requested more than once",
            });
        }
        self.reserve_node_slot()?;
        if let Err(error) = self
            .ledger
            .reserve_descriptor_slot("binding the borrowed retained namespace root descriptor")
        {
            self.ledger.release_node();
            return Err(error);
        }
        let observed = match observe_retained_path(
            self.retained_root,
            &mut self.ledger,
            "observing borrowed retained namespace root",
        ) {
            Ok(observed) => observed,
            Err(error) => {
                self.ledger.release_descriptor_slot();
                self.ledger.release_node();
                return Err(error);
            }
        };
        if observed.kind != BootNamespaceNodeKind::Directory {
            self.ledger.release_descriptor_slot();
            self.ledger.release_node();
            return Err(invalid_data(
                "observing borrowed retained namespace root",
                "retained root is not a directory",
            ));
        }
        let identity = observed.identity;
        if let Err(source) = self
            .hook
            .emit(FixtureRetainedBootNamespaceProtocolEvent::RootRetained { identity })
        {
            self.ledger.release_descriptor_slot();
            self.ledger.release_node();
            return Err(RetainedBootNamespaceAssessmentError::Filesystem {
                action: "running the retained-root protocol hook",
                source,
            });
        }
        self.push_reserved_node(RetainedNode {
            file: RetainedFile::Borrowed(self.retained_root),
            observation: observed,
            request_index: None,
            component_index: None,
            expected_length: None,
            reader: None,
            opening_regular: None,
        });
        self.observed_root_identity = Some(identity);
        Ok(identity)
    }

    fn directory_entry_count_impl(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> Result<usize, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        let file = top_node(&self.nodes, directory)?.file.file();
        let inventory = capture_inventory(file, directory, boundary, &mut self.ledger, &mut self.hook)?;
        let count = inventory.len();
        self.inventory = Some(inventory);
        Ok(count)
    }

    fn directory_entry_impl(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
        index: usize,
        raw_name: &mut [u8],
    ) -> Result<BootNamespaceDirectoryEntryObservation, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        top_node(&self.nodes, directory)?;
        let inventory = self
            .inventory
            .as_ref()
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested an entry before its complete inventory",
            })?;
        if inventory.directory() != directory || inventory.boundary() != boundary {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier mixed directory inventory boundaries",
            });
        }
        inventory.entry(index, raw_name)
    }

    fn lookup_impl(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        requested_name: &[u8],
        boundary: BootNamespaceObservationBoundary,
        request_index: usize,
        component_index: usize,
    ) -> Result<BootNamespaceLookup, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        let retaining = boundary == BootNamespaceObservationBoundary::Opening;
        if retaining {
            self.reserve_node_slot()?;
        }
        let parent = match top_node(&self.nodes, directory) {
            Ok(node) => node.file.file(),
            Err(error) => {
                if retaining {
                    self.ledger.release_node();
                }
                return Err(error);
            }
        };
        let opened = match open_path_component(
            parent,
            requested_name,
            &mut self.ledger,
            "looking up one requested raw component",
        ) {
            Ok(opened) => opened,
            Err(error) => {
                if retaining {
                    self.ledger.release_node();
                }
                return Err(error);
            }
        };
        let Some(file) = opened else {
            if retaining {
                self.ledger.release_node();
            }
            let emitted = self
                .hook
                .emit(FixtureRetainedBootNamespaceProtocolEvent::LookupObserved {
                    boundary,
                    request_index,
                    component_index,
                    present: false,
                })
                .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
                    action: "running one absent-lookup protocol hook",
                    source,
                });
            return emitted.map(|()| BootNamespaceLookup::Absent);
        };
        let observed = match observe_retained_path(
            file.file(),
            &mut self.ledger,
            "observing one requested lookup identity and kind",
        ) {
            Ok(observed) => observed,
            Err(error) => {
                file.close(&mut self.ledger);
                if retaining {
                    self.ledger.release_node();
                }
                return Err(error);
            }
        };
        let lookup = BootNamespaceLookup::Present {
            identity: observed.identity,
            kind: observed.kind,
        };
        if boundary == BootNamespaceObservationBoundary::Closing {
            let emitted = self
                .hook
                .emit(FixtureRetainedBootNamespaceProtocolEvent::LookupObserved {
                    boundary,
                    request_index,
                    component_index,
                    present: true,
                })
                .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
                    action: "running one closing-lookup protocol hook",
                    source,
                });
            file.close(&mut self.ledger);
            return emitted.map(|()| lookup);
        }

        let request = match self.requests.get(request_index).copied() {
            Some(request) => request,
            None => {
                file.close(&mut self.ledger);
                self.ledger.release_node();
                return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                    reason: "classifier supplied an out-of-range request index",
                });
            }
        };
        let expected_length = request.expected_length();
        let reader = if observed.kind == BootNamespaceNodeKind::Regular && observed.stat.size == expected_length {
            match open_regular_reader(parent, requested_name, observed, &mut self.ledger) {
                Ok(reader) => Some(reader),
                Err(error) => {
                    file.close(&mut self.ledger);
                    self.ledger.release_node();
                    return Err(error);
                }
            }
        } else {
            None
        };
        if let Err(source) = self
            .hook
            .emit(FixtureRetainedBootNamespaceProtocolEvent::LookupObserved {
                boundary,
                request_index,
                component_index,
                present: true,
            })
            .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
                action: "running one opening-lookup protocol hook",
                source,
            })
        {
            if let Some(reader) = reader {
                reader.close(&mut self.ledger);
            }
            file.close(&mut self.ledger);
            self.ledger.release_node();
            return Err(source);
        }
        self.push_reserved_node(RetainedNode {
            file: RetainedFile::Owned(file),
            observation: observed,
            request_index: Some(request_index),
            component_index: Some(component_index),
            expected_length: Some(expected_length),
            reader,
            opening_regular: None,
        });
        Ok(lookup)
    }

    fn regular_witness_impl(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> Result<BootNamespaceRegularWitness, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        let node = top_node(&self.nodes, identity)?;
        let request_index = node
            .request_index
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested regular content for the retained root",
            })?;
        let expected_length = node
            .expected_length
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "retained regular node has no expected length",
            })?;
        let (witness, stat) = capture_regular_witness(
            node.file.file(),
            node.reader.as_ref().map(AccountedFile::file),
            identity,
            expected_length,
            boundary,
            request_index,
            &mut self.ledger,
            &mut self.hook,
        )?;
        let node = self
            .nodes
            .last_mut()
            .expect("validated retained regular node remains on stack");
        match boundary {
            BootNamespaceObservationBoundary::Opening => {
                if node.opening_regular.replace((stat, witness)).is_some() {
                    return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                        reason: "classifier requested two opening regular witnesses",
                    });
                }
            }
            BootNamespaceObservationBoundary::Closing => {
                let opening = node
                    .opening_regular
                    .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                        reason: "classifier requested closing regular witness before opening",
                    })?;
                if opening != (stat, witness) {
                    return Err(invalid_data(
                        "closing one retained regular witness",
                        "regular metadata or digest changed across content comparison",
                    ));
                }
            }
        }
        Ok(witness)
    }

    fn read_actual_impl(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        let node = top_node(&self.nodes, identity)?;
        let request_index = node
            .request_index
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested root content",
            })?;
        let reader = node
            .reader
            .as_ref()
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested content from a non-readable node",
            })?;
        pread_actual(
            reader.file(),
            request_index,
            offset,
            output,
            &mut self.ledger,
            &mut self.hook,
        )
    }

    fn read_expected_impl(
        &mut self,
        request_index: usize,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, RetainedBootNamespaceAssessmentError> {
        self.require_healthy()?;
        let source =
            self.expected
                .get(request_index)
                .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                    reason: "classifier requested an out-of-range expected stream",
                })?;
        let evidence = self.bound_expected.get(request_index).copied().ok_or(
            RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested expected evidence outside the prebound range",
            },
        )?;
        let expected_length = self
            .requests
            .get(request_index)
            .copied()
            .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "classifier requested expected bytes outside the request range",
            })?
            .expected_length();
        read_expected(source, evidence, expected_length, offset, output, &mut self.ledger)
    }
}

fn top_node<'nodes, 'root>(
    nodes: &'nodes [RetainedNode<'root>],
    identity: BootNamespaceNodeIdentity,
) -> Result<&'nodes RetainedNode<'root>, RetainedBootNamespaceAssessmentError> {
    let node = nodes
        .last()
        .ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "classifier referenced a node with an empty retained stack",
        })?;
    if node.identity() != identity {
        return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "classifier referenced a retained node out of LIFO order",
        });
    }
    Ok(node)
}

impl<Hook: RetainedBootNamespaceHook> BootNamespaceObserver for RetainedBootNamespaceObserver<'_, '_, '_, '_, Hook> {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn before_allocation(&mut self, _attempt: usize) -> ObserverResult<()> {
        let result = self.require_healthy();
        self.return_result(result)
    }

    fn root_identity(&mut self) -> ObserverResult<BootNamespaceNodeIdentity> {
        let result = self.root_identity_impl();
        self.return_result(result)
    }

    fn directory_entry_count(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<usize> {
        let result = self.directory_entry_count_impl(directory, boundary);
        self.return_result(result)
    }

    fn directory_entry(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
        index: usize,
        raw_name: &mut [u8],
    ) -> ObserverResult<BootNamespaceDirectoryEntryObservation> {
        let result = self.directory_entry_impl(directory, boundary, index, raw_name);
        self.return_result(result)
    }

    fn lookup(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        requested_name: &[u8],
        boundary: BootNamespaceObservationBoundary,
        request_index: usize,
        component_index: usize,
    ) -> ObserverResult<BootNamespaceLookup> {
        let result = self.lookup_impl(directory, requested_name, boundary, request_index, component_index);
        self.return_result(result)
    }

    fn release_node(&mut self, identity: BootNamespaceNodeIdentity) {
        let matches = self.nodes.last().is_some_and(|node| node.identity() == identity);
        if !matches {
            if self.failure.is_none() {
                self.failure = Some(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                    reason: "classifier released retained nodes outside strict LIFO order",
                });
            }
            self.clear_nodes();
            return;
        }
        self.nodes
            .pop()
            .expect("validated retained-node stack is nonempty")
            .release(&mut self.ledger);
        if let Err(source) = self
            .hook
            .emit(FixtureRetainedBootNamespaceProtocolEvent::NodeReleased { identity })
        {
            if self.failure.is_none() {
                self.failure = Some(RetainedBootNamespaceAssessmentError::Filesystem {
                    action: "running one retained-node release protocol hook",
                    source,
                });
            }
        }
    }

    fn regular_witness(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<BootNamespaceRegularWitness> {
        let result = self.regular_witness_impl(identity, boundary);
        self.return_result(result)
    }

    fn read_actual(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        offset: u64,
        output: &mut [u8],
    ) -> ObserverResult<usize> {
        let result = self.read_actual_impl(identity, offset, output);
        self.return_result(result)
    }

    fn read_expected(&mut self, request_index: usize, offset: u64, output: &mut [u8]) -> ObserverResult<usize> {
        let result = self.read_expected_impl(request_index, offset, output);
        self.return_result(result)
    }
}
