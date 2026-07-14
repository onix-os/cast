// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub fn greeting(subject: &str) -> String {
    format!("hello from {subject}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn greeting_is_deterministic() {
        assert_eq!(super::greeting("the vendor tree"), "hello from the vendor tree");
    }
}
