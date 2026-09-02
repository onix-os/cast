// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    path::{Path, PathBuf},
    str,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use config::declaration::{
    LoadFixedRootDeclarationError, RegisteredLanguages,
    RootDeclarationDiscoveryError, RootDeclarationSlot, RootDeclarationSlotError,
    TypedDeclarationEvaluatorSet, load_fixed_root_declaration,
};
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, EvaluationDeadline,
    Evaluation as DeclarationEvaluation,
    LanguageSpec, Limits, Source, SourceRoot,
};
use fs_err as fs;
use declarative_config::EvaluationIdentity;
use stone_recipe::build_policy::{TargetEmulationSpec, TargetPolicySpec};
use stone_recipe::package::{
    BuilderSpec, HooksSpec, LuaPackageEvaluator, PackageConversionError,
    PackageSpec, PhasesSpec, ProfileSpec,
};
use thiserror::Error;

use crate::{
    generated_lock,
    source_lock::{self, LuaSourceLockCodec, SOURCE_LOCK_FILE_NAME, SourceLock},
};

const RECIPE_ROOT_BASENAME: &str = "stone";

const RECIPE_ROOT_LOGICAL_NAME_LUA: &str = "stone.lua";

#[derive(Debug)]
pub struct Recipe {
    pub path: PathBuf,
    pub source: String,
    /// Concrete package-v3 declaration produced by the authored factory.
    pub declaration: PackageSpec,
    pub source_lock: Option<SourceLock>,
    pub fingerprint: EvaluationIdentity,
    pub build_time: DateTime<Utc>,
}

