use std::{
    fs::Permissions,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use fs_err as fs;
use stone::StoneDigestWriterHasher;

use super::*;
use crate::package::collect::CollectionLimits;

fn add_rule(collector: &mut Collector, pattern: &str, package: &str, kind: PathRuleKind) {
    collector.add_rule(pattern, package, kind).unwrap();
}

fn write_regular(root: &Path, relative: &str, mode: u32) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"route fixture").unwrap();
    fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
    path
}

fn projected_package(collector: &Collector, relative: &str, kind: ProjectedPathKind) -> Arc<str> {
    let relative = Path::new(relative);
    let display = collector.root.join(relative);
    let context = CollectionContext::detached(CollectionLimits::default(), collector.deadline());
    collector
        .package_for_projected(&context, relative, kind, &display)
        .unwrap()
}

#[test]
fn actual_and_projected_regular_and_symlink_routes_have_identical_semantics() {
    let root = tempfile::tempdir().unwrap();
    let executable = write_regular(root.path(), "usr/bin/tool", 0o755);
    let data = write_regular(root.path(), "usr/share/data", 0o644);
    fs::create_dir_all(root.path().join("usr/lib")).unwrap();
    let link = root.path().join("usr/lib/current");
    symlink("libfixture.so.1", &link).unwrap();

    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "*", "fallback", PathRuleKind::Any);
    add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);
    add_rule(&mut collector, "/usr/lib/*", "links", PathRuleKind::Symlink);

    let mut hasher = StoneDigestWriterHasher::new();
    let actual_executable = collector.path(&executable, &mut hasher).unwrap().package;
    let actual_data = collector.path(&data, &mut hasher).unwrap().package;
    let actual_link = collector.path(&link, &mut hasher).unwrap().package;

    assert_eq!(
        actual_executable,
        projected_package(
            &collector,
            "usr/bin/tool",
            ProjectedPathKind::Regular { mode: 0o755 }
        )
    );
    assert_eq!(
        actual_data,
        projected_package(
            &collector,
            "usr/share/data",
            ProjectedPathKind::Regular { mode: 0o644 }
        )
    );
    assert_eq!(
        actual_link,
        projected_package(&collector, "usr/lib/current", ProjectedPathKind::Symlink)
    );
    assert_eq!(actual_executable.as_ref(), "executables");
    assert_eq!(actual_data.as_ref(), "fallback");
    assert_eq!(actual_link.as_ref(), "links");
}

#[test]
fn projected_routes_preserve_reverse_precedence_and_kind_rejection() {
    let root = tempfile::tempdir().unwrap();
    let regular = write_regular(root.path(), "usr/bin/tool", 0o755);
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "/usr/bin/tool", "lower", PathRuleKind::Executable);
    add_rule(&mut collector, "/usr/bin/tool", "higher", PathRuleKind::Executable);

    assert_eq!(
        projected_package(
            &collector,
            "usr/bin/tool",
            ProjectedPathKind::Regular { mode: 0o755 }
        )
        .as_ref(),
        "higher"
    );
    assert_eq!(
        collector
            .path(&regular, &mut StoneDigestWriterHasher::new())
            .unwrap()
            .package
            .as_ref(),
        "higher"
    );

    let relative = Path::new("usr/bin/tool");
    let display = root.path().join(relative);
    let context = CollectionContext::detached(CollectionLimits::default(), collector.deadline());
    assert!(matches!(
        collector.package_for_projected(
            &context,
            relative,
            ProjectedPathKind::Regular { mode: 0o644 },
            &display,
        ),
        Err(Error::NoMatchingRule { .. })
    ));
    assert!(matches!(
        collector.package_for_projected(&context, relative, ProjectedPathKind::Symlink, &display),
        Err(Error::NoMatchingRule { .. })
    ));
}
