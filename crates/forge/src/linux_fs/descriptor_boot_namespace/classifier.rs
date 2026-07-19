use std::time::Instant;

use xxhash_rust::xxh3::Xxh3;

use super::{
    budget::{Operation, RAW_NAME_BUFFER_BYTES, STREAM_BUFFER_BYTES},
    error::BootNamespaceAssessmentError,
    model::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState, BootNamespaceRequest,
        ValidatedBootNamespaceAssessment,
    },
    observer::{
        BootNamespaceLookup, BootNamespaceNodeIdentity, BootNamespaceNodeKind, BootNamespaceObservationBoundary,
        BootNamespaceObserver, BootNamespaceRegularWitness,
    },
    trie::{RequestTrie, RequestTrieEdge, RequestTrieNode},
};

#[cfg(test)]
use super::{fixture::FixtureBootNamespace, model::FixtureBootNamespaceUsage};

#[derive(Debug, Eq, PartialEq)]
struct InventoryEntry {
    raw_name: Vec<u8>,
    identity: BootNamespaceNodeIdentity,
    kind: BootNamespaceNodeKind,
}

pub(super) fn assess_with_observer_until<'a, Observer: BootNamespaceObserver>(
    requests: &'a [BootNamespaceRequest<'a>],
    limits: BootNamespaceAssessmentLimits,
    deadline: Instant,
    observer: &mut Observer,
) -> Result<(ValidatedBootNamespaceAssessment, AssessmentUsage), BootNamespaceAssessmentError> {
    let mut operation = Operation::new(observer, limits, deadline)?;
    let trie = RequestTrie::build(requests, &mut operation)?;
    let mut states = Vec::new();
    operation.reserve(&mut states, requests.len(), "allocating ordered destination states")?;
    states.resize(requests.len(), BootNamespaceDestinationState::Absent);

    if !requests.is_empty() {
        operation.acquire_descriptor(0, 0)?;
        let root = match operation.observe_retained(
            "observing the retained namespace root identity",
            |observer| observer.root_identity(),
            |observer, identity| observer.release_node(*identity),
        ) {
            Ok(root) => root,
            Err(error) => {
                operation.cancel_descriptor_reservation();
                return Err(error);
            }
        };
        let assessment = (|| {
            if !root.is_valid() {
                return Err(BootNamespaceAssessmentError::InvalidRootIdentity);
            }
            let mut bound_identities = Vec::new();
            operation.reserve(
                &mut bound_identities,
                1,
                "allocating bounded requested-identity mapping",
            )?;
            bound_identities.push(root);
            assess_directory(
                &trie,
                trie.root(),
                root,
                root.mount_id,
                requests,
                &mut states,
                &mut bound_identities,
                &mut operation,
            )
        })();
        operation.release_descriptor(root);
        assessment?;
    }

    operation.checkpoint()?;
    let usage = AssessmentUsage::from_operation(&operation);
    Ok((ValidatedBootNamespaceAssessment::new(states), usage))
}

#[cfg(test)]
pub(crate) fn assess_fixture_boot_namespace_until<'a>(
    requests: &'a [BootNamespaceRequest<'a>],
    limits: BootNamespaceAssessmentLimits,
    deadline: Instant,
    fixture: &mut FixtureBootNamespace,
) -> Result<(ValidatedBootNamespaceAssessment, FixtureBootNamespaceUsage), BootNamespaceAssessmentError> {
    assess_with_observer_until(requests, limits, deadline, fixture).map(|(assessment, usage)| (assessment, usage.0))
}

