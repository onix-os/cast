// SPDX-FileCopyrightText: 2025 AerynOS Developers

use std::{
    collections::HashSet,
    io::{self, Read},
    path::{Path, PathBuf},
};

use fs_err as fs;
use rapidfuzz::distance::levenshtein;
use rayon::prelude::*;
use thiserror::Error;
use tui::Styled;

use super::File;

const MAX_SPDX_FILES: usize = 2_048;
const MAX_LICENSE_CANDIDATES: usize = 128;
const MAX_LICENSE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LICENSE_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SPDX_FILE_BYTES: u64 = 512 * 1024;
const MAX_SPDX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NORMALIZED_CHARS: usize = 64 * 1024;
const MAX_COMPARISONS: usize = 8_192;
const MAX_PAIR_WORD_STEPS: u64 = 32 * 1024 * 1024;
const MAX_AGGREGATE_WORD_STEPS: u64 = 256 * 1024 * 1024;

struct SpdxIndex {
    names: HashSet<PathBuf>,
    paths: Vec<PathBuf>,
    prefixes: Vec<String>,
}

struct LoadedText {
    path: PathBuf,
    normalized: String,
    chars: usize,
}

struct Comparison<'a> {
    source: &'a LoadedText,
    canonical: &'a LoadedText,
    canonical_chars: usize,
}

fn collect_spdx_licenses(dir: &Path) -> Result<SpdxIndex, Error> {
    let mut names = HashSet::new();
    let mut paths = Vec::new();
    let mut prefixes = HashSet::new();

    for (index, entry) in fs::read_dir(dir)?.enumerate() {
        if index >= MAX_SPDX_FILES {
            return Err(Error::Limit {
                resource: "SPDX license files",
                limit: MAX_SPDX_FILES,
            });
        }
        let entry = entry?;
        let name = PathBuf::from(entry.file_name());
        if name.to_str().unwrap_or_default().contains("deprecated_") {
            continue;
        }
        if let Some((prefix, _)) = name.to_string_lossy().split_once('-') {
            prefixes.insert(prefix.to_lowercase());
        }
        names.insert(name);
        paths.push(entry.path());
    }

    Ok(SpdxIndex {
        names,
        paths,
        prefixes: prefixes.into_iter().collect(),
    })
}

fn collect_source_licenses(files: &[File], spdx: &SpdxIndex) -> Result<(Vec<PathBuf>, Vec<String>), Error> {
    let mut candidates = Vec::new();
    let mut reuse_matches = HashSet::new();

    for file in files.iter().filter(|file| file.depth() == 0) {
        let name = PathBuf::from(file.file_name());
        if spdx.names.contains(&name) {
            reuse_matches.insert(name.with_extension("").to_string_lossy().into_owned());
        }
        let lower = file.file_name().to_lowercase();
        let looks_like_license = ["copying", "license"].iter().any(|pattern| lower.contains(pattern))
            || spdx.prefixes.iter().any(|prefix| lower.starts_with(prefix));
        if looks_like_license && !candidates.contains(&file.path) {
            if candidates.len() >= MAX_LICENSE_CANDIDATES {
                return Err(Error::Limit {
                    resource: "candidate license files",
                    limit: MAX_LICENSE_CANDIDATES,
                });
            }
            candidates.push(file.path.clone());
        }
    }

    Ok((candidates, reuse_matches.into_iter().collect()))
}

