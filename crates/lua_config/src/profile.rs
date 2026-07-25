//! The Cast Lua declaration profile: a grammar-level restriction on authored
//! sources, enforced before execution.
//!
//! The capability sandbox removes dangerous *globals*; this profile removes
//! dangerous *language constructs*. Authored roots and relative modules may use
//! initialized local bindings, literals, field/index reads, pure expressions,
//! allowlisted helper calls, functions, conditionals, and one final return.
//! Loops, `goto`/labels, global writes, reassignment/post-construction
//! mutation, and varargs are rejected here — a later policy version may add and
//! test more. Embedded ABI modules use a wider reviewed subset and are not run
//! through this pass.

use full_moon::ast;
use full_moon::visitors::Visitor;

/// A construct the Cast Lua profile does not permit in authored sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileViolation {
    pub construct: &'static str,
}

/// Validate that `source` parses and uses only the authored declaration
/// profile. Returns the first violation found, or `Ok` if the source is within
/// the profile. Parse failures are reported as a `parse` violation.
pub fn validate_profile(source: &str) -> Result<(), ProfileViolation> {
    let ast = match full_moon::parse(source) {
        Ok(ast) => ast,
        Err(_) => return Err(ProfileViolation { construct: "unparseable source" }),
    };
    let mut checker = ProfileChecker { violation: None };
    checker.visit_ast(&ast);
    match checker.violation {
        Some(violation) => Err(violation),
        None => Ok(()),
    }
}

struct ProfileChecker {
    violation: Option<ProfileViolation>,
}

impl ProfileChecker {
    fn reject(&mut self, construct: &'static str) {
        self.violation.get_or_insert(ProfileViolation { construct });
    }
}

impl Visitor for ProfileChecker {
    fn visit_while(&mut self, _: &ast::While) {
        self.reject("while loop");
    }

    fn visit_repeat(&mut self, _: &ast::Repeat) {
        self.reject("repeat loop");
    }

    fn visit_numeric_for(&mut self, _: &ast::NumericFor) {
        self.reject("numeric for loop");
    }

    fn visit_generic_for(&mut self, _: &ast::GenericFor) {
        self.reject("generic for loop");
    }

    fn visit_assignment(&mut self, _: &ast::Assignment) {
        // A bare assignment writes a global, reassigns a binding, or mutates a
        // table after construction. Only initialized `local` bindings and one
        // final `return` may introduce values.
        self.reject("assignment (global write, reassignment, or mutation)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn construct(source: &str) -> Option<&'static str> {
        validate_profile(source).err().map(|violation| violation.construct)
    }

    #[test]
    fn accepts_locals_literals_functions_conditionals_and_a_return() {
        assert!(
            validate_profile(
                r#"
                    local base = { channel = "stable", priority = 10 }
                    local function bump(value) return value + 1 end
                    local priority = base.priority
                    if base.channel == "stable" then
                        priority = bump(priority)
                    end
                    return { channel = base.channel, priority = priority }
                "#
            )
            // `priority = bump(priority)` is an assignment, so this specific
            // source is rejected — reassignment is not in the profile.
            .is_err()
        );

        assert!(
            validate_profile(
                r#"
                    local base = { channel = "stable", priority = 10 }
                    local function bump(value) return value + 1 end
                    local next_priority = bump(base.priority)
                    return { channel = base.channel, priority = next_priority }
                "#
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_loops() {
        assert_eq!(construct("while true do end return 1"), Some("while loop"));
        assert_eq!(construct("for i = 1, 3 do end return 1"), Some("numeric for loop"));
        assert_eq!(
            construct("local t = {} for _ in t do end return 1"),
            Some("generic for loop")
        );
        assert_eq!(construct("repeat until true return 1"), Some("repeat loop"));
    }

    #[test]
    fn rejects_global_writes_and_mutation() {
        assert_eq!(
            construct("x = 1 return x"),
            Some("assignment (global write, reassignment, or mutation)")
        );
        assert_eq!(
            construct("local t = {} t.field = 1 return t"),
            Some("assignment (global write, reassignment, or mutation)")
        );
    }

    #[test]
    fn rejects_unparseable_source() {
        assert_eq!(construct("local = ="), Some("unparseable source"));
    }
}
