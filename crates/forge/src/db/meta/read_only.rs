//! Exact installed-package metadata queries over read-only SQLite.

use std::{collections::BTreeSet, str::FromStr};

use thiserror::Error;

use crate::{
    Dependency, Installation, Provider,
    db::{ReadOnlyConnection, ReadOnlyError, ReadOnlyStep},
    installation::DatabaseKind,
    package::{self, Meta, Name},
};

const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_RELATIONS_PER_KIND: usize = 4_096;
const MAX_RELATED_TEXT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ReadOnlyDatabase {
    connection: ReadOnlyConnection,
}

impl ReadOnlyDatabase {
    pub(crate) fn open(installation: &Installation) -> Result<Self, ReadOnlyMetaError> {
        Ok(Self {
            connection: ReadOnlyConnection::open(installation, DatabaseKind::Install)?,
        })
    }

    pub(crate) fn revalidate(&self, installation: &Installation) -> Result<(), ReadOnlyMetaError> {
        installation.revalidate_read_only_database(self.connection.anchor())?;
        Ok(())
    }

    pub(crate) fn get(&self, package: &package::Id) -> Result<Option<Meta>, ReadOnlyMetaError> {
        self.connection
            .snapshot(|row| {
                let mut statement = row.prepare(c"SELECT package, name, version_identifier, source_release, build_release, architecture, summary, description, source_id, homepage, uri, hash, download_size FROM meta WHERE package = ?1 LIMIT 2")?;
                statement.bind_text(1, package.as_str())?;
                if statement.step()? == ReadOnlyStep::Done {
                    return Ok(None);
                }
                if statement.text(0, MAX_TEXT_BYTES)? != package.as_str() {
                    return Err(ReadOnlyError::Policy {
                        context: "metadata lookup returned another stored package identifier",
                    });
                }
                let name = statement.text(1, MAX_TEXT_BYTES)?;
                let version_identifier = statement.text(2, MAX_TEXT_BYTES)?;
                let source_release = nonnegative_i32(statement.i64(3)?, "invalid source release")?;
                let build_release = nonnegative_i32(statement.i64(4)?, "invalid build release")?;
                let architecture = statement.text(5, MAX_TEXT_BYTES)?;
                let summary = statement.text(6, MAX_TEXT_BYTES)?;
                let description = statement.text(7, MAX_TEXT_BYTES)?;
                let source_id = statement.text(8, MAX_TEXT_BYTES)?;
                let homepage = statement.text(9, MAX_TEXT_BYTES)?;
                let uri = statement.nullable_text(10, MAX_TEXT_BYTES)?;
                let hash = statement.nullable_text(11, MAX_TEXT_BYTES)?;
                let download_size = statement
                    .nullable_i64(12)?
                    .map(|value| nonnegative(value, "negative download size"))
                    .transpose()?;
                if statement.step()? != ReadOnlyStep::Done {
                    return Err(ReadOnlyError::Policy {
                        context: "metadata primary-key lookup returned duplicate rows",
                    });
                }
                drop(statement);

                let licenses = string_relations(row, c"SELECT license FROM meta_licenses WHERE package = ?1 ORDER BY license LIMIT 4097", package)?;
                let dependencies = parsed_relations::<Dependency>(
                    row,
                    c"SELECT dependency FROM meta_dependencies WHERE package = ?1 ORDER BY dependency LIMIT 4097",
                    package,
                    "invalid stored dependency",
                )?;
                let providers = parsed_relations::<Provider>(
                    row,
                    c"SELECT provider FROM meta_providers WHERE package = ?1 ORDER BY provider LIMIT 4097",
                    package,
                    "invalid stored provider",
                )?;
                let conflicts = parsed_relations::<Provider>(
                    row,
                    c"SELECT conflict FROM meta_conflicts WHERE package = ?1 ORDER BY conflict LIMIT 4097",
                    package,
                    "invalid stored conflict",
                )?;

                let meta = Meta {
                    name: Name::from(name),
                    version_identifier,
                    source_release,
                    build_release,
                    architecture,
                    summary,
                    description,
                    source_id,
                    homepage,
                    licenses,
                    dependencies,
                    providers,
                    conflicts,
                    uri,
                    hash,
                    download_size,
                };
                let reconstructed: package::Id = meta.id().into();
                if &reconstructed != package {
                    return Err(ReadOnlyError::Policy {
                        context: "stored package identifier does not match reconstructed metadata identifier",
                    });
                }
                Ok(Some(meta))
            })
            .map_err(Into::into)
    }
}

fn string_relations(
    row: &crate::db::ReadOnlyRow,
    sql: &'static std::ffi::CStr,
    package: &package::Id,
) -> Result<Vec<String>, ReadOnlyError> {
    let mut statement = row.prepare(sql)?;
    statement.bind_text(1, package.as_str())?;
    let mut values = Vec::new();
    let mut bytes = 0usize;
    while statement.step()? == ReadOnlyStep::Row {
        if values.len() == MAX_RELATIONS_PER_KIND {
            return Err(ReadOnlyError::Limit {
                resource: "package relations",
                limit: MAX_RELATIONS_PER_KIND,
            });
        }
        let value = statement.text(0, MAX_TEXT_BYTES)?;
        bytes = bytes.checked_add(value.len()).ok_or(ReadOnlyError::Limit {
            resource: "package relation text bytes",
            limit: MAX_RELATED_TEXT_BYTES,
        })?;
        if bytes > MAX_RELATED_TEXT_BYTES {
            return Err(ReadOnlyError::Limit {
                resource: "package relation text bytes",
                limit: MAX_RELATED_TEXT_BYTES,
            });
        }
        values.push(value);
    }
    Ok(values)
}

fn parsed_relations<T>(
    row: &crate::db::ReadOnlyRow,
    sql: &'static std::ffi::CStr,
    package: &package::Id,
    invalid: &'static str,
) -> Result<BTreeSet<T>, ReadOnlyError>
where
    T: Ord + FromStr,
{
    string_relations(row, sql, package)?
        .into_iter()
        .map(|value| T::from_str(&value).map_err(|_| ReadOnlyError::Policy { context: invalid }))
        .collect()
}

fn nonnegative(value: i64, context: &'static str) -> Result<u64, ReadOnlyError> {
    u64::try_from(value).map_err(|_| ReadOnlyError::Policy { context })
}

fn nonnegative_i32(value: i64, context: &'static str) -> Result<u64, ReadOnlyError> {
    let value = i32::try_from(value).map_err(|_| ReadOnlyError::Policy { context })?;
    u64::try_from(value).map_err(|_| ReadOnlyError::Policy { context })
}

#[derive(Debug, Error)]
pub(crate) enum ReadOnlyMetaError {
    #[error(transparent)]
    Database(#[from] ReadOnlyError),
    #[error(transparent)]
    Installation(#[from] crate::installation::Error),
}