impl Recipe {
    /// Desired recipe value invariants are checked here
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) =
            load_recipe_declaration(&path, SourceLockPolicy::RequireCurrent)?;

        Self::from_loaded(path, source, declaration, source_lock, fingerprint, None)
    }

    /// Load using an explicit reproducible build timestamp. No process
    /// environment, Git metadata, or clock fallback participates.
    pub(crate) fn load_at(path: impl AsRef<Path>, build_time: DateTime<Utc>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) =
            load_recipe_declaration(&path, SourceLockPolicy::RequireCurrent)?;
        Self::from_loaded(path, source, declaration, source_lock, fingerprint, Some(build_time))
    }

    /// Load only the authored expression, ignoring any generated source lock.
    ///
    /// This is reserved for lock regeneration: stale or malformed generated
    /// state must not prevent Cast from evaluating the authoritative source.
    pub(crate) fn load_authored(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) =
            load_recipe_declaration(&path, SourceLockPolicy::Ignore)?;

        Self::from_loaded(path, source, declaration, source_lock, fingerprint, None)
    }

    fn from_loaded(
        path: PathBuf,
        source: String,
        declaration: PackageSpec,
        source_lock: Option<SourceLock>,
        fingerprint: EvaluationIdentity,
        explicit_build_time: Option<DateTime<Utc>>,
    ) -> Result<Self, Error> {
        let build_time = explicit_build_time
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("the Unix epoch is a valid UTC timestamp"));

        Ok(Self {
            path,
            source,
            declaration,
            source_lock,
            fingerprint,
            build_time,
        })
    }

    /// Whether this recipe opts into one exact repository build target.
    ///
    /// An empty architecture list means every native policy target, plus
    /// explicitly enabled compatibility targets. Once a recipe declares an
    /// architecture list, only an exact target name or the typed `native` and
    /// `emul32` classes match. Target-name prefixes and host architecture are
    /// deliberately not inferred here.
    pub fn supports_target(&self, target: &TargetPolicySpec) -> bool {
        if self.declaration.architectures.is_empty() {
            matches!(&target.emulation, TargetEmulationSpec::Native) || self.declaration.emul32
        } else {
            self.declaration.architectures.iter().any(|declared| {
                declared == &target.name
                    || matches!(
                        (declared.as_str(), &target.emulation),
                        ("native", TargetEmulationSpec::Native) | ("emul32", TargetEmulationSpec::Emul32 { .. })
                    )
            })
        }
    }

    pub fn build_target_profile_key(&self, target: &TargetPolicySpec) -> Option<&str> {
        if let Some(profile) = self
            .declaration
            .profiles
            .iter()
            .find(|profile| profile.name == target.name)
        {
            Some(&profile.name)
        } else if matches!(&target.emulation, TargetEmulationSpec::Emul32 { .. }) {
            self.declaration
                .profiles
                .iter()
                .find(|profile| profile.name == "emul32")
                .map(|profile| profile.name.as_str())
        } else {
            None
        }
    }

    pub fn build_target_profile(&self, target: &TargetPolicySpec) -> Option<&ProfileSpec> {
        let key = self.build_target_profile_key(target)?;
        self.declaration.profiles.iter().find(|profile| profile.name == key)
    }

    /// Select the structural package-v3 builder for one target.
    pub fn build_target_builder(&self, target: &TargetPolicySpec) -> &BuilderSpec {
        self.declaration
            .builder_for_profile(self.build_target_profile_key(target))
    }

    /// Select the package-v3 hooks paired with the target's structural builder.
    pub fn build_target_hooks(&self, target: &TargetPolicySpec) -> &HooksSpec {
        self.declaration
            .hooks_for_profile(self.build_target_profile_key(target))
    }

    /// Select the structural package-v3 phases for one target.
    pub fn build_target_phases(&self, target: &TargetPolicySpec) -> PhasesSpec {
        self.declaration
            .phases_for_profile(self.build_target_profile_key(target))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceLockPolicy {
    RequireCurrent,
    Ignore,
}

fn load_recipe_declaration(
    path: &Path,
    source_lock_policy: SourceLockPolicy,
) -> Result<(String, PackageSpec, Option<SourceLock>, EvaluationIdentity), Error> {
    let parent = path.parent().ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    // The recipe language is selected by the file's extension (`stone.lua`);
    // an unregistered extension is a hard missing-recipe error.
    let extension = path.extension().and_then(|extension| extension.to_str());
    let registered_extensions = RecipeDeclarationEvaluator::registered(Arc::from(Vec::new()))
        .iter()
        .map(|evaluator| {
            <RecipeDeclarationEvaluator as DeclarationEvaluator<RecipeDeclaration>>::language_spec(
                evaluator,
            )
            .extension()
            .to_owned()
        })
        .collect::<Vec<_>>();
    if !registered_extensions.iter().any(|registered| Some(registered.as_str()) == extension) {
        return Err(Error::MissingRecipe(path.to_owned()));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    let basename = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;

    let lock_path = path.with_file_name(SOURCE_LOCK_FILE_NAME);
    let source_lock_codec = LuaSourceLockCodec::default();
    let (explicit_inputs, source_lock) = match source_lock_policy {
        SourceLockPolicy::Ignore => (Vec::new(), None),
        SourceLockPolicy::RequireCurrent => match generated_lock::read(
            &lock_path,
            <LuaSourceLockCodec as DeclarationEvaluator<SourceLock>>::limits(
                &source_lock_codec,
            )
            .max_source_bytes,
        ) {
            Ok(bytes) => {
                let text = str::from_utf8(&bytes).map_err(|source| {
                    Error::SourceLockUtf8 {
                        path: lock_path.clone(),
                        source,
                    }
                })?;
                let lock = source_lock_codec
                    .evaluate(&Source::new(SOURCE_LOCK_FILE_NAME, text))
                    .map(|evaluation| evaluation.value)
                    .map_err(|source| Error::EvaluateSourceLock {
                        path: lock_path.clone(),
                        source: Box::new(source),
                    })?;
                (bytes, Some(lock))
            }
            Err(error) if error.is_not_found() => (Vec::new(), None),
            Err(source) => {
                return Err(Error::LoadSourceLock {
                    path: lock_path,
                    source: Box::new(source),
                });
            }
        },
    };

    let slot = RootDeclarationSlot::new(basename, file_name).map_err(|source| {
        Error::InvalidRecipeSlot {
            path: path.to_owned(),
            source,
        }
    })?;
    let evaluators = TypedDeclarationEvaluatorSet::new(
        RecipeDeclarationEvaluator::registered(Arc::from(explicit_inputs)),
    )
    .expect("the recipe languages register distinct extensions");
    let evaluated = load_fixed_root_declaration(parent, &slot, &evaluators)
        .map_err(map_recipe_load_error)?
        .ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    if let Some(lock) = source_lock.as_ref() {
        lock.validate_against(&evaluated.value.package.sources)
            .map_err(|source| Error::StaleSourceLock {
                path: lock_path,
                source: Box::new(source),
            })?;
    }
    Ok((
        evaluated.value.source,
        evaluated.value.package,
        source_lock,
        evaluated.identity,
    ))
}

#[derive(Debug)]
struct RecipeDeclaration {
    source: String,
    package: PackageSpec,
}

/// One registered recipe declaration language, selected by the recipe file's
/// extension. Every engine reaches the same [`PackageSpec`] and binds the source
/// lock as explicit inputs.
#[derive(Debug, Clone)]
enum RecipeDeclarationEvaluator {
    Lua(LuaPackageEvaluator, Arc<[u8]>),
}

impl RecipeDeclarationEvaluator {
    fn registered(explicit_inputs: Arc<[u8]>) -> [Self; 1] {
        [Self::Lua(LuaPackageEvaluator::default(), explicit_inputs)]
    }
}

impl DeclarationEvaluator<RecipeDeclaration> for RecipeDeclarationEvaluator {
    type Identity = EvaluationIdentity;
    type Error = PackageConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Lua(package, _) => {
                <LuaPackageEvaluator as DeclarationEvaluator<PackageSpec>>::language_spec(package)
            }
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Lua(package, _) => {
                <LuaPackageEvaluator as DeclarationEvaluator<PackageSpec>>::limits(package)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Lua(package, inputs) => Self::Lua(
                <LuaPackageEvaluator as DeclarationEvaluator<PackageSpec>>::with_source_root(
                    package,
                    source_root,
                ),
                inputs.clone(),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<RecipeDeclaration, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = match self {
            Self::Lua(package, inputs) => {
                <LuaPackageEvaluator as DeclarationInputEvaluator<PackageSpec>>::evaluate_with_inputs_within(
                    package, source, inputs, deadline,
                )?
            }
        };
        Ok(DeclarationEvaluation {
            value: RecipeDeclaration {
                source: source.text().to_owned(),
                package: evaluation.value,
            },
            identity: evaluation.identity,
        })
    }
}

fn map_recipe_load_error(
    error: LoadFixedRootDeclarationError<PackageConversionError>,
) -> Error {
    match error {
        LoadFixedRootDeclarationError::Read { source, .. }
        | LoadFixedRootDeclarationError::RetainSourceRoot { source, .. } => {
            Error::LoadRecipeSource(source)
        }
        LoadFixedRootDeclarationError::Evaluation { source, .. } => {
            Error::EvaluateRecipe(
                DeclarationEvaluationError::Evaluation(source),
            )
        }
        LoadFixedRootDeclarationError::Conversion { source, .. } => {
            Error::EvaluateRecipe(
                DeclarationEvaluationError::Conversion(source),
            )
        }
        error => Error::LoadRecipeDeclaration(Box::new(error)),
    }
}

pub fn resolve_path(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    let path = path.as_ref();

    let path = if path.is_dir() {
        let slot = RootDeclarationSlot::new(
            RECIPE_ROOT_BASENAME,
            RECIPE_ROOT_LOGICAL_NAME_LUA,
        )
        .expect("the canonical recipe declaration slot is valid");
        let languages = registered_recipe_languages();
        discover_recipe_root(path, &slot, &languages)?
            .unwrap_or_else(|| path.join(RECIPE_ROOT_LOGICAL_NAME_LUA))
    } else {
        path.to_path_buf()
    };

    // Ensure it's absolute & exists
    fs::canonicalize(&path).map_err(|_| Error::MissingRecipe(path))
}


fn registered_recipe_languages() -> RegisteredLanguages {
    let languages = RecipeDeclarationEvaluator::registered(Arc::from(Vec::new()))
        .iter()
        .map(|evaluator| {
            <RecipeDeclarationEvaluator as DeclarationEvaluator<RecipeDeclaration>>::language_spec(
                evaluator,
            )
            .clone()
        })
        .collect::<Vec<_>>();
    RegisteredLanguages::new(languages)
        .expect("the recipe languages register distinct extensions")
}

fn discover_recipe_root(
    directory: &Path,
    slot: &RootDeclarationSlot,
    languages: &RegisteredLanguages,
) -> Result<Option<PathBuf>, Error> {
    slot.discover(directory, languages)
        .map(|discovered| {
            discovered.map(|declaration| declaration.path().to_owned())
        })
        .map_err(|source| Error::DiscoverRecipeSlot {
            directory: directory.to_owned(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("recipe file does not exist: {0:?}")]
    MissingRecipe(PathBuf),
    #[error("recipe path {path:?} cannot identify a fixed declaration slot")]
    InvalidRecipeSlot {
        path: PathBuf,
        #[source]
        source: RootDeclarationSlotError,
    },
    #[error("discover recipe declaration in {directory:?}")]
    DiscoverRecipeSlot {
        directory: PathBuf,
        #[source]
        source: RootDeclarationDiscoveryError,
    },
    #[error("load recipe source")]
    LoadRecipeSource(#[source] declarative_config::Diagnostic),
    #[error("load recipe declaration")]
    LoadRecipeDeclaration(
        #[source]
        Box<LoadFixedRootDeclarationError<PackageConversionError>>,
    ),
    #[error("load source lock {path:?}")]
    LoadSourceLock {
        path: PathBuf,
        #[source]
        source: Box<generated_lock::ReadError>,
    },
    #[error("source lock {path:?} is not UTF-8")]
    SourceLockUtf8 {
        path: PathBuf,
        #[source]
        source: str::Utf8Error,
    },
    #[error("evaluate source lock {path:?}")]
    EvaluateSourceLock {
        path: PathBuf,
        #[source]
        source: Box<DeclarationEvaluationError<source_lock::ValidationError>>,
    },
    #[error("stale source lock {path:?}")]
    StaleSourceLock {
        path: PathBuf,
        #[source]
        source: Box<source_lock::ValidationError>,
    },
    #[error("evaluate recipe")]
    EvaluateRecipe(#[from] DeclarationEvaluationError<PackageConversionError>),
    #[error("remove authored recipe {path:?} after an authorized Lua migration")]
    SwitchRecipeAuthority {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests;
