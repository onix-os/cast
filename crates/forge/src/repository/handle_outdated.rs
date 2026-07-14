// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;

use fs_err as fs;
use gluon_config::Evaluator;
use tui::Styled;
use url::Url;

use crate::{
    Repository,
    repository::{self, manager},
};

#[derive(Debug, Clone)]
pub struct OutdatedRepoIndexUri {
    pub repository: repository::Cached,
    pub legacy_index_uri: Url,
    pub compatible_root_index_source: repository::RootIndexSource,
}

pub fn handle_outdated_index_uris(source: &manager::Source, outdated_repos: Vec<OutdatedRepoIndexUri>) {
    let count = outdated_repos.len();

    let repo_plural = if count == 1 { "repo" } else { "repos" };
    let require_plural = if count == 1 { "requires" } else { "require" };

    match source {
        manager::Source::ConfigManager(config_manager) => {
            println!("{count} {repo_plural} {require_plural} an updated Gluon repository source");

            let loaded_config = match config_manager.load_gluon(&Evaluator::default(), &repository::RepositoryCodec) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!("Failed to load Gluon repository configuration: {error:#}");
                    return;
                }
            }
            .into_iter()
            .map(|item| {
                let no_ext = item.path.with_extension("");
                (no_ext, item)
            })
            .collect::<HashMap<_, _>>();

            let updates = outdated_repos
                .into_iter()
                .fold(HashMap::<_, Vec<_>>::new(), |mut acc, repo| {
                    let Some(path) = repo.repository.config_path.as_ref() else {
                        // Every repository loaded through the configuration manager has a source path.
                        return acc;
                    };
                    acc.entry(path.with_extension("")).or_default().push(repo);
                    acc
                });

            for (no_ext, updates) in updates {
                let Some(current_config) = loaded_config.get(&no_ext) else {
                    // Unreachable, everything returned originated from
                    // stuff loaded via config manager
                    continue;
                };

                let mut updated_map = current_config.value.clone();
                for update in &updates {
                    let old_repo = &update.repository.repository;
                    updated_map.add(
                        update.repository.id.clone(),
                        Repository {
                            source: repository::Source::RootIndex(update.compatible_root_index_source.clone()),
                            ..old_repo.clone()
                        },
                    );
                }
                let old_content = fs::read_to_string(&current_config.path).unwrap_or_default();

                let gluon_path = match config_manager.save_gluon(
                    &current_config.logical_name,
                    &updated_map,
                    &repository::RepositoryCodec,
                ) {
                    Ok(path) => path,
                    Err(config::SaveGluonError::AuthoredFragment { path }) => {
                        println!("\nCast left the authored source at {path:?} unchanged.");
                        for update in &updates {
                            print_repository_suggestion(update);
                        }
                        continue;
                    }
                    Err(error) => {
                        eprintln!("Failed to save updated Gluon repository configuration: {error:#}");
                        continue;
                    }
                };

                let new_content = fs::read_to_string(&gluon_path).unwrap_or_default();

                println!("\nUpdate applied to {gluon_path:?}");

                println!("\n```diff");
                print_diff(
                    &old_content,
                    &new_content,
                    Some((
                        current_config.path.as_os_str().to_str().unwrap_or_default(),
                        gluon_path.as_os_str().to_str().unwrap_or_default(),
                    )),
                );
                println!("```");
            }
        }
        manager::Source::SystemModel { system_model, .. } => {
            println!("{count} system-intent {repo_plural} {require_plural} an authored Gluon source update");

            let path = system_model.path().to_owned();

            println!("Cast left the authored source at {path:?} unchanged.");
            for outdated in outdated_repos {
                print_repository_suggestion(&outdated);
            }
        }
        manager::Source::Explicit { .. } => {
            println!("{count} {repo_plural} {require_plural} a configuration update to the new repository format");

            for repo in outdated_repos {
                print_repository_suggestion(&repo);
            }
        }
    }
}

fn print_repository_suggestion(outdated: &OutdatedRepoIndexUri) {
    let id = &outdated.repository.id;
    let source = &outdated.compatible_root_index_source;

    println!("\nSuggested repository source change for {}:", id.to_string().bold());
    println!("  current.direct_index = {:?}", outdated.legacy_index_uri.as_str());
    println!("  replacement.kind = root-index");
    println!("  replacement.base_uri = {:?}", source.base_uri.as_str());
    println!("  replacement.channel = {:?}", source.channel.as_ref());
    println!("  replacement.version = {:?}", source.version.to_string());
    println!("  replacement.arch = {:?}", source.arch);
}

fn print_diff(a: &str, b: &str, header: Option<(&str, &str)>) {
    let diff = similar::TextDiff::from_lines(a, b);

    let mut unified = diff.unified_diff();

    if let Some((file_a, file_b)) = header {
        unified.header(file_a, file_b);
    }

    for line in unified.to_string().lines() {
        let colored = if line.starts_with('-') {
            line.red()
        } else if line.starts_with('+') {
            line.green()
        } else {
            line.dim()
        };

        println!("{colored}");
    }
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;
    use crate::{db, system_model};

    #[test]
    fn system_intent_suggestion_never_mutates_authored_source() {
        let temporary = tempfile::tempdir().unwrap();
        let intent_path = system_model::intent_path(temporary.path());
        fs::create_dir_all(intent_path.parent().unwrap()).unwrap();
        let legacy_uri = Url::parse("https://cdn.aerynos.dev/stream/volatile/x86_64/stone.index").unwrap();
        let authored = format!(
            r#"// Keep this comment and expression byte-for-byte.
let cast = import! cast.system.v1
{{
    repositories = [
        cast.repository.direct "volatile" "{legacy_uri}",
    ],
    .. cast.system
}}
"#
        );
        fs::write(&intent_path, &authored).unwrap();
        let loaded = system_model::load(&intent_path).unwrap().unwrap();
        let source = manager::Source::SystemModel {
            identifier: "system-intent-test".to_owned(),
            system_model: loaded,
        };
        let repository = Repository {
            description: "volatile".to_owned(),
            source: repository::Source::DirectIndex(legacy_uri.clone()),
            priority: repository::Priority::new(0),
            active: true,
        };
        let outdated = OutdatedRepoIndexUri {
            repository: repository::Cached::new(
                repository::Id::new("volatile"),
                repository,
                db::meta::Database::new(":memory:").unwrap(),
                None,
                temporary.path().join("repo-cache"),
            ),
            legacy_index_uri: legacy_uri,
            compatible_root_index_source: repository::RootIndexSource {
                base_uri: Url::parse("https://cdn.aerynos.dev").unwrap(),
                channel: repository::DEFAULT_CHANNEL.try_into().unwrap(),
                version: "stream/volatile".parse().unwrap(),
                arch: repository::DEFAULT_ARCH.to_owned(),
            },
        };

        handle_outdated_index_uris(&source, vec![outdated]);

        assert_eq!(fs::read_to_string(&intent_path).unwrap(), authored);
        assert!(!system_model::snapshot_path(temporary.path()).exists());
    }
}
