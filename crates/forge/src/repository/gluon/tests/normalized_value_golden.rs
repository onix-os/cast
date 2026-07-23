use super::*;

#[derive(::std::fmt::Debug, PartialEq, Eq)]
enum SourceValue {
    Direct(String),
    Root {
        base_uri: String,
        channel: String,
        version: String,
        arch: String,
    },
}

type RepositoryValue = (String, String, SourceValue, u64, bool);

fn source_value(source: &Source) -> SourceValue {
    match source {
        Source::DirectIndex(uri) => SourceValue::Direct(uri.to_string()),
        Source::RootIndex(root) => {
            let repository::RootIndexSource {
                base_uri,
                channel,
                version,
                arch,
            } = root;
            SourceValue::Root {
                base_uri: base_uri.to_string(),
                channel: channel.to_string(),
                version: version.to_string(),
                arch: arch.clone(),
            }
        }
    }
}

fn repository_value(id: &repository::Id, repository: &Repository) -> RepositoryValue {
    let Repository {
        description,
        source,
        priority,
        active,
    } = repository;
    (
        id.to_string(),
        description.clone(),
        source_value(source),
        u64::from(*priority),
        *active,
    )
}

#[test]
fn generated_repository_fragment_has_exact_normalized_owned_value() {
    let decoded = RepositoryCodec
        .decode(
            &Evaluator::default(),
            &GluonSource::new(
                "repository-fragment.glu",
                include_str!("../../../../../../tests/fixtures/gluon/goldens/repository-fragment.glu"),
            ),
        )
        .unwrap();
    let actual = decoded
        .value
        .iter()
        .map(|(id, repository)| repository_value(id, repository))
        .collect::<Vec<_>>();
    let expected = vec![
        (
            "a-direct".to_owned(),
            String::new(),
            SourceValue::Direct("file:///var/cache/local.index".to_owned()),
            0,
            true,
        ),
        (
            "z-root".to_owned(),
            String::new(),
            SourceValue::Root {
                base_uri: "https://packages.example.test/".to_owned(),
                channel: "main".to_owned(),
                version: "stream/volatile".to_owned(),
                arch: "x86_64".to_owned(),
            },
            0,
            true,
        ),
    ];

    assert_eq!(actual, expected);
}
