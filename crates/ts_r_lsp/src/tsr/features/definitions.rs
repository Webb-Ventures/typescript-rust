//! Positions where a new name is being written, which get no completion list (or only
//! keywords): `var |`, `class |`, `function f(|`, `enum E { |`, `import * as |`, ...
//!
//! Rules learned from tsc 7's answers on its fourslash tests
//! (`completionListAtIdentifierDefinitionLocations_*`, `completionListAfterPropertyName`,
//! `importTypeCompletions*`) and from `tsc: tsc/internal/ls/completions.go` (the checks that
//! end completion at a definition location), written here on our AST:
//!
//! - After `var`/`let`/`const`, `class`, `interface`, `enum`, `type`, `function` (and
//!   `function*`) the declared name comes next: nothing is offered. Namespace and module
//!   names do get the names in scope.
//! - In a declaration's type parameter list, after `<` or `,`: nothing. After `infer`:
//!   nothing.
//! - In the parameter list of a function, method or accessor, after `(` or `,` (not inside
//!   a parameter's type or default): nothing; in a constructor's, only the parameter property
//!   modifiers. After a parameter's name with no comma yet: nothing.
//! - After `catch (`, in an enum body after `{` or `,`, in an array binding pattern after
//!   `[` or `,`: nothing.
//! - `import |`, `import type |`, `import * as |`, `{ a as | }` in an import or export, and
//!   `import { | }` with no module after it: nothing.

use super::position::{node_covering, token_before};
use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

/// The modifiers a constructor parameter may start with.
pub const PARAMETER_KEYWORDS: &[&str] = &["override", "private", "protected", "public", "readonly"];

/// Levels walked up from the token at most.
const MAX_UP: usize = 64;

/// What a definition location offers.
pub enum Spot {
    /// No list at all.
    Nothing,
    /// Only these keywords.
    Keywords(&'static [&'static str]),
}

/// Whether the word starting at `word` (empty or not) is a name being declared.
pub fn definition_spot(
    c: &Checker<'_>,
    file: FileId,
    word_node: NodeId,
    word: u32,
) -> Option<Spot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    // A word already typed: the node it is.
    if matches!(ast.node(word_node), Node::Ident { .. }) && ast.span(word_node).start == word {
        let p = parents.parent(word_node)?;
        return match *ast.node(p) {
            Node::ImportDefaultSpecifier { local }
            | Node::ImportNamespaceSpecifier { local }
            | Node::ImportEquals { name: local, .. }
                if local == word_node =>
            {
                Some(Spot::Nothing)
            }
            Node::ImportSpecifier {
                imported, local, ..
            } if local == word_node => {
                let decl = parents.parent(p)?;
                let sourced = matches!(*ast.node(decl), Node::ImportDecl { source, .. }
                    if ast.span(source).end > ast.span(source).start);
                (imported.is_some() || !sourced).then_some(Spot::Nothing)
            }
            Node::EnumMember { name, .. } if name == word_node => Some(Spot::Nothing),
            Node::ArrayPattern { .. } | Node::Rest { .. } if in_declaration_pattern(c, file, p) => {
                Some(Spot::Nothing)
            }
            Node::Param { pattern, .. } if pattern == word_node => {
                Some(if in_constructor(c, file, p) {
                    Spot::Keywords(PARAMETER_KEYWORDS)
                } else {
                    Spot::Nothing
                })
            }
            _ => None,
        };
    }
    let (tok_at, tok) = token_before(ast, text, word)?;
    let holder = node_covering(ast, tok_at)?;
    let before = token_before(ast, text, tok_at).map(|(_, t)| t);
    match tok {
        "var" | "let" | "const" | "using" => {
            return ancestors(c, file, holder)
                .any(|n| matches!(ast.node(n), Node::VarDecl { .. }))
                .then_some(Spot::Nothing);
        }
        "class" | "interface" | "enum" | "function" | "type" => {
            let ok = matches!(
                (tok, ast.node(holder)),
                ("class", Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    | ("interface", Node::Interface { .. })
                    | ("enum", Node::Enum { .. })
                    | (
                        "function",
                        Node::FunctionDecl { .. } | Node::FunctionExpr { .. }
                    )
                    | ("type", Node::TypeAlias { .. })
            ) || (tok == "type"
                && before == Some("import")
                && matches!(
                    ast.node(holder),
                    Node::ImportDecl { .. } | Node::ImportEquals { .. }
                ));
            return ok.then_some(Spot::Nothing);
        }
        "*" if before == Some("function") => return Some(Spot::Nothing),
        "infer" if matches!(ast.node(holder), Node::InferType { .. }) => {
            return Some(Spot::Nothing);
        }
        "import"
            if matches!(
                ast.node(holder),
                Node::ImportDecl { .. } | Node::ImportEquals { .. }
            ) =>
        {
            return Some(Spot::Nothing);
        }
        "as" if matches!(
            ast.node(holder),
            Node::ImportNamespaceSpecifier { .. }
                | Node::ImportSpecifier { .. }
                | Node::ExportSpecifier { .. }
                | Node::ImportDecl { .. }
                | Node::ExportNamed { .. }
        ) =>
        {
            return Some(Spot::Nothing);
        }
        "(" if before == Some("catch") => return Some(Spot::Nothing),
        _ => {}
    }
    // Lists: type parameters, parameters, enum members, array patterns.
    for n in ancestors(c, file, holder).take(MAX_UP) {
        match *ast.node(n) {
            Node::TypeParams { .. }
                if matches!(tok, "<" | ",") && is_list_separator(c, file, n, tok_at) =>
            {
                return Some(Spot::Nothing);
            }
            Node::TypeParam { .. } => return None,
            Node::Enum { name, .. } if matches!(tok, "{" | ",") && ast.span(name).end <= tok_at => {
                return Some(Spot::Nothing);
            }
            Node::ArrayPattern { .. } if matches!(tok, "[" | ",") => {
                return in_declaration_pattern(c, file, n).then_some(Spot::Nothing);
            }
            Node::Param { pattern, .. } => {
                // After the name, before a `:`, `=` or `,`: nothing more to complete.
                return (ast.span(pattern).end <= tok_at + tok.len() as u32
                    && tok_at < ast.span(pattern).end
                    && matches!(ast.node(pattern), Node::Ident { .. }))
                .then_some(Spot::Nothing);
            }
            Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. }
                if matches!(tok, "(" | ",") =>
            {
                // The `(` of the parameter list, or a `,` between parameters.
                let list = ast.list(params);
                let open = list.first().map_or(tok_at, |&p| ast.span(p).start);
                if tok == "(" && tok_at > open {
                    return None;
                }
                if tok == "," && !list.iter().any(|&p| ast.span(p).end <= tok_at) {
                    return None;
                }
                return Some(if in_constructor(c, file, n) {
                    Spot::Keywords(PARAMETER_KEYWORDS)
                } else {
                    Spot::Nothing
                });
            }
            Node::Block { .. }
            | Node::Program { .. }
            | Node::Arrow { .. }
            | Node::Call { .. }
            | Node::New { .. }
            | Node::Paren { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. } => return None,
            _ => {}
        }
    }
    None
}