pub fn match_licences(files: &[File], spdx_dir: &Path) -> Result<Vec<String>, Error> {
    let spdx = collect_spdx_licenses(spdx_dir)?;
    let (licenses, reuse_matches) = collect_source_licenses(files, &spdx)?;
    if !reuse_matches.is_empty() {
        return Ok(reuse_matches);
    }
    if licenses.is_empty() {
        println!("{} | Failed to find any licenses", "Warning".yellow());
        return Ok(Vec::new());
    }

    let source_texts = load_texts(&licenses, MAX_LICENSE_FILE_BYTES, MAX_LICENSE_TOTAL_BYTES, "license")?;
    let spdx_texts = load_texts(&spdx.paths, MAX_SPDX_FILE_BYTES, MAX_SPDX_TOTAL_BYTES, "SPDX license")?;
    let confidence = 0.9;
    let comparisons = comparison_plan(&source_texts, &spdx_texts)?;
    let matches = comparisons
        .par_iter()
        .filter_map(|comparison| {
            let scorer = levenshtein::BatchComparator::new(comparison.source.normalized.chars());
            let truncated = comparison
                .canonical
                .normalized
                .chars()
                .take(comparison.canonical_chars)
                .collect::<String>();
            let similarity = scorer.normalized_similarity_with_args(
                truncated.chars(),
                &levenshtein::Args::default().score_cutoff(confidence),
            )?;
            (similarity >= confidence).then(|| {
                let path = &comparison.canonical.path;
                println!(
                    "{} | Matched against {:?} (confidence {:.2}%)",
                    "License".green(),
                    path.with_extension("").file_name().unwrap_or_default(),
                    similarity * 100.0
                );
                path.with_extension("")
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect::<Vec<_>>();

    if matches.is_empty() {
        println!("{} | Failed to match against any licenses", "Warning".yellow());
    }
    Ok(matches.into_iter().collect::<HashSet<_>>().into_iter().collect())
}

fn comparison_plan<'a>(sources: &'a [LoadedText], canonicals: &'a [LoadedText]) -> Result<Vec<Comparison<'a>>, Error> {
    let mut comparisons = Vec::new();
    let mut aggregate_word_steps = 0_u64;
    for source in sources {
        for canonical in canonicals {
            if source.chars == 0 || canonical.chars == 0 {
                continue;
            }
            // A shorter string cannot reach 90% normalized similarity when
            // the unavoidable length difference already exceeds 10%.
            if canonical.chars.saturating_mul(10) < source.chars.saturating_mul(9) {
                continue;
            }
            let canonical_chars = canonical.chars.min(source.chars.saturating_mul(105) / 100);
            let canonical_words = u64::try_from(canonical_chars)
                .map_err(|_| Error::ArithmeticOverflow)?
                .checked_add(63)
                .ok_or(Error::ArithmeticOverflow)?
                / 64;
            let pair_word_steps = u64::try_from(source.chars)
                .map_err(|_| Error::ArithmeticOverflow)?
                .checked_mul(canonical_words)
                .ok_or(Error::ArithmeticOverflow)?;
            aggregate_word_steps = aggregate_word_steps
                .checked_add(pair_word_steps)
                .ok_or(Error::ArithmeticOverflow)?;
            require_comparison_budget(comparisons.len() + 1, pair_word_steps, aggregate_word_steps)?;
            comparisons.push(Comparison {
                source,
                canonical,
                canonical_chars,
            });
        }
    }
    Ok(comparisons)
}

fn require_comparison_budget(comparisons: usize, pair_word_steps: u64, aggregate_word_steps: u64) -> Result<(), Error> {
    if comparisons > MAX_COMPARISONS {
        return Err(Error::Limit {
            resource: "license comparisons",
            limit: MAX_COMPARISONS,
        });
    }
    if pair_word_steps > MAX_PAIR_WORD_STEPS {
        return Err(Error::ComputationLimit {
            resource: "one normalized license comparison",
            limit: MAX_PAIR_WORD_STEPS,
        });
    }
    if aggregate_word_steps > MAX_AGGREGATE_WORD_STEPS {
        return Err(Error::ComputationLimit {
            resource: "aggregate normalized license comparisons",
            limit: MAX_AGGREGATE_WORD_STEPS,
        });
    }
    Ok(())
}

fn load_texts(
    paths: &[PathBuf],
    one_limit: u64,
    total_limit: u64,
    resource: &'static str,
) -> Result<Vec<LoadedText>, Error> {
    let mut total = 0_u64;
    let mut texts = Vec::with_capacity(paths.len());
    for path in paths {
        let text = read_bounded_text(path, one_limit, resource)?;
        total = total.checked_add(text.len() as u64).ok_or(Error::ArithmeticOverflow)?;
        if total > total_limit {
            return Err(Error::ByteLimit {
                resource,
                limit: total_limit,
            });
        }
        let normalized = normalize_bounded(&text, resource)?;
        let chars = normalized.chars().count();
        texts.push(LoadedText {
            path: path.clone(),
            normalized,
            chars,
        });
    }
    Ok(texts)
}

fn read_bounded_text(path: &Path, limit: u64, resource: &'static str) -> Result<String, Error> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::ByteLimit { resource, limit });
    }
    String::from_utf8(bytes).map_err(Error::Utf8)
}

fn normalize_bounded(content: &str, resource: &'static str) -> Result<String, Error> {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > MAX_NORMALIZED_CHARS {
        return Err(Error::CharacterLimit {
            resource,
            limit: MAX_NORMALIZED_CHARS,
        });
    }
    Ok(normalized)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{resource} exceed item limit of {limit}")]
    Limit { resource: &'static str, limit: usize },
    #[error("{resource} exceed byte limit of {limit}")]
    ByteLimit { resource: &'static str, limit: u64 },
    #[error("{resource} exceed normalized character limit of {limit}")]
    CharacterLimit { resource: &'static str, limit: usize },
    #[error("{resource} exceed conservative computation limit of {limit} word steps")]
    ComputationLimit { resource: &'static str, limit: u64 },
    #[error("license analysis arithmetic overflow")]
    ArithmeticOverflow,
    #[error("license text is not UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
    #[error("license analysis I/O")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_accepts_n_and_rejects_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LICENSE");
        fs::write(&path, b"1234").unwrap();
        assert_eq!(read_bounded_text(&path, 4, "test").unwrap(), "1234");
        assert!(matches!(
            read_bounded_text(&path, 3, "test"),
            Err(Error::ByteLimit { limit: 3, .. })
        ));
    }

    #[test]
    fn normalized_text_and_comparison_work_accept_n_and_reject_n_plus_one() {
        let exact = "a".repeat(MAX_NORMALIZED_CHARS);
        assert_eq!(normalize_bounded(&exact, "test").unwrap().len(), MAX_NORMALIZED_CHARS);
        assert!(matches!(
            normalize_bounded(&(exact + "a"), "test"),
            Err(Error::CharacterLimit { .. })
        ));

        assert!(require_comparison_budget(MAX_COMPARISONS, MAX_PAIR_WORD_STEPS, MAX_AGGREGATE_WORD_STEPS,).is_ok());
        assert!(matches!(
            require_comparison_budget(MAX_COMPARISONS + 1, MAX_PAIR_WORD_STEPS, MAX_AGGREGATE_WORD_STEPS,),
            Err(Error::Limit {
                resource: "license comparisons",
                ..
            })
        ));
        assert!(matches!(
            require_comparison_budget(MAX_COMPARISONS, MAX_PAIR_WORD_STEPS + 1, MAX_AGGREGATE_WORD_STEPS),
            Err(Error::ComputationLimit {
                resource: "one normalized license comparison",
                ..
            })
        ));
        assert!(matches!(
            require_comparison_budget(MAX_COMPARISONS, MAX_PAIR_WORD_STEPS, MAX_AGGREGATE_WORD_STEPS + 1),
            Err(Error::ComputationLimit {
                resource: "aggregate normalized license comparisons",
                ..
            })
        ));
    }
}
