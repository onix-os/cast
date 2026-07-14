// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

fn message() -> String {
    cast_fixture_greeting::greeting("vendored Cargo fixture")
}

fn main() {
    println!("{}", message());
}

#[cfg(test)]
mod tests {
    #[test]
    fn dependency_is_available_offline() {
        assert_eq!(super::message(), "hello from vendored Cargo fixture");
    }
}
