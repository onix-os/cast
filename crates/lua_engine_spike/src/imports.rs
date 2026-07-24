//! Grammar-aware literal-import extraction spike.
//!
//! Cast's Lua profile expresses an import as a call `cast.import("<name>")`
//! whose single argument is a string *literal*. This module proves the
//! maintained `full_moon` parser can recognize those calls from the real Lua
//! grammar and reject any computed/dynamic import — the security boundary the
//! plan requires (a regex scanner is explicitly disallowed).

use full_moon::ast::{self, Expression, Index, Prefix, Suffix};
use full_moon::tokenizer::TokenType;
use full_moon::visitors::Visitor;

/// Failure while extracting imports from a Lua source.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportError {
    /// The source is not valid Lua under the selected grammar.
    Parse,
    /// A `cast.import(...)` call whose argument is not a single string literal
    /// (e.g. a variable, concatenation, or table). These are rejected because
    /// only grammar-recognized literal imports may reach the shared resolver.
    ComputedImport,
}

/// Extract every literal `cast.import("...")` name, in source order.
pub fn extract_imports(source: &str) -> Result<Vec<String>, ImportError> {
    let ast = full_moon::parse(source).map_err(|_| ImportError::Parse)?;
    let mut collector = ImportCollector {
        imports: Vec::new(),
        error: None,
    };
    collector.visit_ast(&ast);
    match collector.error {
        Some(error) => Err(error),
        None => Ok(collector.imports),
    }
}

struct ImportCollector {
    imports: Vec<String>,
    error: Option<ImportError>,
}

impl Visitor for ImportCollector {
    fn visit_function_call(&mut self, call: &ast::FunctionCall) {
        if !is_cast_import(call) {
            return;
        }
        match import_argument(call) {
            Some(ImportArgument::Literal(name)) => self.imports.push(name),
            Some(ImportArgument::Computed) | None => {
                self.error.get_or_insert(ImportError::ComputedImport);
            }
        }
    }
}

/// Is this call `cast.import( ... )` (prefix `cast`, one `.import` index)?
fn is_cast_import(call: &ast::FunctionCall) -> bool {
    let Prefix::Name(name) = call.prefix() else {
        return false;
    };
    if token_word(name.token().token_type()).as_deref() != Some("cast") {
        return false;
    }
    let mut suffixes = call.suffixes();
    let Some(Suffix::Index(Index::Dot { name: method, .. })) = suffixes.next() else {
        return false;
    };
    token_word(method.token().token_type()).as_deref() == Some("import")
}

enum ImportArgument {
    Literal(String),
    Computed,
}

/// Inspect the call arguments and classify the single import argument.
fn import_argument(call: &ast::FunctionCall) -> Option<ImportArgument> {
    let call_suffix = call.suffixes().nth(1)?;
    let Suffix::Call(ast::Call::AnonymousCall(args)) = call_suffix else {
        return Some(ImportArgument::Computed);
    };
    match args {
        ast::FunctionArgs::Parentheses { arguments, .. } => {
            let mut arguments = arguments.iter();
            let first = arguments.next()?;
            if arguments.next().is_some() {
                return Some(ImportArgument::Computed);
            }
            Some(classify_expression(first))
        }
        ast::FunctionArgs::String(token) => Some(classify_string_token(token.token_type())),
        _ => Some(ImportArgument::Computed),
    }
}

fn classify_expression(expression: &Expression) -> ImportArgument {
    match expression {
        Expression::String(token) => classify_string_token(token.token_type()),
        _ => ImportArgument::Computed,
    }
}

fn classify_string_token(token_type: &TokenType) -> ImportArgument {
    match token_type {
        TokenType::StringLiteral { literal, .. } => {
            ImportArgument::Literal(literal.to_string())
        }
        _ => ImportArgument::Computed,
    }
}

fn token_word(token_type: &TokenType) -> Option<String> {
    match token_type {
        TokenType::Identifier { identifier } => Some(identifier.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_embedded_and_relative_literal_imports_in_order() {
        let source = r#"
            local pkg = cast.import("cast.package.v3")
            local helper = cast.import("./helper.lua")
            return { pkg = pkg, helper = helper }
        "#;
        assert_eq!(
            extract_imports(source),
            Ok(vec![
                "cast.package.v3".to_owned(),
                "./helper.lua".to_owned(),
            ])
        );
    }

    #[test]
    fn a_source_with_no_imports_yields_an_empty_list() {
        assert_eq!(
            extract_imports("return { name = \"n\", value = 1 }"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn rejects_a_computed_import_argument() {
        let source = r#"
            local name = "cast.package.v3"
            return cast.import(name)
        "#;
        assert_eq!(extract_imports(source), Err(ImportError::ComputedImport));
    }

    #[test]
    fn rejects_a_concatenated_import_argument() {
        let source = r#"return cast.import("cast.package." .. "v3")"#;
        assert_eq!(extract_imports(source), Err(ImportError::ComputedImport));
    }

    #[test]
    fn rejects_unparseable_source() {
        assert_eq!(extract_imports("local = = ="), Err(ImportError::Parse));
    }
}
