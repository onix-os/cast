use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorError {
    field: &'static str,
    value: String,
}

impl DescriptorError {
    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} descriptor {:?}",
            self.field, self.value
        )
    }
}

impl Error for DescriptorError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonicalName(String);

impl CanonicalName {
    fn parse(field: &'static str, value: impl Into<String>) -> Result<Self, DescriptorError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (index > 0 && matches!(byte, b'-' | b'_' | b'.'))
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(DescriptorError { field, value })
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(CanonicalName);

impl LanguageId {
    pub fn new(value: impl Into<String>) -> Result<Self, DescriptorError> {
        CanonicalName::parse("language", value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EngineId {
    implementation: CanonicalName,
    version: CanonicalName,
}

impl EngineId {
    pub fn new(
        implementation: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, DescriptorError> {
        Ok(Self {
            implementation: CanonicalName::parse("engine implementation", implementation)?,
            version: CanonicalName::parse("engine version", version)?,
        })
    }

    pub fn implementation(&self) -> &str {
        self.implementation.as_str()
    }

    pub fn version(&self) -> &str {
        self.version.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbiId {
    name: CanonicalName,
    version: CanonicalName,
}

impl AbiId {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, DescriptorError> {
        Ok(Self {
            name: CanonicalName::parse("ABI name", name)?,
            version: CanonicalName::parse("ABI version", version)?,
        })
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn version(&self) -> &str {
        self.version.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvaluatorPolicyId(CanonicalName);

impl EvaluatorPolicyId {
    pub fn new(value: impl Into<String>) -> Result<Self, DescriptorError> {
        CanonicalName::parse("evaluator policy", value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Immutable public identity and file profile of one declaration adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageSpec {
    language: LanguageId,
    engine: EngineId,
    extension: CanonicalName,
    source_profile: CanonicalName,
    generated_marker: String,
}

impl LanguageSpec {
    pub fn new(
        language: LanguageId,
        engine: EngineId,
        extension: impl Into<String>,
        source_profile: impl Into<String>,
        generated_marker: impl Into<String>,
    ) -> Result<Self, DescriptorError> {
        let generated_marker = generated_marker.into();
        if generated_marker.is_empty()
            || generated_marker.contains('\r')
            || !generated_marker.ends_with('\n')
            || generated_marker[..generated_marker.len() - 1].contains('\n')
        {
            return Err(DescriptorError {
                field: "generated marker",
                value: generated_marker,
            });
        }

        Ok(Self {
            language,
            engine,
            extension: CanonicalName::parse("extension", extension)?,
            source_profile: CanonicalName::parse("source profile", source_profile)?,
            generated_marker,
        })
    }

    pub fn language(&self) -> &LanguageId {
        &self.language
    }

    pub fn engine(&self) -> &EngineId {
        &self.engine
    }

    pub fn extension(&self) -> &str {
        self.extension.as_str()
    }

    pub fn source_profile(&self) -> &str {
        self.source_profile.as_str()
    }

    pub fn generated_marker(&self) -> &str {
        &self.generated_marker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec() -> LanguageSpec {
        LanguageSpec::new(
            LanguageId::new("fixture").unwrap(),
            EngineId::new("fixture-engine", "1.2.3").unwrap(),
            "decl",
            "declaration-v1",
            "# generated fixture\n",
        )
        .unwrap()
    }

    #[test]
    fn validated_descriptors_have_one_canonical_spelling() {
        let spec = fixture_spec();

        assert_eq!(spec.language().as_str(), "fixture");
        assert_eq!(spec.engine().implementation(), "fixture-engine");
        assert_eq!(spec.engine().version(), "1.2.3");
        assert_eq!(spec.extension(), "decl");
        assert_eq!(spec.source_profile(), "declaration-v1");
        assert_eq!(spec.generated_marker(), "# generated fixture\n");

        assert!(LanguageId::new("").is_err());
        assert!(LanguageId::new("Fixture").is_err());
        assert!(EngineId::new("fixture engine", "1.2.3").is_err());
        assert!(EngineId::new("fixture-engine", "").is_err());
        assert!(AbiId::new("package", "").is_err());
        assert!(EvaluatorPolicyId::new("policy 1").is_err());
    }

    #[test]
    fn extensions_and_generated_markers_are_not_sniffing_rules() {
        let language = LanguageId::new("fixture").unwrap();
        let engine = EngineId::new("fixture-engine", "1").unwrap();

        assert!(LanguageSpec::new(language.clone(), engine.clone(), ".decl", "v1", "# generated\n").is_err());
        assert!(LanguageSpec::new(language.clone(), engine.clone(), "decl", "v1", "missing newline").is_err());
        assert!(LanguageSpec::new(language, engine, "decl", "v1", "line\nbreak\n").is_err());
    }
}
