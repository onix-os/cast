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
use gluon_config::EvaluationIdentity;
use stone_recipe::build_policy::{TargetEmulationSpec, TargetPolicySpec};
use stone_recipe::package::{
    BuilderSpec, GluonPackageEvaluator, HooksSpec, LuaPackageEvaluator, PackageConversionError,
    PackageSpec, PhasesSpec, ProfileSpec, RecipeMigrationDecision, authorize_recipe_migration,
};
use thiserror::Error;

use crate::{
    generated_lock,
    source_lock::{self, GluonSourceLockCodec, SOURCE_LOCK_FILE_NAME, SourceLock},
};

const RECIPE_ROOT_BASENAME: &str = "stone";
const RECIPE_ROOT_LOGICAL_NAME_V1: &str = "stone.glu";

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

    /// Authorize migrating this authored recipe to an operator-supplied Lua
    /// replacement recipe. The bridge migrates a recipe tree only when the
    /// operator provides its exact root and a Lua replacement whose normalized
    /// package value matches the authored recipe; a mismatch is rejected rather
    /// than silently converted. Only an authorized replacement proceeds to
    /// regenerate the source/build-lock pair.
    pub fn authorize_lua_replacement(&self, replacement: &Recipe) -> RecipeMigrationDecision {
        authorize_recipe_migration(&self.declaration, &replacement.declaration)
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
    // The recipe language is selected by the file's extension (`stone.glu` or
    // `stone.lua`); an unregistered extension is a hard missing-recipe error.
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
    let source_lock_codec = GluonSourceLockCodec::default();
    let (explicit_inputs, source_lock) = match source_lock_policy {
        SourceLockPolicy::Ignore => (Vec::new(), None),
        SourceLockPolicy::RequireCurrent => match generated_lock::read(
            &lock_path,
            <GluonSourceLockCodec as DeclarationEvaluator<SourceLock>>::limits(
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

/// One registered recipe declaration language (`stone.glu` or `stone.lua`),
/// selected by the recipe file's extension. Both engines reach the same
/// [`PackageSpec`] and bind the source lock as explicit inputs.
#[derive(Debug, Clone)]
enum RecipeDeclarationEvaluator {
    Gluon(GluonPackageEvaluator, Arc<[u8]>),
    Lua(LuaPackageEvaluator, Arc<[u8]>),
}

impl RecipeDeclarationEvaluator {
    fn registered(explicit_inputs: Arc<[u8]>) -> [Self; 2] {
        [
            Self::Gluon(GluonPackageEvaluator::default(), explicit_inputs.clone()),
            Self::Lua(LuaPackageEvaluator::default(), explicit_inputs),
        ]
    }
}

impl DeclarationEvaluator<RecipeDeclaration> for RecipeDeclarationEvaluator {
    type Identity = EvaluationIdentity;
    type Error = PackageConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Gluon(package, _) => {
                <GluonPackageEvaluator as DeclarationEvaluator<PackageSpec>>::language_spec(package)
            }
            Self::Lua(package, _) => {
                <LuaPackageEvaluator as DeclarationEvaluator<PackageSpec>>::language_spec(package)
            }
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(package, _) => {
                <GluonPackageEvaluator as DeclarationEvaluator<PackageSpec>>::limits(package)
            }
            Self::Lua(package, _) => {
                <LuaPackageEvaluator as DeclarationEvaluator<PackageSpec>>::limits(package)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(package, inputs) => Self::Gluon(
                <GluonPackageEvaluator as DeclarationEvaluator<PackageSpec>>::with_source_root(
                    package,
                    source_root,
                ),
                inputs.clone(),
            ),
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
            Self::Gluon(package, inputs) => {
                <GluonPackageEvaluator as DeclarationInputEvaluator<PackageSpec>>::evaluate_with_inputs_within(
                    package, source, inputs, deadline,
                )?
            }
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
            RECIPE_ROOT_LOGICAL_NAME_V1,
        )
        .expect("the canonical recipe declaration slot is valid");
        let languages = registered_recipe_languages();
        discover_recipe_root(path, &slot, &languages)?
            .unwrap_or_else(|| path.join(RECIPE_ROOT_LOGICAL_NAME_V1))
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
    #[error("load Gluon recipe source")]
    LoadRecipeSource(#[source] gluon_config::Diagnostic),
    #[error("load recipe declaration")]
    LoadRecipeDeclaration(
        #[source]
        Box<LoadFixedRootDeclarationError<PackageConversionError>>,
    ),
    #[error("load Gluon source lock {path:?}")]
    LoadSourceLock {
        path: PathBuf,
        #[source]
        source: Box<generated_lock::ReadError>,
    },
    #[error("Gluon source lock {path:?} is not UTF-8")]
    SourceLockUtf8 {
        path: PathBuf,
        #[source]
        source: str::Utf8Error,
    },
    #[error("evaluate Gluon source lock {path:?}")]
    EvaluateSourceLock {
        path: PathBuf,
        #[source]
        source: Box<DeclarationEvaluationError<source_lock::ValidationError>>,
    },
    #[error("stale Gluon source lock {path:?}")]
    StaleSourceLock {
        path: PathBuf,
        #[source]
        source: Box<source_lock::ValidationError>,
    },
    #[error("evaluate Gluon recipe")]
    EvaluateRecipe(#[from] DeclarationEvaluationError<PackageConversionError>),
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationCodec, EngineId, LanguageId};
    use stone_recipe::package::{BuilderSpec, HooksSpec, ProfileSpec};

    use super::*;

    const SOURCE_SPEC: &str = r#"{
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}"#;

    const ARCHIVE_URL: &str = "https://example.com/source.tar.xz";
    const ARCHIVE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GIT_URL: &str = "https://example.com/source.git";
    const GIT_REF: &str = "main";
    const FULL_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const MATERIALIZATION_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn canonical_source_lock(lock: &SourceLock) -> String {
        GluonSourceLockCodec::default().encode(lock).unwrap()
    }

    fn gluon_recipe(source: &str) -> String {
        format!("let cast = import! cast.package.v3\ncast.mk_package (cast.meta {source})")
    }

    fn synthetic_language(name: &str, extension: &str) -> LanguageSpec {
        LanguageSpec::new(
            LanguageId::new(name).unwrap(),
            EngineId::new(format!("{name}-engine"), "1").unwrap(),
            extension,
            "declaration-v1",
            format!("# generated by {name}\n"),
        )
        .unwrap()
    }

    fn minimal_recipe() -> Recipe {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        Recipe::load(root.path()).unwrap()
    }

    fn recipe_from(source: &str) -> Recipe {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(source)).unwrap();
        Recipe::load(root.path()).unwrap()
    }

    #[test]
    fn a_recipe_authorizes_only_an_equivalent_replacement() {
        let authored = recipe_from(SOURCE_SPEC);
        let equivalent = recipe_from(SOURCE_SPEC);
        assert_eq!(
            authored.authorize_lua_replacement(&equivalent),
            RecipeMigrationDecision::Authorized
        );

        let divergent = recipe_from(&SOURCE_SPEC.replace("example", "renamed"));
        assert_eq!(
            authored.authorize_lua_replacement(&divergent),
            RecipeMigrationDecision::Rejected
        );
    }

    #[test]
    fn a_stone_lua_recipe_is_discovered_by_extension() {
        // The recipe loader now registers both languages; a directory holding a
        // `stone.lua` resolves to it (the `.lua` recipe dispatch), while an
        // unregistered extension is not discovered.
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.lua"), "return {}\n").unwrap();
        let resolved = resolve_path(root.path()).unwrap();
        assert!(resolved.ends_with("stone.lua"));

        let unknown = tempfile::tempdir().unwrap();
        fs::write(unknown.path().join("stone.toml"), "").unwrap();
        assert!(matches!(resolve_path(unknown.path()), Err(Error::MissingRecipe(_))));
    }

    fn target(name: &str) -> TargetPolicySpec {
        crate::BuildPolicy::repository_for_tests().target(name).unwrap().clone()
    }

    fn profile(name: &str) -> ProfileSpec {
        ProfileSpec {
            name: name.to_owned(),
            builder: BuilderSpec::default(),
            hooks: HooksSpec::default(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        }
    }

    fn gluon_recipe_with_upstreams() -> String {
        format!(
            r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {SOURCE_SPEC})
{{
    sources = [
        cast.source.archive "{ARCHIVE_URL}" "{ARCHIVE_HASH}",
        cast.source.git "{GIT_URL}" "{GIT_REF}",
    ],
    .. base
}}"#
        )
    }

    #[test]
    fn documented_recipe_examples_remain_loadable() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");

        let minimal = Recipe::load(examples.join("stone.glu")).unwrap();
        assert_eq!(minimal.declaration.meta.pname, "hello");

        let composed = Recipe::load(examples.join("composed-stone.glu")).unwrap();
        assert_eq!(composed.declaration.meta.pname, "composed-hello");
        assert_eq!(
            composed.declaration.outputs[0].summary.as_deref(),
            Some("Recipe composed with an imported policy")
        );
    }

    #[test]
    fn empty_architecture_list_supports_all_native_policy_targets_and_gates_emul32() {
        let mut recipe = minimal_recipe();
        let x86_64 = target("x86_64");
        let x86_64_v3x = target("x86_64-v3x");
        let emul32 = target("emul32/x86_64");

        assert!(recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.emul32 = true;
        assert!(recipe.supports_target(&emul32));
    }

    #[test]
    fn declared_architectures_match_exact_policy_names_or_typed_classes_only() {
        let mut recipe = minimal_recipe();
        let x86_64 = target("x86_64");
        let x86_64_v3x = target("x86_64-v3x");
        let emul32 = target("emul32/x86_64");

        recipe.declaration.architectures = vec!["x86_64".to_owned()];
        assert!(recipe.supports_target(&x86_64));
        assert!(!recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["native".to_owned()];
        assert!(recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["emul32".to_owned()];
        assert!(!recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["emul32/x86_64".to_owned()];
        assert!(recipe.supports_target(&emul32));
    }

    #[test]
    fn target_profiles_prefer_exact_policy_name_then_typed_emul32_fallback() {
        let mut recipe = minimal_recipe();
        let target = target("emul32/x86_64");
        recipe.declaration.profiles = vec![profile("emul32"), profile("emul32/x86_64")];

        assert_eq!(recipe.build_target_profile_key(&target), Some("emul32/x86_64"));
        assert_eq!(recipe.build_target_profile(&target).unwrap().name, "emul32/x86_64");

        recipe.declaration.profiles.pop();
        assert_eq!(recipe.build_target_profile_key(&target), Some("emul32"));
        assert_eq!(recipe.build_target_profile(&target).unwrap().name, "emul32");
    }

    fn matching_source_lock() -> SourceLock {
        SourceLock::new(vec![
            source_lock::SourceResolution::Archive(source_lock::ArchiveResolution {
                order: 0,
                url: ARCHIVE_URL.to_owned(),
                sha256: ARCHIVE_HASH.to_owned(),
            }),
            source_lock::SourceResolution::Git(source_lock::GitResolution {
                order: 1,
                url: GIT_URL.to_owned(),
                requested_ref: GIT_REF.to_owned(),
                commit: FULL_COMMIT.to_owned(),
                materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
            }),
        ])
    }

    #[test]
    fn recipe_directory_resolves_registered_gluon_root() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.yaml"), "ignored").unwrap();

        let missing = resolve_path(root.path()).unwrap_err();
        assert!(matches!(missing, Error::MissingRecipe(path) if path.ends_with("stone.glu")));

        let gluon = root.path().join("stone.glu");
        fs::write(&gluon, "{}").unwrap();
        assert_eq!(resolve_path(root.path()).unwrap(), gluon.canonicalize().unwrap());
    }

    #[test]
    fn recipe_root_discovery_dispatches_registered_extensions_and_rejects_collisions() {
        let root = tempfile::tempdir().unwrap();
        let slot = RootDeclarationSlot::new(
            RECIPE_ROOT_BASENAME,
            RECIPE_ROOT_LOGICAL_NAME_V1,
        )
        .unwrap();
        let languages = RegisteredLanguages::new([
            synthetic_language("alpha", "alpha"),
            synthetic_language("beta", "beta"),
        ])
        .unwrap();
        let alpha = root.path().join("stone.alpha");
        fs::write(&alpha, "alpha").unwrap();

        assert_eq!(
            discover_recipe_root(root.path(), &slot, &languages).unwrap(),
            Some(alpha.clone())
        );

        let beta = root.path().join("stone.beta");
        fs::write(&beta, "beta").unwrap();
        let error = discover_recipe_root(root.path(), &slot, &languages).unwrap_err();
        let Error::DiscoverRecipeSlot { directory, source } = error else {
            panic!("unexpected error: {error}");
        };
        assert_eq!(directory, root.path());
        assert_eq!(source.collision_paths(), Some([alpha, beta].as_slice()));
    }

    #[cfg(unix)]
    #[test]
    fn recipe_root_discovery_rejects_a_symlinked_registered_candidate() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.glu");
        let recipe = root.path().join(RECIPE_ROOT_LOGICAL_NAME_V1);
        fs::write(&target, gluon_recipe(SOURCE_SPEC)).unwrap();
        symlink(&target, &recipe).unwrap();

        let error = resolve_path(root.path()).unwrap_err();
        let Error::DiscoverRecipeSlot { directory, source } = error else {
            panic!("unexpected error: {error}");
        };
        assert_eq!(directory, root.path());
        assert!(matches!(
            source,
            RootDeclarationDiscoveryError::NotRegular { path } if path == recipe
        ));
    }

    #[test]
    fn explicit_gluon_file_loads_and_records_provenance() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("custom.glu");
        fs::write(&path, gluon_recipe(SOURCE_SPEC)).unwrap();

        let recipe = Recipe::load(&path).unwrap();

        assert_eq!(recipe.path, path.canonicalize().unwrap());
        assert_eq!(recipe.declaration.meta.pname, "example");
        assert!(recipe.source_lock.is_none());
        let fingerprint = recipe.fingerprint;
        assert_eq!(fingerprint.root_source_sha256.len(), 64);
        assert!(
            fingerprint
                .modules
                .iter()
                .any(|module| module.logical_name == "cast.package.v3")
        );
    }

    #[test]
    fn explicit_unknown_extension_never_selects_a_sibling_gluon_recipe() {
        let root = tempfile::tempdir().unwrap();
        let gluon = root.path().join("custom.glu");
        let unknown = root.path().join("custom.yaml");
        fs::write(&gluon, gluon_recipe(SOURCE_SPEC)).unwrap();
        fs::write(&unknown, gluon_recipe(SOURCE_SPEC)).unwrap();

        let error = Recipe::load(&unknown).unwrap_err();

        assert!(matches!(error, Error::MissingRecipe(path) if path == unknown.canonicalize().unwrap()));
    }

    #[test]
    fn directory_gluon_loads_contained_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.glu"), SOURCE_SPEC).unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let cast = import! cast.package.v3
let source = import! "source.glu"
cast.mk_package (cast.meta source)
"#,
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();
        let fingerprint = recipe.fingerprint;

        assert_eq!(recipe.path, root.path().join("stone.glu").canonicalize().unwrap());
        assert_eq!(recipe.declaration.meta.version, "1.2.3");
        assert_eq!(fingerprint.root_logical_name, RECIPE_ROOT_LOGICAL_NAME_V1);
        let mut modules = fingerprint
            .modules
            .iter()
            .map(|module| module.logical_name.as_str())
            .collect::<Vec<_>>();
        // v2 identity orders modules by canonical graph identity; assert
        // membership independent of that ordering.
        modules.sort_unstable();
        assert_eq!(
            modules,
            [
                "cast.package.v3",
                "source.glu",
                "std.array.prim",
                "std.string.prim",
                "std.types",
            ]
        );
    }

    #[test]
    fn invalid_gluon_preserves_source_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let cast = import! cast.package.v3
cast.mk_package (cast.meta {
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = 42,
    license = ["MPL-2.0"],
})
"#,
        )
        .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::EvaluateRecipe(DeclarationEvaluationError::Evaluation(diagnostic)) = error else {
            panic!("unexpected error: {error}");
        };

        assert_eq!(diagnostic.category, gluon_config::DiagnosticCategory::Type);
        assert_eq!(diagnostic.source_name.as_deref(), Some("stone.glu"));
        assert!(diagnostic.span.is_some());
    }

    #[test]
    fn legacy_recipe_shape_is_not_an_implicit_fallback() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            format!("let source = {SOURCE_SPEC}\n{{ source }}"),
        )
        .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::EvaluateRecipe(DeclarationEvaluationError::Evaluation(_))
        ));
    }

    #[test]
    fn valid_source_lock_is_decoded_and_retained() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        fs::write(
            root.path().join(SOURCE_LOCK_FILE_NAME),
            canonical_source_lock(&matching_source_lock()),
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();

        assert_eq!(recipe.source_lock, Some(matching_source_lock()));
    }

    #[test]
    fn oversized_and_symlinked_source_locks_are_rejected_structurally() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        let limit = Limits::default().max_source_bytes;
        fs::File::create(&lock_path)
            .unwrap()
            .set_len(u64::try_from(limit).unwrap() + 1)
            .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::LoadSourceLock { source, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert!(matches!(
            *source,
            generated_lock::ReadError::TooLarge { limit: found, .. } if found == limit
        ));

        fs::remove_file(&lock_path).unwrap();
        let target = root.path().join("lock-target");
        fs::write(&target, canonical_source_lock(&matching_source_lock())).unwrap();
        symlink(&target, &lock_path).unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::LoadSourceLock { source, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert!(matches!(
            *source,
            generated_lock::ReadError::NotRegular {
                kind: generated_lock::FileKind::Symlink,
                ..
            }
        ));
    }

    #[test]
    fn malformed_schema_and_commit_lock_errors_include_the_lock_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);

        let mut wrong_schema = matching_source_lock();
        wrong_schema.schema_version = 3;
        let mut short_commit = matching_source_lock();
        let source_lock::SourceResolution::Git(git) = &mut short_commit.sources[1] else {
            unreachable!();
        };
        git.commit = "abc123d".to_owned();

        let cases = [
            ("{".to_owned(), "Unexpected end of file"),
            (canonical_source_lock(&wrong_schema), "unsupported schema"),
            (
                canonical_source_lock(&short_commit),
                "exactly 40 lowercase hexadecimal",
            ),
        ];

        for (contents, expected) in cases {
            fs::write(&lock_path, contents).unwrap();
            let error = Recipe::load(root.path()).unwrap_err();
            let Error::EvaluateSourceLock { path, source } = error else {
                panic!("unexpected error: {error}");
            };
            assert_eq!(path, lock_path);
            assert!(source.to_string().contains(expected), "{source}");
        }
    }

    #[test]
    fn authored_load_ignores_generated_lock_bytes_without_mutating_them() {
        let root = tempfile::tempdir().unwrap();
        let recipe_path = root.path().join("stone.glu");
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        fs::write(&recipe_path, gluon_recipe(SOURCE_SPEC)).unwrap();
        fs::write(&lock_path, "not valid Gluon").unwrap();

        let recipe = Recipe::load_authored(root.path()).unwrap();

        assert_eq!(recipe.declaration.meta.pname, "example");
        assert!(recipe.source_lock.is_none());
        assert_eq!(fs::read_to_string(lock_path).unwrap(), "not valid Gluon");
    }

    #[test]
    fn stale_source_lock_rejects_count_kind_url_hash_and_requested_ref() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);

        let mut count = matching_source_lock();
        count.sources.pop();

        let mut kind = matching_source_lock();
        kind.sources[0] = source_lock::SourceResolution::Git(source_lock::GitResolution {
            order: 0,
            url: ARCHIVE_URL.to_owned(),
            requested_ref: "archive".to_owned(),
            commit: FULL_COMMIT.to_owned(),
            materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
        });

        let mut url = matching_source_lock();
        let source_lock::SourceResolution::Archive(archive) = &mut url.sources[0] else {
            unreachable!();
        };
        archive.url = "https://example.com/changed.tar.xz".to_owned();

        let mut hash = matching_source_lock();
        let source_lock::SourceResolution::Archive(archive) = &mut hash.sources[0] else {
            unreachable!();
        };
        archive.sha256 = "b".repeat(64);

        let mut requested_ref = matching_source_lock();
        let source_lock::SourceResolution::Git(git) = &mut requested_ref.sources[1] else {
            unreachable!();
        };
        git.requested_ref = "v2".to_owned();

        for (lock, expected) in [
            (count, "stale source count"),
            (kind, "stale source kind"),
            (url, "sources[0].url"),
            (hash, "sources[0].sha256"),
            (requested_ref, "sources[1].requested_ref"),
        ] {
            fs::write(&lock_path, canonical_source_lock(&lock)).unwrap();
            let error = Recipe::load(root.path()).unwrap_err();
            let Error::StaleSourceLock { path, source } = error else {
                panic!("unexpected error: {error}");
            };
            assert_eq!(path, lock_path);
            assert!(source.to_string().contains(expected), "{source}");
        }
    }

    #[test]
    fn recipe_and_lock_fingerprints_are_deterministic() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        let lock = canonical_source_lock(&SourceLock::default());
        fs::write(root.path().join(SOURCE_LOCK_FILE_NAME), &lock).unwrap();

        let first = Recipe::load(root.path()).unwrap();
        let repeated = Recipe::load(root.path()).unwrap();

        assert_eq!(first.declaration, repeated.declaration);
        assert_eq!(first.source, repeated.source);
        assert_eq!(first.fingerprint, repeated.fingerprint);

        fs::write(
            root.path().join(SOURCE_LOCK_FILE_NAME),
            format!("{lock}// semantically inert fingerprint change\n"),
        )
        .unwrap();
        let changed = Recipe::load(root.path()).unwrap();
        let first_fingerprint = &first.fingerprint;
        let changed_fingerprint = &changed.fingerprint;

        assert_ne!(first_fingerprint.sha256, changed_fingerprint.sha256);
        assert_ne!(
            first_fingerprint.explicit_inputs_sha256,
            changed_fingerprint.explicit_inputs_sha256
        );
    }

    #[test]
    fn explicit_build_timestamp_is_deterministic_and_ambient_free() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        let timestamp = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let first = Recipe::load_at(root.path(), timestamp).unwrap();
        let repeated = Recipe::load_at(root.path(), timestamp).unwrap();
        let defaulted = Recipe::load(root.path()).unwrap();

        assert_eq!(first.build_time, repeated.build_time);
        assert_eq!(first.build_time.timestamp(), 1_700_000_000);
        assert_eq!(defaulted.build_time.timestamp(), 0);
    }
}
