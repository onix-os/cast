use sha2::{Digest, Sha256};

#[test]
fn normalized_build_policy_root_matches_the_complete_owned_snapshot() {
    let policy = repository_policy_value();
    let rendered = format!("{policy:#?}");
    let digest = format!("{:x}", Sha256::digest(rendered.as_bytes()));

    // The derived Debug tree visits every field of every owned policy value.
    // Length plus digest keeps this exhaustive characterization compact while
    // avoiding a generated multi-thousand-line fixture.
    assert_eq!(rendered.len(), 232_027);
    assert_eq!(digest, "76c198b746a808cc36e2302bccde2512d5fde0034075f758d266f2a3d760d266");
}
