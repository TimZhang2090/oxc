use oxc_ast::AstKind;
use oxc_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::Error,
};
use oxc_macros::declare_oxc_lint;
use oxc_span::Span;

use crate::{context::LintContext, rule::Rule, AstNode};

#[derive(Debug, Error, Diagnostic)]
#[error(
    "eslint-plugin-jsdoc(implements-on-classes): `@implements` used on a non-constructor function"
)]
#[diagnostic(severity(warning), help("Add `@class` tag or use ES6 class syntax."))]
struct ImplementsOnClassesDiagnostic(#[label] pub Span);

#[derive(Debug, Default, Clone)]
pub struct ImplementsOnClasses;

declare_oxc_lint!(
    /// ### What it does
    /// Reports an issue with any non-constructor function using `@implements`.
    ///
    /// ### Why is this bad?
    /// Constructor functions should be
    /// whether marked with `@class`, `@constructs`, or being an ES6 class constructor.
    ///
    /// ### Example
    /// ```javascript
    /// // Passing
    /// class Foo {
    ///   /**
    ///    * @implements {SomeClass}
    ///    */
    ///   constructor() {}
    /// }
    /// /**
    ///  * @implements {SomeClass}
    ///  * @class
    ///  */
    /// function quux () {}
    ///
    /// // Failing
    /// /**
    ///  * @implements {SomeClass}
    ///  */
    /// function quux () {}
    /// ```
    ImplementsOnClasses,
    correctness
);

/// Get the definition root node of a function.
/// JSDoc often appears on the parent node of a function.
///
/// ```js
/// /** FunctionDeclaration */
/// function foo() {}
///
/// /** VariableDeclaration > VariableDeclarator > FunctionExpression */
/// const bar = function() {}
///
/// /** VariableDeclaration > VariableDeclarator > ArrowFunctionExpression */
/// const baz = () => {}
/// ```
fn get_function_definition_node<'a, 'b>(
    node: &'b AstNode<'a>,
    ctx: &'b LintContext<'a>,
) -> Option<&'b AstNode<'a>> {
    match node.kind() {
        AstKind::Function(f) if f.is_function_declaration() => return Some(node),
        AstKind::Function(f) if f.is_expression() => {}
        AstKind::ArrowFunctionExpression(_) => {}
        _ => return None,
    };

    let mut current_node = node;
    while let Some(parent_node) = ctx.nodes().parent_node(current_node.id()) {
        match parent_node.kind() {
            // `MethodDefinition` is not a target
            AstKind::VariableDeclarator(_) | AstKind::ParenthesizedExpression(_) => {
                current_node = parent_node;
            }
            AstKind::VariableDeclaration(_) => return Some(parent_node),
            _ => return None,
        }
    }

    None
}

impl Rule for ImplementsOnClasses {
    fn run<'a>(&self, node: &AstNode<'a>, ctx: &LintContext<'a>) {
        let Some(jsdocs) = get_function_definition_node(node, ctx)
            .and_then(|node| ctx.jsdoc().get_all_by_node(node))
        else {
            return;
        };

        let settings = &ctx.settings().jsdoc;

        let resolved_implements_tag_name = settings.resolve_tag_name("implements");
        let resolved_class_tag_name = settings.resolve_tag_name("class");
        let resolved_constructor_tag_name = settings.resolve_tag_name("constructor");

        let (mut implements_found, mut class_or_ctor_found) = (None, false);
        for jsdoc in &jsdocs {
            for tag in jsdoc.tags() {
                let tag_name = tag.kind.parsed();

                if tag_name == resolved_implements_tag_name {
                    implements_found = Some(tag.kind.span);
                }
                if tag_name == resolved_class_tag_name || tag_name == resolved_constructor_tag_name
                {
                    class_or_ctor_found = true;
                }
            }
        }

        if let Some(span) = implements_found {
            if !class_or_ctor_found {
                ctx.diagnostic(ImplementsOnClassesDiagnostic(span));
            }
        }
    }
}

#[test]
fn test() {
    use crate::tester::Tester;

    let pass = vec![
        (
            "
			      /**
			       * @implements {SomeClass}
			       * @class
			       */
			      function quux () {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @implements {SomeClass}
			       * @constructor
			       */
			      function quux () {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @implements {SomeClass}
			       * @constructor
			       */
			      const quux = () => {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       *
			       */
			      class quux {
			        /**
			         * @implements {SomeClass}
			         */
			        constructor () {
			
			        }
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       *
			       */
			      const quux = class {
			        /**
			         * @implements {SomeClass}
			         */
			        constructor () {
			
			        }
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       *
			       */
			      class quux {
			        /**
			         * @implements {SomeClass}
			         */
			        foo() {
			
			        }
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       *
			       */
			      function quux () {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @function
			       * @implements {SomeClass}
			       */
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @callback
			       * @implements {SomeClass}
			       */
			      ",
            None,
            None,
        ),
    ];

    let fail = vec![
        (
            "
			      /**
			       * @implements {SomeClass}
			       */
			      function quux () {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @implements {SomeClass}
			       */
			      const quux = () => {
			
			      }
			      ",
            None,
            None,
        ),
        (
            "
			      /**
			       * @function
			       * @implements {SomeClass}
			       */
			      const quux = function() {
			
			      }
			      ",
            None,
            None,
        ),
    ];

    Tester::new(ImplementsOnClasses::NAME, pass, fail).test_and_snapshot();
}
