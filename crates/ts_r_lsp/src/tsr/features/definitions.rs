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
//! - A JSDoc comment between the token and the cursor lifts all of this (tsc reads a token
//!   inside the comment).
//! - `import type |`, `import * as |` and `{ a as | }` in an import or export: nothing.
//!   The name right after `import` (`import |`, `import f|`, `import f| =`) and a name in
//!   `import { | }` with no module after it: only `type` (nothing in JavaScript or in
//!   `import type { | }`). After a whole import clause with
//!   no module yet (`import {} |`, `import * as ns |`, `import D |`): only `from`
//!   (`importStatementCompletions*`).

use super::position::{node_covering, token_before};
use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

/// The modifiers a constructor parameter may start with.
pub const PARAMETER_KEYWORDS: &[&str] = &["override", "private", "protected", "public", "readonly"];

/// `type`, offered where an import's names start.
const TYPE_KEYWORD: &[&str] = &["type"];

/// `from`, offered after an import clause with no module.
const FROM_KEYWORD: &[&str] = &["from"];

/// Levels walked up from the token at most.
const MAX_UP: usize = 64;

/// What a definition location offers.
pub enum Spot {
    /// No list at all.
    Nothing,
    /// Only these keywords.
    Keywords(&'static [&'static str]),
}

/// Whether the word starting at `word` (empty or not) is a name being declared. `typed`: the
/// cursor is past the word's start (at the start of an existing name, the token before the
/// name decides, as for an empty word).
pub fn definition_spot(
    c: &Checker<'_>,
    file: FileId,
    word_node: NodeId,
    word: u32,
    typed: bool,
) -> Option<Spot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    let ts = ast.source_kind.is_typescript();
    if let Some(spot) = import_keyword_spot(c, file, word) {
        return Some(spot);
    }
    // A word already typed: the node it is.
    if typed
        && matches!(ast.node(word_node), Node::Ident { .. })
        && ast.span(word_node).start == word
    {
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
                let type_only = matches!(
                    *ast.node(decl),
                    Node::ImportDecl {
                        type_only: true,
                        ..
                    }
                );
                if imported.is_some() || (!sourced && (type_only || !ts)) {
                    Some(Spot::Nothing)
                } else if !sourced {
                    Some(Spot::Keywords(TYPE_KEYWORD))
                } else {
                    None
                }
            }
            Node::EnumMember { name, .. } if name == word_node => Some(Spot::Nothing),
            Node::ArrayPattern { .. } | Node::Rest { .. } if in_declaration_pattern(c, file, p) => {
                Some(Spot::Nothing)
            }
            // `var { x: n| }`: the local name of a binding property.
            Node::BindingProperty { value: Some(v), .. }
                if v == word_node && in_declaration_pattern(c, file, p) =>
            {
                Some(Spot::Nothing)
            }
            // `(...a|`: a rest parameter's name, in a constructor too.
            Node::Param {
                pattern,
                rest: true,
                ..
            } if pattern == word_node => Some(Spot::Nothing),
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
    // A JSDoc comment right before the cursor is what tsc reads as the token before it, and
    // that blocks nothing (observed: `{ /** doc */ |Name` in an enum gets the list).
    if text
        .get(tok_at as usize + tok.len()..word as usize)
        .is_some_and(|between| between.contains("/**"))
    {
        return None;
    }
    let holder = node_covering(ast, tok_at)?;
    let before = token_before(ast, text, tok_at).map(|(_, t)| t);
    let after_dot = before == Some(".");
    match tok {
        // These keywords (or names spelled like them, as `interface` before a line break is)
        // are always followed by a name being declared.
        "var" | "let" | "const" | "abstract" | "declare" if !after_dot => {
            return Some(Spot::Nothing);
        }
        "using" => {
            return ancestors(c, file, holder)
                .any(|n| matches!(ast.node(n), Node::VarDecl { .. }))
                .then_some(Spot::Nothing);
        }
        "interface" | "enum" if !after_dot => return Some(Spot::Nothing),
        // A parameter property modifier: only the other modifiers in a constructor; nothing in
        // any other parameter list.
        "public" | "private" | "protected" | "readonly" | "override"
            if matches!(ast.node(holder), Node::Param { .. })
                || (matches!(
                    before,
                    Some("(" | "," | "public" | "private" | "protected" | "readonly" | "override")
                ) && in_constructor(c, file, holder)) =>
        {
            return Some(if in_constructor(c, file, holder) {
                Spot::Keywords(PARAMETER_KEYWORDS)
            } else {
                Spot::Nothing
            });
        }
        // `(...|` in a parameter list, `[...|` in a binding pattern.
        "." if text[..tok_at as usize].ends_with("..")
            && ancestors(c, file, holder)
                .take(3)
                .any(|n| match ast.node(n) {
                    Node::Param { .. } => true,
                    Node::Rest { .. } | Node::ArrayPattern { .. } => {
                        in_declaration_pattern(c, file, n)
                    }
                    _ => false,
                }) =>
        {
            return Some(Spot::Nothing);
        }
        // `var { x: |`: the local name of a binding property comes next.
        ":" if matches!(ast.node(holder), Node::BindingProperty { .. })
            && in_declaration_pattern(c, file, holder) =>
        {
            return Some(Spot::Nothing);
        }
        // `var a, |`: the next declarator's name (not `new Foo<A, |`, a type argument list
        // the parser cut short: more `<` than `>` before the comma).
        "," if matches!(ast.node(holder), Node::VarDecl { .. }) => {
            let head = text
                .get(ast.span(holder).start as usize..tok_at as usize)
                .unwrap_or("");
            let open = head.matches('<').count() > head.matches('>').count();
            return (!open).then_some(Spot::Nothing);
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
    // Right after a declared name (`let foo |`, `function f |`): nothing, except after a
    // class, interface or type parameter name (`extends` may follow) and after a variable's
    // name on an earlier line (a new statement starts there).
    if matches!(ast.node(holder), Node::Ident { .. })
        && ast.span(holder).start == tok_at
        && let Some(p) = parents.parent(holder)
        && super::complete::declares(ast.node(p), holder)
    {
        match ast.node(p) {
            Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Interface { .. }
            | Node::TypeParam { .. } => {}
            Node::VarDeclarator { .. } => {
                if !text[ast.span(holder).end as usize..word as usize].contains('\n') {
                    return Some(Spot::Nothing);
                }
            }
            _ => return Some(Spot::Nothing),
        }
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

/// The keyword-only spots of an import declaration (see the module docs): after `import`,
/// `type`; after a whole clause of an import with no module, `from`.
fn import_keyword_spot(c: &Checker<'_>, file: FileId, word: u32) -> Option<Spot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (tok_at, tok) = token_before(ast, text, word)?;
    let holder = node_covering(ast, tok_at)?;
    let decl = ancestors(c, file, holder).take(6).find(|&n| {
        matches!(
            ast.node(n),
            Node::ImportDecl { .. } | Node::ImportEquals { .. }
        )
    })?;
    if tok == "import" && ast.span(decl).start == tok_at {
        // `import type` is TypeScript only.
        return Some(if ast.source_kind.is_typescript() {
            Spot::Keywords(TYPE_KEYWORD)
        } else {
            Spot::Nothing
        });
    }
    let Node::ImportDecl { source, .. } = *ast.node(decl) else {
        return None;
    };
    if ast.span(source).end > ast.span(source).start {
        return None;
    }
    let parents = c.file(file).parents();
    let clause_end = tok == "}"
        || (matches!(ast.node(holder), Node::Ident { .. })
            && ast.span(holder).start == tok_at
            && parents.parent(holder).is_some_and(|p| {
                matches!(
                    *ast.node(p),
                    Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local }
                        if local == holder
                )
            }));
    clause_end.then_some(Spot::Keywords(FROM_KEYWORD))
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
