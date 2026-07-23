use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProductionRawDirectorySourceError;

pub(super) type ProductionRawDirectorySourceResult<T> = Result<T, ProductionRawDirectorySourceError>;

/// Private capability seam for one already-retained directory descriptor.
///
/// `read_chunk` models one complete `getdents64` result. Implementations must
/// neither split records across calls nor return a count larger than `output`.
/// The protocol carries no path, descriptor, reopen closure, or mutation
/// operation into the parser.
pub(crate) trait ProductionRawDirectorySource {
    fn now(&mut self) -> Instant;

    fn before_allocation(&mut self, attempt: usize, bytes: usize) -> ProductionRawDirectorySourceResult<()>;

    fn read_chunk(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize>;

    /// Performs the one explicitly bounded terminal probe used when fewer
    /// than one maximum-size native record remains in the byte budget.
    /// Returning nonzero proves that the directory was not exhausted.
    fn probe_end(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize>;
}
