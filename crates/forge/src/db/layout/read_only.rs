//! Bounded selected-layout queries over read-only SQLite.

use std::{collections::BTreeSet, ffi::CString};

use astr::AStr;
use stone::StonePayloadLayoutRecord;
use thiserror::Error;

use crate::{
    Installation,
    db::{ReadOnlyConnection, ReadOnlyError, ReadOnlyStep},
    installation::DatabaseKind,
    package,
};

const MAX_PACKAGES: usize = 4_096;
const MAX_PACKAGE_ID_BYTES: usize = 1024 * 1024;
const QUERY_CHUNK: usize = 256;
const MAX_LAYOUT_ROWS: usize = 262_144;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_LAYOUT_TEXT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ReadOnlyDatabase {
    connection: ReadOnlyConnection,
}

impl ReadOnlyDatabase {
    pub(crate) fn open(installation: &Installation) -> Result<Self, ReadOnlyLayoutError> {
        Ok(Self {
            connection: ReadOnlyConnection::open(installation, DatabaseKind::Layout)?,
        })
    }

    pub(crate) fn revalidate(&self, installation: &Installation) -> Result<(), ReadOnlyLayoutError> {
        installation.revalidate_read_only_database(self.connection.anchor())?;
        Ok(())
    }

    pub(crate) fn selected(
        &self,
        packages: &[package::Id],
    ) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, ReadOnlyLayoutError> {
        let packages = canonical_packages(packages)?;
        self.connection
            .snapshot(|row| {
                let mut output = Vec::new();
                let mut text_bytes = 0usize;
                for chunk in packages.chunks(QUERY_CHUNK) {
                    let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
                    let sql = CString::new(format!(
                        "SELECT package_id, uid, gid, mode, tag, entry_type, entry_value1, entry_value2 \
                         FROM layout WHERE package_id IN ({placeholders}) ORDER BY package_id, id LIMIT ?"
                    ))
                    .expect("generated selected-layout SQL contains no NUL");
                    let mut statement = row.prepare(&sql)?;
                    for (index, package) in chunk.iter().enumerate() {
                        statement.bind_text(
                            i32::try_from(index + 1).expect("layout query chunk fits i32"),
                            package.as_str(),
                        )?;
                    }
                    let remaining = MAX_LAYOUT_ROWS.saturating_sub(output.len());
                    statement.bind_i64(
                        i32::try_from(chunk.len() + 1).expect("layout query chunk fits i32"),
                        i64::try_from(remaining.saturating_add(1)).unwrap_or(i64::MAX),
                    )?;

                    while statement.step()? == ReadOnlyStep::Row {
                        if output.len() == MAX_LAYOUT_ROWS {
                            return Err(ReadOnlyError::Limit {
                                resource: "layout rows",
                                limit: MAX_LAYOUT_ROWS,
                            });
                        }
                        let package_id = statement.text(0, MAX_TEXT_BYTES)?;
                        let entry_type = statement.text(5, MAX_TEXT_BYTES)?;
                        let entry_value1 = statement.nullable_text(6, MAX_TEXT_BYTES)?;
                        let entry_value2 = statement.nullable_text(7, MAX_TEXT_BYTES)?;
                        text_bytes = text_bytes
                            .checked_add(package_id.len())
                            .and_then(|bytes| bytes.checked_add(entry_type.len()))
                            .and_then(|bytes| bytes.checked_add(entry_value1.as_ref().map_or(0, String::len)))
                            .and_then(|bytes| bytes.checked_add(entry_value2.as_ref().map_or(0, String::len)))
                            .ok_or(ReadOnlyError::Limit {
                                resource: "layout text bytes",
                                limit: MAX_LAYOUT_TEXT_BYTES,
                            })?;
                        if text_bytes > MAX_LAYOUT_TEXT_BYTES {
                            return Err(ReadOnlyError::Limit {
                                resource: "layout text bytes",
                                limit: MAX_LAYOUT_TEXT_BYTES,
                            });
                        }
                        let file =
                            super::decode_entry(entry_type, entry_value1.map(AStr::from), entry_value2.map(AStr::from))
                                .ok_or(ReadOnlyError::Policy {
                                    context: "layout row has an invalid entry encoding",
                                })?;
                        output.push((
                            package::Id::from(package_id),
                            StonePayloadLayoutRecord {
                                uid: u32_value(statement.i64(1)?, "layout uid is outside u32")?,
                                gid: u32_value(statement.i64(2)?, "layout gid is outside u32")?,
                                mode: u32_value(statement.i64(3)?, "layout mode is outside u32")?,
                                tag: u32_value(statement.i64(4)?, "layout tag is outside u32")?,
                                file,
                            },
                        ));
                    }
                }
                Ok(output)
            })
            .map_err(Into::into)
    }
}

fn canonical_packages(packages: &[package::Id]) -> Result<Vec<&package::Id>, ReadOnlyLayoutError> {
    if packages.len() > MAX_PACKAGES {
        return Err(ReadOnlyLayoutError::PackageLimit {
            limit: MAX_PACKAGES,
            actual: packages.len(),
        });
    }
    let mut bytes = 0usize;
    let mut seen = BTreeSet::new();
    let mut canonical = Vec::new();
    for package in packages {
        bytes = bytes.checked_add(package.as_str().len()).unwrap_or(usize::MAX);
        if bytes > MAX_PACKAGE_ID_BYTES {
            return Err(ReadOnlyLayoutError::PackageBytes {
                limit: MAX_PACKAGE_ID_BYTES,
                actual: bytes,
            });
        }
        if seen.insert(package.as_str()) {
            canonical.push(package);
        }
    }
    Ok(canonical)
}

fn u32_value(value: i64, context: &'static str) -> Result<u32, ReadOnlyError> {
    u32::try_from(value).map_err(|_| ReadOnlyError::Policy { context })
}

#[derive(Debug, Error)]
pub(crate) enum ReadOnlyLayoutError {
    #[error(transparent)]
    Database(#[from] ReadOnlyError),
    #[error(transparent)]
    Installation(#[from] crate::installation::Error),
    #[error("layout selection exceeds the {limit}-package bound (got {actual})")]
    PackageLimit { limit: usize, actual: usize },
    #[error("layout selection exceeds the {limit}-byte package-ID bound (got {actual})")]
    PackageBytes { limit: usize, actual: usize },
}
