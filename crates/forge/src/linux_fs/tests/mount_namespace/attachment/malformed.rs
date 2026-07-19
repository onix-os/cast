use std::io;

use super::super::super::super::mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor};
use super::super::support::SyntheticMountNamespace;

const COMPONENTS: &[&str] = &["alpha", "bravo", "charlie"];

fn selector(components: &[&str]) -> String {
    format!("/{}", components.join("/"))
}

fn prepared_anchor(fixture: &SyntheticMountNamespace) -> io::Result<PreparedMountNamespaceAnchor> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare()
}

fn attempt(fixture: &SyntheticMountNamespace, authored: &str) -> io::Result<()> {
    let anchor = prepared_anchor(fixture)?;
    anchor.revalidate()?.prepare_task_rooted_attachment(authored).map(drop)
}

#[test]
fn malformed_absolute_lexical_selectors_fail_before_resolution() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    for authored in [
        "",
        ".",
        "relative",
        "/",
        "/alpha/",
        "/alpha//bravo",
        "/alpha/./bravo",
        "/alpha/../bravo",
        "/alpha\0bravo",
    ] {
        assert_eq!(
            attempt(&fixture, authored).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn selector_byte_component_and_depth_ceilings_are_exact() {
    let fixture = SyntheticMountNamespace::stable().unwrap();

    let component_255 = "a".repeat(255);
    let component_256 = "a".repeat(256);
    assert_ne!(
        attempt(&fixture, &format!("/{component_255}")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        attempt(&fixture, &format!("/{component_256}")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    let depth_128 = format!("/{}", std::iter::repeat("a").take(128).collect::<Vec<_>>().join("/"));
    let depth_129 = format!("/{}", std::iter::repeat("a").take(129).collect::<Vec<_>>().join("/"));
    assert_ne!(
        attempt(&fixture, &depth_128).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        attempt(&fixture, &depth_129).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    let mut exact_components = std::iter::repeat_with(|| "a".repeat(255)).take(15).collect::<Vec<_>>();
    exact_components.push("a".repeat(254));
    let exact_bytes = format!("/{}", exact_components.join("/"));
    let too_many_bytes = format!(
        "/{}",
        std::iter::repeat_with(|| "a".repeat(255))
            .take(16)
            .collect::<Vec<_>>()
            .join("/")
    );
    assert_eq!(exact_bytes.len(), 4_095);
    assert_eq!(too_many_bytes.len(), 4_096);
    assert_ne!(
        attempt(&fixture, &exact_bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        attempt(&fixture, &too_many_bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn missing_symlink_fifo_and_non_directory_components_are_rejected() {
    for index in 0..COMPONENTS.len() {
        let missing = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        missing.remove_attachment(COMPONENTS, index).unwrap();
        assert!(attempt(&missing, &selector(COMPONENTS)).is_err());
        missing.assert_outside_unchanged();

        let symlink = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        symlink.replace_attachment_symlink(COMPONENTS, index).unwrap();
        assert!(attempt(&symlink, &selector(COMPONENTS)).is_err());
        symlink.assert_outside_unchanged();

        let fifo = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        fifo.replace_attachment_fifo(COMPONENTS, index).unwrap();
        assert!(attempt(&fifo, &selector(COMPONENTS)).is_err());
        fifo.assert_outside_unchanged();

        let regular = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        regular.replace_attachment_regular(COMPONENTS, index).unwrap();
        assert!(attempt(&regular, &selector(COMPONENTS)).is_err());
        regular.assert_outside_unchanged();
    }
}
