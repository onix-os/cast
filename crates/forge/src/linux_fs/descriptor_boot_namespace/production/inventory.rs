#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RawNameSpan {
    start: usize,
    length: u16,
}

impl RawNameSpan {
    pub(super) fn new(start: usize, length: usize) -> Self {
        Self {
            start,
            length: u16::try_from(length).expect("validated raw directory names fit in u16"),
        }
    }

    const fn start(self) -> usize {
        self.start
    }

    const fn length(self) -> usize {
        self.length as usize
    }
}

/// Closed raw-entry inventory.
///
/// Only uninterpreted component bytes are retained. Kernel inode and type
/// hints are intentionally absent because they are not identity evidence; the
/// future descriptor observer must establish identity and kind independently.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ProductionRawDirectoryInventory {
    names: Vec<u8>,
    entries: Vec<RawNameSpan>,
}

impl ProductionRawDirectoryInventory {
    pub(super) fn vectors_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<RawNameSpan>) {
        (&mut self.names, &mut self.entries)
    }

    pub(super) fn push_reserved(&mut self, raw_name: &[u8]) {
        let start = self.names.len();
        self.names.extend_from_slice(raw_name);
        self.entries.push(RawNameSpan::new(start, raw_name.len()));
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn raw_name(&self, index: usize) -> Option<&[u8]> {
        let span = *self.entries.get(index)?;
        let end = span.start().checked_add(span.length())?;
        self.names.get(span.start()..end)
    }
}
