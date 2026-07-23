//! Grammar-aware literal-import extraction for the Cast Lua profile.
//!
//! Lua only *parses* imports; the shared `declarative_config` core resolves
//! them. An import is the call `cast.import("<name>")` whose single argument is
//! a string literal. Embedded ABI imports use a semantic name such as
//! `cast.package.v3`; relative imports carry an exact `.lua` extension. Every
//! non-literal, computed, or malformed import becomes a rejecting
//! [`ImportRequest`] so the shared graph fails closed before VM execution.

use declarative_config::ImportRequest;
use full_moon::ast::{self, Expression, Index, Prefix, Suffix};
use full_moon::tokenizer::TokenType;
use full_moon::visitors::Visitor;

/// Parse `source` under the Lua 5.4 grammar and return one [`ImportRequest`]
/// per `cast.import(...)` call, in source order.
///
/// A parse failure is reported as a single invalid request so the caller's
/// diagnostic carries the source name. Computed or malformed import arguments
/// become invalid requests too; only grammar-recognized string literals become
/// embedded or relative requests.
pub fn discover_imports(source: &str) -> Vec<ImportRequest> {
    let ast = match full_moon::parse(source) {
        Ok(ast) => ast,
        Err(errors) => {
            let detail = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return vec![ImportRequest::invalid(format!(
                "lua source does not parse: {detail}"
            ))];
        }
    };
    let mut collector = ImportCollector { requests: Vec::new() };
    collector.visit_ast(&ast);
    collector.requests
}

/// Classify one literal import name into an embedded or relative request.
///
/// Relative imports are exactly those ending in `.lua`; everything else is an
/// embedded semantic ABI name resolved from the adapter-supplied catalog.
fn classify_literal(name: String) -> ImportRequest {
    if name.ends_with(".lua") {
        ImportRequest::relative(name)
    } else {
        ImportRequest::embedded(name)
    }
}

struct ImportCollector {
    requests: Vec<ImportRequest>,
}

impl Visitor for ImportCollector {
    fn visit_function_call(&mut self, call: &ast::FunctionCall) {
        if !is_cast_import(call) {
            return;
        }
        let request = match literal_argument(call) {
            Some(name) => classify_literal(name),
            None => ImportRequest::invalid(
                "cast.import requires a single string-literal argument".to_owned(),
            ),
        };
        self.requests.push(request);
    }
}

/// Is this call `cast.import( ... )` — prefix `cast`, one `.import` index?
fn is_cast_import(call: &ast::FunctionCall) -> bool {
    let Prefix::Name(name) = call.prefix() else {
        return false;
    };
    if identifier(name.token().token_type()).as_deref() != Some("cast") {
        return false;
    }
    let Some(Suffix::Index(Index::Dot { name: method, .. })) = call.suffixes().next() else {
        return false;
    };
    identifier(method.token().token_type()).as_deref() == Some("import")
}

/// Return the single string-literal argument of a `cast.import` call, or `None`
/// when the argument is computed, absent, or one of several.
fn literal_argument(call: &ast::FunctionCall) -> Option<String> {
    let Some(Suffix::Call(ast::Call::AnonymousCall(args))) = call.suffixes().nth(1) else {
        return None;
    };
    match args {
        ast::FunctionArgs::Parentheses { arguments, .. } => {
            let mut arguments = arguments.iter();
            let first = arguments.next()?;
            if arguments.next().is_some() {
                return None;
            }
            match first {
                Expression::String(token) => string_literal(token.token_type()),
                _ => None,
            }
        }
        ast::FunctionArgs::String(token) => string_literal(token.token_type()),
        _ => None,
    }
}

fn string_literal(token_type: &TokenType) -> Option<String> {
    match token_type {
        TokenType::StringLiteral { literal, .. } => Some(literal.to_string()),
        _ => None,
    }
}

fn identifier(token_type: &TokenType) -> Option<String> {
    match token_type {
        TokenType::Identifier { identifier } => Some(identifier.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_embedded_and_relative_literals_in_order() {
        let requests = discover_imports(
            r#"
                local pkg = cast.import("cast.package.v3")
                local helper = cast.import("./helper.lua")
                return { pkg = pkg, helper = helper }
            "#,
        );
        assert_eq!(
            requests,
            vec![
                ImportRequest::embedded("cast.package.v3"),
                ImportRequest::relative("./helper.lua"),
            ]
        );
    }

    #[test]
    fn a_source_with_no_imports_has_no_requests() {
        assert!(discover_imports("return { name = \"n\" }").is_empty());
    }

    #[test]
    fn a_computed_import_becomes_an_invalid_request() {
        let requests = discover_imports("local n = \"x\" return cast.import(n)");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], ImportRequest::Invalid { .. }));
    }

    #[test]
    fn a_concatenated_import_becomes_an_invalid_request() {
        let requests = discover_imports(r#"return cast.import("a" .. "b")"#);
        assert!(matches!(requests[0], ImportRequest::Invalid { .. }));
    }

    #[test]
    fn unparseable_source_becomes_a_single_invalid_request() {
        let requests = discover_imports("local = = =");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], ImportRequest::Invalid { .. }));
    }
}
