use std::collections::{BTreeMap, BTreeSet};

use super::{
    ActiveReblitBootSchemaSemanticReason, ValidatedActiveReblitBootSchema, ValidatedActiveReblitFormerIdentity,
};

const MAX_ID_BYTES: usize = 255;
const MAX_TEXT_BYTES: usize = 4 * 1024;
const MAX_FORMER_IDENTITIES: usize = 64;
const MAX_FORMER_IDENTITY_BYTES: usize = 64 * 1024;
const MAX_OS_RELEASE_LINES: usize = 512;

pub(super) fn parse_os_info(
    source: &str,
) -> Result<ValidatedActiveReblitBootSchema, ActiveReblitBootSchemaSemanticReason> {
    let info = os_info::load_os_info(source).map_err(|_| ActiveReblitBootSchemaSemanticReason::InvalidDocument)?;
    let identity = info.metadata.identity;
    let os_id = validate_identifier(&identity.id)?;
    let os_name = validate_text(&identity.name)?;
    let display_name = validate_text(&identity.display)?;
    let mut former = Vec::with_capacity(identity.former_identities.len());
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    if identity.former_identities.len() > MAX_FORMER_IDENTITIES {
        return Err(ActiveReblitBootSchemaSemanticReason::TooManyFormerIdentities);
    }
    seen.insert(os_id.to_ascii_lowercase());
    for previous in identity.former_identities {
        let id = validate_identifier(&previous.id)?;
        let name = validate_text(&previous.name)?;
        bytes = bytes
            .checked_add(id.len())
            .and_then(|total| total.checked_add(name.len()))
            .ok_or(ActiveReblitBootSchemaSemanticReason::TooManyFormerIdentities)?;
        if bytes > MAX_FORMER_IDENTITY_BYTES {
            return Err(ActiveReblitBootSchemaSemanticReason::TooManyFormerIdentities);
        }
        if !seen.insert(id.to_ascii_lowercase()) {
            return Err(ActiveReblitBootSchemaSemanticReason::DuplicateFormerIdentity);
        }
        former.push(ValidatedActiveReblitFormerIdentity {
            id: id.into(),
            name: name.into(),
        });
    }
    Ok(ValidatedActiveReblitBootSchema {
        namespace: os_id.clone().into(),
        os_id: os_id.into(),
        os_name: os_name.into(),
        display_name: display_name.into(),
        former_identities: former.into_boxed_slice(),
    })
}

pub(super) fn parse_os_release(
    source: &[u8],
) -> Result<ValidatedActiveReblitBootSchema, ActiveReblitBootSchemaSemanticReason> {
    let source = std::str::from_utf8(source).map_err(|_| ActiveReblitBootSchemaSemanticReason::NonUtf8)?;
    if source
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r'))
    {
        return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
    }
    let mut fields = BTreeMap::<&str, String>::new();
    let mut line_count = 0usize;
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line_count = line_count.saturating_add(1);
        if line_count > MAX_OS_RELEASE_LINES {
            return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(ActiveReblitBootSchemaSemanticReason::InvalidDocument)?;
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
        }
        let value = decode_os_release_value(value.trim())?;
        if fields.insert(key, value).is_some() {
            return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
        }
    }
    let os_name = fields
        .get("NAME")
        .ok_or(ActiveReblitBootSchemaSemanticReason::MissingIdentity)
        .and_then(|value| validate_text(value))?;
    let os_id = fields
        .get("ID")
        .ok_or(ActiveReblitBootSchemaSemanticReason::MissingIdentity)
        .and_then(|value| validate_identifier(value))?;
    let display_name = fields
        .get("PRETTY_NAME")
        .map_or_else(|| Ok(os_name.clone()), |value| validate_text(value))?;
    Ok(ValidatedActiveReblitBootSchema {
        namespace: os_id.clone().into(),
        os_id: os_id.into(),
        os_name: os_name.into(),
        display_name: display_name.into(),
        former_identities: Box::new([]),
    })
}

fn decode_os_release_value(value: &str) -> Result<String, ActiveReblitBootSchemaSemanticReason> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let bytes = value.as_bytes();
    if matches!(bytes[0], b'\'' | b'"') {
        let quote = bytes[0];
        if bytes.len() < 2 || bytes[bytes.len() - 1] != quote {
            return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
        }
        let inner = &value[1..value.len() - 1];
        if quote == b'\'' {
            if inner.contains('\'') {
                return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
            }
            return Ok(inner.to_owned());
        }
        let mut decoded = String::with_capacity(inner.len());
        let mut escaped = false;
        for character in inner.chars() {
            if escaped {
                if !matches!(character, '"' | '\\' | '$' | '`') {
                    return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
                }
                decoded.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
            } else {
                decoded.push(character);
            }
        }
        if escaped {
            return Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument);
        }
        Ok(decoded)
    } else if value.contains(['\'', '"', '\\']) {
        Err(ActiveReblitBootSchemaSemanticReason::InvalidDocument)
    } else {
        Ok(value.to_owned())
    }
}

fn validate_identifier(value: &str) -> Result<String, ActiveReblitBootSchemaSemanticReason> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.is_ascii()
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || value.contains('~')
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
        || is_dos_reserved_component(value)
    {
        Err(ActiveReblitBootSchemaSemanticReason::UnsafeIdentifier)
    } else {
        Ok(value.to_owned())
    }
}

fn validate_text(value: &str) -> Result<String, ActiveReblitBootSchemaSemanticReason> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.chars().any(char::is_control) {
        Err(ActiveReblitBootSchemaSemanticReason::UnsafeText)
    } else {
        Ok(value.to_owned())
    }
}

fn is_dos_reserved_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4 && (&bytes[..3] == b"COM" || &bytes[..3] == b"LPT") && matches!(bytes[3], b'1'..=b'9')
}