/// Whether the `,` or `<` at `at` belongs to the type parameter list `n` itself.
fn is_list_separator(c: &Checker<'_>, file: FileId, n: NodeId, at: u32) -> bool {
    let ast = c.ast(file);
    let Node::TypeParams { params } = *ast.node(n) else {
        return false;
    };
    let span = ast.span(n);
    if at == span.start {
        return true;
    }
    if !(span.start < at && at < span.end.max(at + 1)) {
        return false;
    }
    // Not inside one of the parameters (their constraints and defaults hold commas too).
    !ast.list(params).iter().any(|&p| {
        let s = ast.span(p);
        s.start <= at && at < s.end
    })
}

/// The node and its ancestors.
fn ancestors<'a>(c: &'a Checker<'_>, file: FileId, n: NodeId) -> impl Iterator<Item = NodeId> + 'a {
    let parents = c.file(file).parents();
    let mut cur = Some(n);
    std::iter::from_fn(move || {
        let n = cur?;
        cur = parents.parent(n);
        Some(n)
    })
    .take(10_000)
}

/// Whether the pattern `n` binds names of a variable or parameter declaration.
fn in_declaration_pattern(c: &Checker<'_>, file: FileId, n: NodeId) -> bool {
    let ast = c.ast(file);
    for a in ancestors(c, file, n).take(MAX_UP) {
        match ast.node(a) {
            Node::ArrayPattern { .. }
            | Node::ObjectPattern { .. }
            | Node::BindingProperty { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. } => {}
            Node::VarDeclarator { .. } | Node::Param { .. } => return true,
            _ => return false,
        }
    }
    false
}

/// Whether `n` (a parameter, or the function holding the list) belongs to a constructor.
fn in_constructor(c: &Checker<'_>, file: FileId, n: NodeId) -> bool {
    let ast = c.ast(file);
    for a in ancestors(c, file, n).take(4) {
        if let Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        } = ast.node(a)
        {
            return true;
        }
    }
    false
}
