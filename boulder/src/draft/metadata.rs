// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

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
            .enumerate()
            .map(|(i, Upstream { uri, hash })| {
                let uri_to_use = if i == 0 && !self.source.uri.is_empty() {
                    &self.source.uri
                } else {
                    uri.as_str()
                };
                UpstreamSpec::Archive {
                    url: uri_to_use.to_owned(),
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
