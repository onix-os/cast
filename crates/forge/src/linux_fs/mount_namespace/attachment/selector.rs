use std::{ffi::CString, io};

use super::super::filesystem::Operation;

pub(super) const MAX_SELECTOR_BYTES: usize = 4_095;
pub(super) const MAX_SELECTOR_COMPONENTS: usize = 128;
pub(super) const MAX_COMPONENT_BYTES: usize = 255;

/// One exact authored absolute selector, split without normalization.
pub(super) struct AttachmentSelector {
    authored: String,
    components: Vec<CString>,
}

impl AttachmentSelector {
    pub(super) fn parse(selector: &str, operation: &mut Operation<'_>) -> io::Result<Self> {
        operation.checkpoint()?;
        let bytes = selector.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_SELECTOR_BYTES || bytes.first() != Some(&b'/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("attachment selector must be an absolute path of 1..={MAX_SELECTOR_BYTES} bytes"),
            ));
        }
        if bytes == b"/" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment selector must name a destination below the task root",
            ));
        }
        if bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment selector contains NUL",
            ));
        }

        operation.charge(bytes.len().saturating_add(1), "validating exact attachment selector")?;
        let mut count = 0usize;
        for component in bytes[1..].split(|byte| *byte == b'/') {
            count = count
                .checked_add(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "attachment component count overflowed"))?;
            if count > MAX_SELECTOR_COMPONENTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("attachment selector exceeds {MAX_SELECTOR_COMPONENTS} components"),
                ));
            }
            if component.is_empty() || component.len() > MAX_COMPONENT_BYTES || component == b"." || component == b".."
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "attachment selector component {count} must contain 1..={MAX_COMPONENT_BYTES} non-dot bytes"
                    ),
                ));
            }
        }

        operation.charge(bytes.len().saturating_add(count), "copying bounded attachment selector")?;
        let mut authored = String::new();
        authored
            .try_reserve_exact(selector.len())
            .map_err(|source| io::Error::other(format!("could not allocate attachment selector: {source}")))?;
        authored.push_str(selector);

        let mut components = Vec::new();
        components
            .try_reserve_exact(count)
            .map_err(|source| io::Error::other(format!("could not allocate attachment components: {source}")))?;
        for component in bytes[1..].split(|byte| *byte == b'/') {
            components.push(CString::new(component).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment selector component contains NUL",
                )
            })?);
        }
        operation.checkpoint()?;
        Ok(Self { authored, components })
    }

    pub(super) fn authored(&self) -> &str {
        &self.authored
    }

    pub(super) fn components(&self) -> &[CString] {
        &self.components
    }
}