fn assess_directory<'a, Observer: BootNamespaceObserver>(
    trie: &RequestTrie<'a>,
    node: &RequestTrieNode<'a>,
    directory: BootNamespaceNodeIdentity,
    root_mount_id: u64,
    requests: &[BootNamespaceRequest<'a>],
    states: &mut [BootNamespaceDestinationState],
    bound_identities: &mut Vec<BootNamespaceNodeIdentity>,
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let opening = capture_inventory(directory, BootNamespaceObservationBoundary::Opening, operation)?;
    let mut opening_lookups = Vec::new();
    operation.reserve(
        &mut opening_lookups,
        node.children().len(),
        "allocating opening lookup observations",
    )?;

    for edge in node.children() {
        reject_ascii_fold_alias(edge, &opening, operation)?;
        operation.acquire_descriptor(edge.request_index(), edge.component_index())?;
        let lookup = match observe_lookup(directory, edge, BootNamespaceObservationBoundary::Opening, operation) {
            Ok(lookup) => lookup,
            Err(error) => {
                operation.cancel_descriptor_reservation();
                return Err(error);
            }
        };
        if lookup == BootNamespaceLookup::Absent {
            operation.cancel_descriptor_reservation();
        }
        let binding = bind_lookup(edge, lookup, &opening, root_mount_id, operation)
            .and_then(|()| bind_unique_requested_identity(lookup, bound_identities, operation));
        if let Err(error) = binding {
            if let BootNamespaceLookup::Present { identity, .. } = lookup {
                operation.release_descriptor(identity);
            }
            return Err(error);
        }
        opening_lookups.push(lookup);
        process_opening_lookup(
            trie,
            edge,
            lookup,
            root_mount_id,
            requests,
            states,
            bound_identities,
            operation,
        )?;
    }

    let closing = capture_inventory(directory, BootNamespaceObservationBoundary::Closing, operation)?;
    operation.charge_work(opening.len().max(1), "comparing complete directory inventories")?;
    if opening != closing {
        return Err(BootNamespaceAssessmentError::InventoryRace);
    }

    for (edge, opening_lookup) in node.children().iter().zip(opening_lookups) {
        let closing_lookup = observe_lookup(directory, edge, BootNamespaceObservationBoundary::Closing, operation)?;
        if matches!(
            (opening_lookup, closing_lookup),
            (BootNamespaceLookup::Absent, BootNamespaceLookup::Present { .. })
                | (BootNamespaceLookup::Present { .. }, BootNamespaceLookup::Absent)
        ) {
            return Err(BootNamespaceAssessmentError::UnstableAbsence {
                request_index: edge.request_index(),
                component_index: edge.component_index(),
            });
        }
        if opening_lookup != closing_lookup {
            return Err(BootNamespaceAssessmentError::LookupRace {
                request_index: edge.request_index(),
                component_index: edge.component_index(),
            });
        }
        bind_lookup(edge, closing_lookup, &closing, root_mount_id, operation)?;
    }
    operation.checkpoint()
}

fn process_opening_lookup<'a, Observer: BootNamespaceObserver>(
    trie: &RequestTrie<'a>,
    edge: &RequestTrieEdge<'a>,
    lookup: BootNamespaceLookup,
    root_mount_id: u64,
    requests: &[BootNamespaceRequest<'a>],
    states: &mut [BootNamespaceDestinationState],
    bound_identities: &mut Vec<BootNamespaceNodeIdentity>,
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let child = trie.node(edge.child());
    let BootNamespaceLookup::Present { identity, kind } = lookup else {
        return mark_subtree_absent(trie, child, states, operation);
    };

    let expected_kind = if child.children().is_empty() {
        BootNamespaceNodeKind::Regular
    } else {
        BootNamespaceNodeKind::Directory
    };
    if let Err(error) = require_kind(edge, kind, expected_kind) {
        operation.release_descriptor(identity);
        return Err(error);
    }
    let assessment = if expected_kind == BootNamespaceNodeKind::Directory {
        assess_directory(
            trie,
            child,
            identity,
            root_mount_id,
            requests,
            states,
            bound_identities,
            operation,
        )
    } else {
        let request_index = child
            .leaf_request()
            .expect("validated request trie regular leaf must own one request");
        assess_regular(request_index, requests[request_index], identity, operation).map(|state| {
            states[request_index] = state;
        })
    };
    operation.release_descriptor(identity);
    assessment
}

fn bind_unique_requested_identity<Observer: BootNamespaceObserver>(
    lookup: BootNamespaceLookup,
    bound_identities: &mut Vec<BootNamespaceNodeIdentity>,
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let BootNamespaceLookup::Present { identity, .. } = lookup else {
        return Ok(());
    };
    for bound in bound_identities.iter().copied() {
        operation.charge_work(1, "checking bounded requested-identity uniqueness")?;
        if bound == identity {
            return Err(BootNamespaceAssessmentError::DuplicateIdentityMapping);
        }
    }
    operation.reserve(bound_identities, 1, "allocating one bounded requested-identity mapping")?;
    bound_identities.push(identity);
    Ok(())
}

fn mark_subtree_absent<Observer: BootNamespaceObserver>(
    trie: &RequestTrie<'_>,
    node: &RequestTrieNode<'_>,
    states: &mut [BootNamespaceDestinationState],
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    operation.charge_work(1, "marking one stably absent request-trie node")?;
    if let Some(request_index) = node.leaf_request() {
        states[request_index] = BootNamespaceDestinationState::Absent;
    }
    for edge in node.children() {
        mark_subtree_absent(trie, trie.node(edge.child()), states, operation)?;
    }
    Ok(())
}

