use std::{fs::File, os::fd::AsFd as _, time::Instant};

use super::super::super::observer::{
    BootNamespaceDirectoryEntryObservation, BootNamespaceNodeIdentity, BootNamespaceNodeKind,
    BootNamespaceObservationBoundary,
};
use super::super::{
    inventory::ProductionRawDirectoryInventory,
    model::ProductionRawDirectoryInventoryUsage,
    parser::parse_production_raw_directory_inventory_with_usage_until,
    source::{ProductionRawDirectorySource, ProductionRawDirectorySourceError, ProductionRawDirectorySourceResult},
};
use super::{
    error::RetainedBootNamespaceAssessmentError,
    hook::{FixtureRetainedBootNamespaceProtocolEvent, RetainedBootNamespaceHook},
    limits::LiveLedger,
    node::{
        NodeObservation, invalid_data, observe_readable, observe_retained_path, open_fresh_directory_reader,
        open_path_component,
    },
    syscall::getdents64_once,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InventoryNode {
    identity: BootNamespaceNodeIdentity,
    kind: BootNamespaceNodeKind,
}

pub(super) struct CachedInventory {
    directory: BootNamespaceNodeIdentity,
    boundary: BootNamespaceObservationBoundary,
    raw: ProductionRawDirectoryInventory,
    nodes: Vec<InventoryNode>,
}

impl CachedInventory {
    pub(super) const fn directory(&self) -> BootNamespaceNodeIdentity {
        self.directory
    }

    pub(super) const fn boundary(&self) -> BootNamespaceObservationBoundary {
        self.boundary
    }

    pub(super) fn len(&self) -> usize {
        self.raw.len()
    }

    pub(super) fn entry(
        &self,
        index: usize,
        output: &mut [u8],
    ) -> Result<BootNamespaceDirectoryEntryObservation, RetainedBootNamespaceAssessmentError> {
        let raw_name = self.raw.raw_name(index).ok_or_else(|| {
            invalid_data(
                "returning one cached raw directory entry",
                "classifier requested an out-of-range inventory entry",
            )
        })?;
        if raw_name.len() > output.len() {
            return Err(invalid_data(
                "returning one cached raw directory entry",
                "classifier supplied a short raw-name buffer",
            ));
        }
        let node = self.nodes.get(index).copied().ok_or_else(|| {
            invalid_data(
                "returning one cached raw directory entry",
                "raw-name and identity inventory lengths disagree",
            )
        })?;
        output[..raw_name.len()].copy_from_slice(raw_name);
        Ok(BootNamespaceDirectoryEntryObservation {
            name_length: raw_name.len(),
            identity: node.identity,
            kind: node.kind,
        })
    }
}

pub(super) fn capture_inventory<Hook: RetainedBootNamespaceHook>(
    directory_file: &File,
    directory_identity: BootNamespaceNodeIdentity,
    boundary: BootNamespaceObservationBoundary,
    ledger: &mut LiveLedger,
    hook: &mut Hook,
) -> Result<CachedInventory, RetainedBootNamespaceAssessmentError> {
    ledger.checkpoint()?;
    let retained_opening = observe_retained_path(
        directory_file,
        ledger,
        "observing retained directory before a fresh inventory",
    )?;
    require_directory(retained_opening, directory_identity)?;
    ledger.admit_inventory_pass()?;
    let reader = open_fresh_directory_reader(directory_file, retained_opening, ledger)?;
    let result = (|| {
        emit(
            hook,
            FixtureRetainedBootNamespaceProtocolEvent::FreshInventoryOpened { boundary },
        )?;
        let parser_limits = ledger.parser_limits()?;
        let (parsed, source_failure) = {
            let mut source = RetainedDirectorySource::new(reader.file(), ledger);
            let deadline = source.deadline();
            let parsed =
                parse_production_raw_directory_inventory_with_usage_until(&mut source, parser_limits, deadline);
            (parsed, source.take_failure())
        };
        if let Some(error) = source_failure {
            return Err(error);
        }
        let (raw, usage) = parsed.map_err(|source| RetainedBootNamespaceAssessmentError::RawInventory { source })?;
        charge_parser_usage(ledger, usage)?;

        observe_readable(
            reader.file(),
            retained_opening,
            true,
            ledger,
            "observing fresh directory after getdents64",
        )?;

        let mut nodes = Vec::new();
        ledger.reserve(&mut nodes, raw.len(), "allocating raw-entry identity observations")?;
        for index in 0..raw.len() {
            let raw_name = raw.raw_name(index).ok_or_else(|| {
                invalid_data(
                    "observing one raw directory name",
                    "closed parser inventory has an invalid name span",
                )
            })?;
            let entry = open_path_component(
                directory_file,
                raw_name,
                ledger,
                "opening one raw inventory name through retained directory authority",
            )?
            .ok_or_else(|| {
                invalid_data(
                    "opening one raw inventory name through retained directory authority",
                    "raw inventory name disappeared before identity observation",
                )
            })?;
            let entry_result: Result<InventoryNode, RetainedBootNamespaceAssessmentError> = (|| {
                let observed = observe_retained_path(
                    entry.file(),
                    ledger,
                    "observing one raw inventory entry identity and kind",
                )?;
                emit(
                    hook,
                    FixtureRetainedBootNamespaceProtocolEvent::RawEntryObserved { boundary, index },
                )?;
                Ok(InventoryNode {
                    identity: observed.identity,
                    kind: observed.kind,
                })
            })();
            entry.close(ledger);
            nodes.push(entry_result?);
        }

        let retained_closing = observe_retained_path(
            directory_file,
            ledger,
            "observing retained directory after a fresh inventory",
        )?;
        if retained_closing != retained_opening {
            return Err(invalid_data(
                "closing one fresh directory inventory",
                "retained directory changed around getdents64 and entry lookup",
            ));
        }
        emit(
            hook,
            FixtureRetainedBootNamespaceProtocolEvent::InventoryParsed {
                boundary,
                entries: raw.len(),
            },
        )?;
        ledger.checkpoint()?;
        Ok(CachedInventory {
            directory: directory_identity,
            boundary,
            raw,
            nodes,
        })
    })();
    reader.close(ledger);
    result
}

struct RetainedDirectorySource<'file, 'ledger> {
    directory: &'file File,
    ledger: &'ledger mut LiveLedger,
    failure: Option<RetainedBootNamespaceAssessmentError>,
}

impl<'file, 'ledger> RetainedDirectorySource<'file, 'ledger> {
    fn new(directory: &'file File, ledger: &'ledger mut LiveLedger) -> Self {
        Self {
            directory,
            ledger,
            failure: None,
        }
    }

    const fn deadline(&self) -> Instant {
        self.ledger.deadline()
    }

    fn take_failure(&mut self) -> Option<RetainedBootNamespaceAssessmentError> {
        self.failure.take()
    }

    fn read_once(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        let action = "issuing one raw getdents64 inventory syscall";
        let result = (|| {
            self.ledger.admit_observation_io_attempt(action)?;
            let found = getdents64_once(self.directory.as_fd(), output)
                .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem { action, source });
            self.ledger.complete_observation_io_attempt()?;
            found
        })();
        match result {
            Ok(found) => Ok(found),
            Err(error) => {
                if self.failure.is_none() {
                    self.failure = Some(error);
                }
                Err(ProductionRawDirectorySourceError)
            }
        }
    }
}

impl ProductionRawDirectorySource for RetainedDirectorySource<'_, '_> {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn before_allocation(&mut self, _attempt: usize, _bytes: usize) -> ProductionRawDirectorySourceResult<()> {
        Ok(())
    }

    fn read_chunk(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        self.read_once(output)
    }

    fn probe_end(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        self.read_once(output)
    }
}

fn require_directory(
    observed: NodeObservation,
    expected: BootNamespaceNodeIdentity,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    if observed.identity != expected || observed.kind != BootNamespaceNodeKind::Directory {
        Err(invalid_data(
            "binding retained directory inventory authority",
            "retained directory identity or kind changed",
        ))
    } else {
        Ok(())
    }
}

fn charge_parser_usage(
    ledger: &mut LiveLedger,
    usage: ProductionRawDirectoryInventoryUsage,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    ledger.charge_raw_usage(usage)
}

fn emit(
    hook: &mut impl RetainedBootNamespaceHook,
    event: FixtureRetainedBootNamespaceProtocolEvent,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    hook.emit(event)
        .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
            action: "running an injected retained-namespace protocol hook",
            source,
        })
}
