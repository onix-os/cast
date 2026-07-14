//! Format-neutral trigger data transfer objects and domain conversion.

// `fnmatch::Pattern` contains a regex cache with interior mutability, but its
// ordering implementation uses only the immutable pattern and capture names.
#![allow(clippy::mutable_key_type)]

use std::{collections::BTreeMap, error::Error, fmt};

use fnmatch::Pattern;

use crate::format::{Handler, Inhibitors, PathDefinition, PathKind, Trigger};

/// A dynamic key/value entry represented without serializer-specific maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueSpec<T> {
    pub key: String,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerSpec {
    pub name: String,
    pub description: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub inhibitors: Option<InhibitorsSpec>,
    pub paths: Vec<KeyValueSpec<PathDefinitionSpec>>,
    pub handlers: Vec<KeyValueSpec<HandlerSpec>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InhibitorsSpec {
    pub paths: Vec<String>,
    pub environment: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDefinitionSpec {
    pub handlers: Vec<String>,
    pub kind: Option<PathKindSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKindSpec {
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerSpec {
    Run { command: String, args: Vec<String> },
    Delete { paths: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerConversionError {
    field: String,
    message: String,
}

impl TriggerConversionError {
    fn new(field: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            field: field.into(),
            message: error.to_string(),
        }
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TriggerConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid trigger specification at `{}`: {}",
            self.field, self.message
        )
    }
}

impl Error for TriggerConversionError {}

impl TryFrom<TriggerSpec> for Trigger {
    type Error = TriggerConversionError;

    fn try_from(spec: TriggerSpec) -> Result<Self, Self::Error> {
        let paths = spec
            .paths
            .into_iter()
            .enumerate()
            .try_fold(BTreeMap::new(), |mut paths, (index, entry)| {
                let pattern = entry
                    .key
                    .parse::<Pattern>()
                    .map_err(|error| TriggerConversionError::new(format!("paths[{index}].key"), error))?;
                if paths.insert(pattern, PathDefinition::from(entry.value)).is_some() {
                    return Err(TriggerConversionError::new(
                        format!("paths[{index}].key"),
                        "duplicate path pattern",
                    ));
                }
                Ok(paths)
            })?;
        let handlers =
            spec.handlers
                .into_iter()
                .enumerate()
                .try_fold(BTreeMap::new(), |mut handlers, (index, entry)| {
                    if handlers.insert(entry.key, Handler::from(entry.value)).is_some() {
                        return Err(TriggerConversionError::new(
                            format!("handlers[{index}].key"),
                            "duplicate handler name",
                        ));
                    }
                    Ok(handlers)
                })?;

        Ok(Self {
            name: spec.name,
            description: spec.description,
            before: spec.before,
            after: spec.after,
            inhibitors: spec.inhibitors.map(Into::into),
            paths,
            handlers,
        })
    }
}

impl From<InhibitorsSpec> for Inhibitors {
    fn from(spec: InhibitorsSpec) -> Self {
        Self {
            paths: spec.paths,
            environment: spec.environment,
        }
    }
}

impl From<PathDefinitionSpec> for PathDefinition {
    fn from(spec: PathDefinitionSpec) -> Self {
        Self {
            handlers: spec.handlers,
            kind: spec.kind.map(Into::into),
        }
    }
}

impl From<PathKindSpec> for PathKind {
    fn from(spec: PathKindSpec) -> Self {
        match spec {
            PathKindSpec::Directory => Self::Directory,
            PathKindSpec::Symlink => Self::Symlink,
        }
    }
}

impl From<HandlerSpec> for Handler {
    fn from(spec: HandlerSpec) -> Self {
        match spec {
            HandlerSpec::Run { command, args } => Self::Run { run: command, args },
            HandlerSpec::Delete { paths } => Self::Delete { delete: paths },
        }
    }
}
