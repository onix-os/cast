
fn message() -> &'static str {
    "cast cargo fixture"
}

fn main() {
    println!("{}", message());
}

#[cfg(test)]
mod tests {
    #[test]
    fn message_is_stable() {
        assert_eq!(super::message(), "cast cargo fixture");
    }
}
