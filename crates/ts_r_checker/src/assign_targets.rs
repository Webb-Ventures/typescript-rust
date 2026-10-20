//! Assignment targets that are not references (TS2364).
//!
//! Rules observed from tsc 7.0.2:
//! - The left side of `=` and of compound assignments must be a variable or a property
//!   access: an identifier, `a.b`, `a[b]`, any of them parenthesized or wrapped in `!`,
//!   `as`, `satisfies` or `<T>`. Anything else (`f()`, `new y`, `this`, a literal,
//!   `obj.fn<number>`) is TS2364 at the whole left side, parentheses included, and the
//!   assignment is not checked further. An object or array literal with a compound
//!   operator is not (`[a, b] += 1` is an operator error).
//! - A property access in an optional chain (`a?.b = 1`, not `(a?.b).c = 1`) is TS2779.
//! - The same holds for each target inside a destructuring assignment pattern (`[""] =`,
//!   `{ a: 1 } =`, `for ([""] of ...)`); nested patterns are walked, `[a = v]` checks `a`.
//!   An object rest target (`{ ...f() } =`) is TS2701 instead, TS2778 when optional.
//! - An expression head of `for...in`/`for...of` is TS2406/TS2487 (TS2780/TS2781 when
//!   optional); `++`/`--` operands in an optional chain are TS2777 (expr.rs). A `for...in`
//!   head is checked only when its type accepts the keys (TS2405 first: `for (this in o)`).
//! - A lone `super` target is left to the parser (TS1034).
//! - A missing operand (a syntax error) is not reported.

use ts_r_ast::{Ast, Node, NodeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::diagnostics_generated::DiagnosticInfo;

/// Wrapper steps skipped looking for the reference inside `((x!) as T)`.
const MAX_WRAPPERS: u32 = 64;

/// `true` when `n` is a variable or property access, possibly parenthesized or wrapped in
/// `!`, `as`, `satisfies` or a `<T>` assertion (or missing).
pub(crate) fn is_reference_shape(ast: &Ast, n: NodeId) -> bool {
    let mut inner = n;
    for _ in 0..MAX_WRAPPERS {
        match *ast.node(inner) {
            Node::NonNull { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::Paren { expr } => inner = expr,
            _ => break,
        }
    }
    matches!(
        ast.node(inner),
        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } | Node::Missing {}
    )
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2364/TS2779 when the assignment target `l` is not a reference; returns whether it
    /// is.
    pub(crate) fn check_assignment_target_shape(&mut self, l: Loc) -> bool {
        self.check_target_shape(
            l,
            dg::THE_LEFT_HAND_SIDE_OF_AN_ASSIGNMENT_EXPRESSION_MUST_BE_A_VARIABLE_OR_A_PROPERTY_A_2364,
        )
    }

    /// TS2364 (TS2701 for an object rest element, `object_rest`) for a destructuring
    /// assignment element `l` that is neither a nested pattern nor a reference.
    pub(crate) fn check_destructuring_element_shape(&mut self, l: Loc, object_rest: bool) -> bool {
        let info = if object_rest {
            dg::THE_TARGET_OF_AN_OBJECT_REST_ASSIGNMENT_MUST_BE_A_VARIABLE_OR_A_PROPERTY_ACCESS_2701
        } else {
            dg::THE_LEFT_HAND_SIDE_OF_AN_ASSIGNMENT_EXPRESSION_MUST_BE_A_VARIABLE_OR_A_PROPERTY_A_2364
        };
        self.check_target_shape(l, info)
    }

    /// TS2406/TS2780 (`for...in`) or TS2487/TS2781 (`for...of`, `is_of`) for an expression
    /// head `l` that is not a reference. Declarations and patterns are checked elsewhere.
    pub(crate) fn check_for_head_target(&mut self, l: Loc, is_of: bool) {
        let ast = self.ast(l.file);
        if matches!(ast.node(l.node), Node::VarDecl { .. }) {
            return;
        }
        let (info, optional) = if is_of {
            (
                dg::THE_LEFT_HAND_SIDE_OF_A_FOR_OF_STATEMENT_MUST_BE_A_VARIABLE_OR_A_PROPERTY_ACCESS_2487,
                dg::THE_LEFT_HAND_SIDE_OF_A_FOR_OF_STATEMENT_MAY_NOT_BE_AN_OPTIONAL_PROPERTY_ACCESS_2781,
            )
        } else {
            (
                dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_MUST_BE_A_VARIABLE_OR_A_PROPERTY_ACCESS_2406,
                dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_MAY_NOT_BE_AN_OPTIONAL_PROPERTY_ACCESS_2780,
            )
        };
        self.check_target_shape_with(l, info, optional);
    }

    fn check_target_shape(&mut self, l: Loc, info: DiagnosticInfo) -> bool {
        let optional = if info.code == 2701 {
            dg::THE_TARGET_OF_AN_OBJECT_REST_ASSIGNMENT_MAY_NOT_BE_AN_OPTIONAL_PROPERTY_ACCESS_2778
        } else {
            dg::THE_LEFT_HAND_SIDE_OF_AN_ASSIGNMENT_EXPRESSION_MAY_NOT_BE_AN_OPTIONAL_PROPERTY_A_2779
        };
        self.check_target_shape_with(l, info, optional)
    }

    fn check_target_shape_with(
        &mut self,
        l: Loc,
        info: DiagnosticInfo,
        optional: DiagnosticInfo,
    ) -> bool {
        let ast = self.ast(l.file);
        let inner = crate::nodes::skip_parens(ast, l.node);
        match ast.node(inner) {
            // A lone `super` is the parser's TS1034.
            Node::ObjectLit { .. } | Node::ArrayLit { .. } | Node::Super {} => return true,
            Node::Member { .. } | Node::Index { .. }
                if self.in_optional_chain(Loc::new(l.file, inner)) =>
            {
                self.error(l, optional, &[]);
                return false;
            }
            _ => {}
        }
        if is_reference_shape(ast, l.node) {
            return true;
        }
        self.error(l, info, &[]);
        false
    }
}