fn capture_inventory<Observer: BootNamespaceObserver>(
    directory: BootNamespaceNodeIdentity,
    boundary: BootNamespaceObservationBoundary,
    operation: &mut Operation<'_, Observer>,
) -> Result<Vec<InventoryEntry>, BootNamespaceAssessmentError> {
    let count = operation.observe("observing a bounded raw directory entry count", |observer| {
        observer.directory_entry_count(directory, boundary)
    })?;
    operation.charge_entries(count)?;
    let mut entries = Vec::new();
    operation.reserve(&mut entries, count, "allocating a bounded raw directory inventory")?;
    let mut raw_name = [0u8; RAW_NAME_BUFFER_BYTES];
    for index in 0..count {
        raw_name.fill(0);
        let observed = operation.observe("observing one raw directory entry", |observer| {
            observer.directory_entry(directory, boundary, index, &mut raw_name)
        })?;
        operation.charge_name(observed.name_length)?;
        let name = raw_name
            .get(..observed.name_length)
            .ok_or(BootNamespaceAssessmentError::RawNameLimitExceeded {
                limit: RAW_NAME_BUFFER_BYTES,
                found: observed.name_length,
            })?;
        if name.is_empty() || name == b"." || name == b".." || name.contains(&0) || name.contains(&b'/') {
            return Err(BootNamespaceAssessmentError::InvalidRawName);
        }
        if !observed.identity.is_valid() {
            return Err(BootNamespaceAssessmentError::InvalidObservedIdentity);
        }
        let mut owned_name = Vec::new();
        operation.reserve(&mut owned_name, name.len(), "allocating one bounded raw entry name")?;
        owned_name.extend_from_slice(name);
        entries.push(InventoryEntry {
            raw_name: owned_name,
            identity: observed.identity,
            kind: observed.kind,
        });
    }

    operation.charge_unstable_sort(entries.len(), "sorting a bounded raw directory inventory")?;
    entries.sort_unstable_by(|left, right| {
        left.raw_name
            .cmp(&right.raw_name)
            .then_with(|| left.identity.cmp(&right.identity))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    if entries.windows(2).any(|pair| pair[0].raw_name == pair[1].raw_name) {
        return Err(BootNamespaceAssessmentError::DuplicateRawName);
    }

    let mut identities = Vec::new();
    operation.reserve(
        &mut identities,
        entries.len(),
        "allocating bounded directory identity mapping",
    )?;
    identities.extend(entries.iter().map(|entry| entry.identity));
    operation.charge_unstable_sort(identities.len(), "sorting bounded directory identity mapping")?;
    identities.sort_unstable();
    if identities.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(BootNamespaceAssessmentError::DuplicateIdentityMapping);
    }
    Ok(entries)
}

fn reject_ascii_fold_alias<Observer: BootNamespaceObserver>(
    edge: &RequestTrieEdge<'_>,
    inventory: &[InventoryEntry],
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    for entry in inventory {
        operation.charge_work(1, "checking one raw ASCII-fold collision")?;
        if entry.raw_name != edge.component() && entry.raw_name.eq_ignore_ascii_case(edge.component()) {
            return Err(BootNamespaceAssessmentError::AsciiFoldAlias {
                request_index: edge.request_index(),
                component_index: edge.component_index(),
            });
        }
    }
    Ok(())
}

fn observe_lookup<Observer: BootNamespaceObserver>(
    directory: BootNamespaceNodeIdentity,
    edge: &RequestTrieEdge<'_>,
    boundary: BootNamespaceObservationBoundary,
    operation: &mut Operation<'_, Observer>,
) -> Result<BootNamespaceLookup, BootNamespaceAssessmentError> {
    let lookup = |observer: &mut Observer| {
        observer.lookup(
            directory,
            edge.component(),
            boundary,
            edge.request_index(),
            edge.component_index(),
        )
    };
    if boundary == BootNamespaceObservationBoundary::Opening {
        operation.observe_retained(
            "performing one bounded kernel-lookup observation",
            lookup,
            |observer, lookup| {
                if let BootNamespaceLookup::Present { identity, .. } = lookup {
                    observer.release_node(*identity);
                }
            },
        )
    } else {
        operation.observe("performing one bounded kernel-lookup observation", lookup)
    }
}

fn bind_lookup<Observer: BootNamespaceObserver>(
    edge: &RequestTrieEdge<'_>,
    lookup: BootNamespaceLookup,
    inventory: &[InventoryEntry],
    root_mount_id: u64,
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let BootNamespaceLookup::Present { identity, kind } = lookup else {
        for entry in inventory {
            operation.charge_work(1, "proving lookup absence against raw inventory")?;
            if entry.raw_name == edge.component() {
                return Err(BootNamespaceAssessmentError::LookupAbsenceInventoryConflict {
                    request_index: edge.request_index(),
                    component_index: edge.component_index(),
                });
            }
        }
        return Ok(());
    };
    if !identity.is_valid() {
        return Err(BootNamespaceAssessmentError::InvalidObservedIdentity);
    }
    if identity.mount_id != root_mount_id {
        return Err(BootNamespaceAssessmentError::CrossMount {
            request_index: edge.request_index(),
            component_index: edge.component_index(),
        });
    }
    let mut matched = None;
    for entry in inventory {
        operation.charge_work(1, "mapping kernel lookup identity to raw inventory")?;
        if entry.identity == identity {
            matched = Some(entry);
            break;
        }
    }
    let entry = matched.ok_or(BootNamespaceAssessmentError::LookupIdentityMissing {
        request_index: edge.request_index(),
        component_index: edge.component_index(),
    })?;
    if entry.raw_name != edge.component() {
        if entry.raw_name.eq_ignore_ascii_case(edge.component()) {
            return Err(BootNamespaceAssessmentError::AsciiFoldAlias {
                request_index: edge.request_index(),
                component_index: edge.component_index(),
            });
        }
        return Err(BootNamespaceAssessmentError::LookupRawNameMismatch {
            request_index: edge.request_index(),
            component_index: edge.component_index(),
        });
    }
    if entry.kind != kind {
        return Err(BootNamespaceAssessmentError::LookupKindMismatch {
            request_index: edge.request_index(),
            component_index: edge.component_index(),
        });
    }
    Ok(())
}

fn require_kind(
    edge: &RequestTrieEdge<'_>,
    found: BootNamespaceNodeKind,
    expected: BootNamespaceNodeKind,
) -> Result<(), BootNamespaceAssessmentError> {
    if found == BootNamespaceNodeKind::Symlink {
        return Err(BootNamespaceAssessmentError::Symlink {
            request_index: edge.request_index(),
            component_index: edge.component_index(),
        });
    }
    if found != expected {
        return Err(BootNamespaceAssessmentError::WrongNodeKind {
            request_index: edge.request_index(),
            component_index: edge.component_index(),
            expected,
            found,
        });
    }
    Ok(())
}

fn assess_regular<Observer: BootNamespaceObserver>(
    request_index: usize,
    request: BootNamespaceRequest<'_>,
    identity: BootNamespaceNodeIdentity,
    operation: &mut Operation<'_, Observer>,
) -> Result<BootNamespaceDestinationState, BootNamespaceAssessmentError> {
    let opening = observe_regular_witness(identity, BootNamespaceObservationBoundary::Opening, operation)?;
    if opening.identity != identity {
        return Err(BootNamespaceAssessmentError::RegularWitnessIdentityMismatch { request_index });
    }

    let state = if opening.length != request.expected_length() {
        BootNamespaceDestinationState::Different
    } else {
        compare_regular_streams(request_index, request, opening, operation)?
    };

    let closing = observe_regular_witness(identity, BootNamespaceObservationBoundary::Closing, operation)?;
    if opening != closing {
        return Err(BootNamespaceAssessmentError::RegularContentRace { request_index });
    }
    Ok(state)
}

fn observe_regular_witness<Observer: BootNamespaceObserver>(
    identity: BootNamespaceNodeIdentity,
    boundary: BootNamespaceObservationBoundary,
    operation: &mut Operation<'_, Observer>,
) -> Result<BootNamespaceRegularWitness, BootNamespaceAssessmentError> {
    operation.observe("observing one regular content witness", |observer| {
        observer.regular_witness(identity, boundary)
    })
}

fn compare_regular_streams<Observer: BootNamespaceObserver>(
    request_index: usize,
    request: BootNamespaceRequest<'_>,
    witness: BootNamespaceRegularWitness,
    operation: &mut Operation<'_, Observer>,
) -> Result<BootNamespaceDestinationState, BootNamespaceAssessmentError> {
    let mut actual_hasher = Xxh3::new();
    let mut expected_hasher = Xxh3::new();
    let mut actual_buffer = [0u8; STREAM_BUFFER_BYTES];
    let mut expected_buffer = [0u8; STREAM_BUFFER_BYTES];
    let mut offset = 0u64;
    let mut exact = true;

    while offset < request.expected_length() {
        operation.checkpoint()?;
        let remaining = request.expected_length() - offset;
        let chunk =
            usize::try_from(remaining.min(STREAM_BUFFER_BYTES as u64)).expect("fixed stream chunk always fits usize");
        read_stream_chunk(
            ContentStream::Actual(witness.identity),
            request_index,
            offset,
            &mut actual_buffer[..chunk],
            operation,
        )?;
        read_stream_chunk(
            ContentStream::Expected,
            request_index,
            offset,
            &mut expected_buffer[..chunk],
            operation,
        )?;
        actual_hasher.update(&actual_buffer[..chunk]);
        expected_hasher.update(&expected_buffer[..chunk]);
        exact &= actual_buffer[..chunk] == expected_buffer[..chunk];
        offset = offset
            .checked_add(chunk as u64)
            .ok_or(BootNamespaceAssessmentError::ReadLimitExceeded {
                limit: operation.limits().max_read_bytes,
            })?;
    }

    require_stream_eof(
        ContentStream::Actual(witness.identity),
        request_index,
        offset,
        operation,
    )?;
    require_stream_eof(ContentStream::Expected, request_index, offset, operation)?;
    if expected_hasher.digest128() != request.expected_digest() {
        return Err(BootNamespaceAssessmentError::ExpectedContentProtocolViolation { request_index });
    }
    if actual_hasher.digest128() != witness.digest {
        return Err(BootNamespaceAssessmentError::ActualContentProtocolViolation { request_index });
    }
    Ok(if exact {
        BootNamespaceDestinationState::Exact
    } else {
        BootNamespaceDestinationState::Different
    })
}

#[derive(Clone, Copy)]
enum ContentStream {
    Actual(BootNamespaceNodeIdentity),
    Expected,
}

impl ContentStream {
    const fn label(self) -> &'static str {
        match self {
            Self::Actual(_) => "actual",
            Self::Expected => "expected",
        }
    }
}

