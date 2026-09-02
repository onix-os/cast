
#[cfg(not(feature = "fixture-protocol"))]
compile_error!("cargo-features fixture requires the fixture-protocol feature");

pub const CLIENT_MESSAGE: &str = "cast cargo features fixture: client protocol enabled";
pub const DAEMON_MESSAGE: &str = "cast cargo features fixture: daemon protocol enabled";

#[cfg(test)]
mod tests {
    #[test]
    fn selected_feature_exposes_both_protocol_roles() {
        assert!(cfg!(feature = "fixture-protocol"));
        assert!(super::CLIENT_MESSAGE.ends_with("protocol enabled"));
        assert!(super::DAEMON_MESSAGE.ends_with("protocol enabled"));
    }
}
