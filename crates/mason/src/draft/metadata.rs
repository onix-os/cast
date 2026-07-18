// SPDX-FileCopyrightText: 2024 AerynOS Developers

use stone_recipe::UpstreamSpec;

use super::Upstream;

mod basic;
mod github;
mod gitlab;
mod metacpan;
mod pypi;

#[derive(Default)]
pub struct Metadata {
    pub source: Source,
    upstreams: Vec<Upstream>,
}

#[derive(Default)]
pub struct Source {
    pub name: String,
    pub version: String,
    pub homepage: String,
    /// Informational canonical URI suggested by metadata matchers. Draft
    /// sources deliberately retain the exact fetched URL bound to their hash.
    #[allow(dead_code)]
    pub uri: String,
}

impl Metadata {
    pub fn new(upstreams: Vec<Upstream>) -> Self {
        let mut source = Source::default();

        // Try to identify source metadata from the first upstream
        if let Some(upstream) = upstreams.first() {
            for matcher in Matcher::ALL {
                if let Some(matched) = match matcher {
                    Matcher::Basic => basic::source(&upstream.uri),
                    Matcher::Github => github::source(&upstream.uri),
                    Matcher::Gitlab => gitlab::source(&upstream.uri),
                    Matcher::Pypi => pypi::source(&upstream.uri),
                    Matcher::Metacpan => metacpan::source(&upstream.uri),
                } {
                    source = matched;
                    break;
                }
            }
        }

        Self { source, upstreams }
    }

    pub fn upstream_specs(&self) -> Vec<UpstreamSpec> {
        self.upstreams
            .iter()
            .map(|Upstream { uri, hash }| {
                UpstreamSpec::Archive {
                    // The hash was calculated from this exact fetched URL.
                    // Metadata matchers may suggest a canonical project URI,
                    // but substituting it here would detach URL from digest.
                    url: uri.to_string(),
                    hash: hash.clone(),
                    rename: None,
                    strip_dirs: None,
                    unpack: true,
                    unpack_dir: None,
                }
            })
            .collect()
    }
}

enum Matcher {
    Basic,
    Gitlab,
    Github,
    Pypi,
    Metacpan,
}

impl Matcher {
    const ALL: &'static [Self] = &[Self::Github, Self::Gitlab, Self::Pypi, Self::Metacpan, Self::Basic];
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn canonical_metadata_uri_never_rebinds_a_hash_to_different_bytes() {
        let fetched = Url::parse(
            "https://files.pythonhosted.org/packages/59/83/a60af4e83c492c7dceceeabd677aa87bbaf2d8910b3d1b973295e560f421/pyzk-0.9.tar.gz",
        )
        .unwrap();
        let metadata = Metadata::new(vec![Upstream {
            uri: fetched.clone(),
            hash: "a".repeat(64),
        }]);

        assert_ne!(metadata.source.uri, fetched.as_str());
        assert!(matches!(
            metadata.upstream_specs().as_slice(),
            [UpstreamSpec::Archive { url, hash, .. }]
                if url == fetched.as_str() && hash == &"a".repeat(64)
        ));
    }
}