fn read_stream_chunk<Observer: BootNamespaceObserver>(
    stream: ContentStream,
    request_index: usize,
    offset: u64,
    output: &mut [u8],
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let mut filled = 0usize;
    while filled < output.len() {
        let stream_offset =
            offset
                .checked_add(filled as u64)
                .ok_or(BootNamespaceAssessmentError::ReadLimitExceeded {
                    limit: operation.limits().max_read_bytes,
                })?;
        let offered = operation.bounded_read_window(output.len() - filled)?;
        let read = operation.observe("reading one fixed-size content stream chunk", |observer| match stream {
            ContentStream::Actual(identity) => {
                observer.read_actual(identity, stream_offset, &mut output[filled..filled + offered])
            }
            ContentStream::Expected => {
                observer.read_expected(request_index, stream_offset, &mut output[filled..filled + offered])
            }
        })?;
        if read == 0 {
            return Err(BootNamespaceAssessmentError::StreamStalled {
                request_index,
                stream: stream.label(),
            });
        }
        if read > offered {
            return Err(BootNamespaceAssessmentError::StreamOverflow {
                request_index,
                stream: stream.label(),
            });
        }
        operation.charge_read(read)?;
        filled += read;
    }
    Ok(())
}

fn require_stream_eof<Observer: BootNamespaceObserver>(
    stream: ContentStream,
    request_index: usize,
    offset: u64,
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let mut probe = [0u8; 1];
    operation.bounded_read_window(probe.len())?;
    operation.charge_read(probe.len())?;
    let read = operation.observe("probing exact content stream length", |observer| match stream {
        ContentStream::Actual(identity) => observer.read_actual(identity, offset, &mut probe),
        ContentStream::Expected => observer.read_expected(request_index, offset, &mut probe),
    })?;
    if read == 0 {
        Ok(())
    } else {
        Err(match stream {
            ContentStream::Actual(_) => BootNamespaceAssessmentError::ActualContentProtocolViolation { request_index },
            ContentStream::Expected => BootNamespaceAssessmentError::ExpectedContentProtocolViolation { request_index },
        })
    }
}

pub(super) struct AssessmentUsage(#[cfg(test)] FixtureBootNamespaceUsage);

impl AssessmentUsage {
    fn from_operation<Observer: BootNamespaceObserver>(operation: &Operation<'_, Observer>) -> Self {
        #[cfg(test)]
        {
            Self(operation.usage())
        }
        #[cfg(not(test))]
        {
            let _ = operation;
            Self()
        }
    }
}
