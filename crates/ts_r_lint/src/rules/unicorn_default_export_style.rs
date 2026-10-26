//! `unicorn/default-export-style`: a named default-exported function or class is declared
//! inline (`export default function f() {}`, the default) or separately
//! (`function f() {}` / `const f = () => {}` / `class C {}` followed by
//! `export default f;`), per the `functions` and `classes` options (`inline`, `separate`,
//! `ignore`). Only the first problem in the program is reported.
//!
//! Separate to inline: a function or class declaration is fixed by moving `export default`
//! onto it; a `const` arrow function gets a suggestion rewriting it as a function
//! declaration. Inline to separate: a function gets a suggestion writing a `const` arrow
//! function and `export default name;`; a class is fixed by splitting it. Nothing is
//! offered for generators, TypeScript syntax anywhere in the declaration, reassigned
//! bindings, or when a comment would be lost; the arrow conversions also need no other
//! reference to the name and no `this`, `super`, `arguments`, `new.target` or `eval()`.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_text::{paren_span, would_remove_comments};
use crate::ast_util::skip_trivia_forward;
use ts_r_ast::{Ast, FnFlags, HeritageKind, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/default-export-style",
        "Enforce consistent default export declarations.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<DefaultExportStyle>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Inline,
    Separate,
    Ignore,
}

#[derive(Debug)]
struct DefaultExportStyle {
    functions: Style,
    classes: Style,
}

fn style_of(o: &serde_json::Map<String, Value>, key: &str) -> Result<Style, String> {
    match o.get(key).and_then(Value::as_str) {
        None | Some("inline") => Ok(Style::Inline),
        Some("separate") => Ok(Style::Separate),
        Some("ignore") => Ok(Style::Ignore),
        Some(other) => Err(format!("unknown style `{other}`")),
    }
}

impl FromOptions for DefaultExportStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(Self {
            functions: style_of(o, "functions")?,
            classes: style_of(o, "classes")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Class,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Class => "class",
        }
    }
}

/// Any TypeScript-only syntax in the subtree (types, assertions, decorators, TS
/// modifiers, optional or definite markers, bodiless declarations, `implements`).
fn has_ts_syntax(ast: &Ast, root: NodeId) -> bool {
    let ts_modifiers = Modifiers::PUBLIC
        | Modifiers::PRIVATE
        | Modifiers::PROTECTED
        | Modifiers::READONLY
        | Modifiers::OVERRIDE
        | Modifiers::ABSTRACT
        | Modifiers::DECLARE;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let k = ast.kind(id);
        if crate::cfg::is_type_kind(k)
            || matches!(
                k,
                NodeKind::As
                    | NodeKind::Satisfies
                    | NodeKind::NonNull
                    | NodeKind::TypeAssertion
                    | NodeKind::Instantiation
                    | NodeKind::IndexSignature
            )
        {
            return true;
        }
        let n = ast.node(id);
        if n.modifiers().intersects(ts_modifiers) {
            return true;
        }
        let marked = match *n {
            Node::Param { optional, .. } => optional,
            Node::PropertyDef {
                optional, definite, ..
            } => optional || definite,
            Node::MethodDef { optional, .. } => optional,
            Node::VarDeclarator { definite, .. } => definite,
            Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body.is_none(),
            Node::HeritageClause { kind, .. } => kind == HeritageKind::Implements,
            _ => false,
        };
        if marked {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// `this`, `super`, `new.target`, `arguments` or a direct `eval()` anywhere inside.
pub(crate) fn has_unsafe_arrow_reference(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let unsafe_here = match *ast.node(id) {
            Node::This {} | Node::Super {} => true,
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::NewTarget,
            } => true,
            Node::Ident { name } => ast.atom(name) == "arguments",
            Node::Call {
                callee,
                optional: false,
                ..
            } => ident_name(ast, callee) == Some("eval"),
            _ => false,
        };
        if unsafe_here {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// The start of the declaration after `export default` (ESTree's `declaration`).
fn declaration_start(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    let ast = cx.ast();
    let span = cx.span(node);
    let default_end = ast
        .modifier_tokens_of(node)
        .iter()
        .filter(|t| t.flag == Modifiers::EXPORT || t.flag == Modifiers::DEFAULT)
        .map(|t| t.span.end)
        .max();
    match default_end {
        Some(end) => skip_trivia_forward(cx.src(), end as usize) as u32,
        None => span.start,
    }
}

fn comments_in(cx: &Ctx<'_, '_>, span: Span) -> usize {
    cx.ast()
        .comments_in(span)
        .iter()
        .filter(|c| c.span.end <= span.end)
        .count()
}

/// A comment starts after `span` on its last line.
fn has_trailing_comment(cx: &Ctx<'_, '_>, span: Span) -> bool {
    let lines = cx.file.lines;
    let end_line = lines.line_of(span.end);
    cx.ast()
        .comments
        .iter()
        .any(|c| c.span.start >= span.end && lines.line_of(c.span.start) == end_line)
}

/// A write to the symbol other than its declaration.
fn is_reassigned(cx: &Ctx<'_, '_>, sym: Option<SymbolId>) -> bool {
    let sem = cx.semantic();
    sym.is_some_and(|s| {
        sem.symbol(s).refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT)
        })
    })
}

/// A reference to the symbol other than its initializer and `allowed`.
fn has_other_references(cx: &Ctx<'_, '_>, sym: Option<SymbolId>, allowed: Option<NodeId>) -> bool {
    let sem = cx.semantic();
    sym.is_some_and(|s| {
        sem.symbol(s).refs.iter().any(|&r| {
            let r = sem.reference(r);
            !r.flags.contains(RefFlags::INIT) && Some(r.node) != allowed
        })
    })
}

fn params_text(cx: &Ctx<'_, '_>, params: &[NodeId]) -> String {
    params
        .iter()
        .map(|&p| cx.text(p))
        .collect::<Vec<_>>()
        .join(", ")
}

struct FunctionParts<'a> {
    flags: FnFlags,
    params: &'a [NodeId],
    body: NodeId,
}

fn function_parts<'a>(ast: &'a Ast, f: NodeId) -> Option<FunctionParts<'a>> {
    match *ast.node(f) {
        Node::FunctionDecl {
            flags,
            params,
            body: Some(body),
            ..
        }
        | Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } => Some(FunctionParts {
            flags,
            params: ast.list(params),
            body,
        }),
        Node::Arrow {
            flags,
            params,
            body,
            ..
        } => Some(FunctionParts {
            flags,
            params: ast.list(params),
            body,
        }),
        _ => None,
    }
}

/// A function declaration (spanning `func_span`, without `export default`) can become a
/// `const` arrow function.
fn can_convert_to_arrow(cx: &Ctx<'_, '_>, f: NodeId, func_span: Span) -> bool {
    let ast = cx.ast();
    let Some(parts) = function_parts(ast, f) else {
        return false;
    };
    !parts.flags.contains(FnFlags::GENERATOR)
        && !has_ts_syntax(ast, f)
        && comments_in(cx, func_span) == 0
        && !parts
            .params
            .iter()
            .any(|&p| has_unsafe_arrow_reference(ast, p))
        && !has_unsafe_arrow_reference(ast, parts.body)
}

fn separate_arrow_text(cx: &Ctx<'_, '_>, name: &str, f: NodeId) -> Option<String> {
    let parts = function_parts(cx.ast(), f)?;
    let is_async = if parts.flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    Some(format!(
        "const {name} = {is_async}({}) => {};{}export default {name};",
        params_text(cx, parts.params),
        cx.text(parts.body),
        linebreak(cx.src())
    ))
}

fn inline_function_text(cx: &Ctx<'_, '_>, name: &str, arrow: NodeId) -> Option<String> {
    let ast = cx.ast();
    let parts = function_parts(ast, arrow)?;
    let is_async = if parts.flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    let body = if ast.kind(parts.body) == NodeKind::Block {
        cx.text(parts.body).to_owned()
    } else {
        let s = paren_span(cx, parts.body);
        format!(
            "{{ return {}; }}",
            &cx.src()[s.start as usize..s.end as usize]
        )
    };
    Some(format!(
        "export default {is_async}function {name}({}) {body}",
        params_text(cx, parts.params)
    ))
}

enum Outcome {
    Fix(Fix),
    Suggest(&'static str, String, Option<Fix>),
    Plain,
}

impl DefaultExportStyle {
    fn style(&self, kind: Kind) -> Style {
        match kind {
            Kind::Function => self.functions,
            Kind::Class => self.classes,
        }
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, id: &'static str, msg: String, o: Outcome) {
        let r = cx.report(span, msg).id(id);
        match o {
            Outcome::Fix(f) => {
                r.fix(f);
            }
            Outcome::Suggest(sid, desc, Some(f)) => {
                r.suggest(sid, desc, f);
            }
            Outcome::Suggest(..) | Outcome::Plain => {}
        }
    }

    /// `export default function f() {}` / `export default class C {}`. True when reported.
    fn inline_declaration(&self, cx: &mut Ctx<'_, '_>, stmt: NodeId, kind: Kind) -> bool {
        let ast = cx.ast();
        let name = match *ast.node(stmt) {
            Node::FunctionDecl { name: Some(n), .. } | Node::ClassDecl { name: Some(n), .. } => n,
            _ => return false,
        };
        let sym = cx.semantic().symbol_of_decl(name);
        if self.style(kind) != Style::Separate || is_reassigned(cx, sym) {
            return false;
        }
        let span = cx.span(stmt);
        let decl_span = Span::new(declaration_start(cx, stmt), span.end);
        let name_text = cx.text(name).to_owned();
        if kind == Kind::Function {
            let generator = matches!(*ast.node(stmt), Node::FunctionDecl { flags, .. } if flags.contains(FnFlags::GENERATOR));
            if generator || has_ts_syntax(ast, stmt) {
                return false;
            }
            let fix = (comments_in(cx, span) <= comments_in(cx, decl_span)
                && !has_trailing_comment(cx, span)
                && !has_other_references(cx, sym, None)
                && can_convert_to_arrow(cx, stmt, decl_span))
            .then(|| separate_arrow_text(cx, &name_text, stmt))
            .flatten()
            .map(|t| Fix::replace(span, t));
            self.report(
                cx,
                span,
                "default-export-style/separate",
                "Prefer declaring this default-exported function as a separate const arrow function.".into(),
                Outcome::Suggest(
                    "default-export-style/suggestion-separate",
                    "Use a separate const arrow function default export.".into(),
                    fix,
                ),
            );
            return true;
        }
        if has_ts_syntax(ast, stmt) {
            return false;
        }
        let msg = "Prefer declaring this default-exported class as a separate class declaration.";
        let outcome = if comments_in(cx, span) > comments_in(cx, decl_span)
            || has_trailing_comment(cx, span)
        {
            Outcome::Plain
        } else {
            let class_text = &cx.src()[decl_span.start as usize..decl_span.end as usize];
            Outcome::Fix(Fix::replace(
                span,
                format!(
                    "{class_text}{}export default {name_text};",
                    linebreak(cx.src())
                ),
            ))
        };
        self.report(
            cx,
            span,
            "default-export-style/separate",
            msg.into(),
            outcome,
        );
        true
    }

    /// `decl` followed by `export default name;`. True when reported.
    fn separate_declaration(&self, cx: &mut Ctx<'_, '_>, decl: NodeId, export: NodeId) -> bool {
        let ast = cx.ast();
        let Node::ExportDefaultExpr { expr } = *ast.node(export) else {
            return false;
        };
        let exported = skip_parens(ast, expr);
        let Some(exported_name) = ident_name(ast, exported) else {
            return false;
        };
        // The declaration: its kind, name node and (for `const`) the arrow function.
        let (kind, name, arrow) = match *ast.node(decl) {
            Node::FunctionDecl {
                name: Some(n),
                modifiers,
                body,
                ..
            } if !modifiers.contains(Modifiers::EXPORT) && body.is_some() => {
                (Kind::Function, n, None)
            }
            Node::ClassDecl {
                name: Some(n),
                modifiers,
                ..
            } if !modifiers.contains(Modifiers::EXPORT) => (Kind::Class, n, None),
            Node::VarDecl {
                modifiers,
                kind: VarKind::Const,
                decls,
            } if !modifiers.intersects(Modifiers::EXPORT | Modifiers::DECLARE) => {
                let &[d] = ast.list(decls) else {
                    return false;
                };
                let Node::VarDeclarator {
                    name,
                    type_ann: None,
                    definite: false,
                    init: Some(init),
                } = *ast.node(d)
                else {
                    return false;
                };
                let init = skip_parens(ast, init);
                if !matches!(ast.node(name), Node::Ident { .. })
                    || ast.kind(init) != NodeKind::Arrow
                    || has_ts_syntax(ast, init)
                {
                    return false;
                }
                (Kind::Function, name, Some(init))
            }
            _ => return false,
        };
        if cx.text(name) != exported_name {
            return false;
        }
        let style = self.style(kind);
        let exported_sym = cx.semantic().symbol_of_ident(exported);
        if style == Style::Ignore || is_reassigned(cx, exported_sym) {
            return false;
        }
        let is_function_decl = ast.kind(decl) == NodeKind::FunctionDecl;
        if is_function_decl {
            let generator = matches!(*ast.node(decl), Node::FunctionDecl { flags, .. } if flags.contains(FnFlags::GENERATOR));
            if generator || has_ts_syntax(ast, decl) {
                return false;
            }
        }
        let decl_span = cx.span(decl);
        let export_span = cx.span(export);
        let between = Span::new(decl_span.end, export_span.end);
        let comment_safe = !would_remove_comments(cx, between, &[]);
        let sym = cx.semantic().symbol_of_decl(name);
        let kind_word = kind.word();
        if style == Style::Inline {
            if let Some(arrow) = arrow {
                let arrow_span = cx.span(arrow);
                let fix = (comment_safe
                    && comments_in(cx, export_span) == 0
                    && !has_trailing_comment(cx, export_span)
                    && comments_in(cx, decl_span) <= comments_in(cx, arrow_span)
                    && !has_other_references(cx, sym, Some(exported))
                    && !has_ts_syntax(ast, arrow)
                    && comments_in(cx, arrow_span) == 0
                    && function_parts(ast, arrow).is_some_and(|p| {
                        !p.params.iter().any(|&q| has_unsafe_arrow_reference(ast, q))
                            && !has_unsafe_arrow_reference(ast, p.body)
                    }))
                .then(|| inline_function_text(cx, exported_name, arrow))
                .flatten()
                .map(|t| Fix::replace(Span::new(decl_span.start, export_span.end), t));
                self.report(
                    cx,
                    export_span,
                    "default-export-style/inline",
                    format!("Prefer declaring this default-exported {kind_word} inline."),
                    Outcome::Suggest(
                        "default-export-style/suggestion-inline",
                        format!("Use an inline default {kind_word} declaration."),
                        fix,
                    ),
                );
                return true;
            }
            if kind == Kind::Class && has_ts_syntax(ast, decl) {
                return false;
            }
            let outcome = if !comment_safe
                || comments_in(cx, export_span) > 0
                || has_trailing_comment(cx, export_span)
            {
                Outcome::Plain
            } else {
                Outcome::Fix(
                    Fix::insert(decl_span.start, "export default ").and(between, String::new()),
                )
            };
            self.report(
                cx,
                export_span,
                "default-export-style/inline",
                format!("Prefer declaring this default-exported {kind_word} inline."),
                outcome,
            );
            return true;
        }
        if style == Style::Separate && kind == Kind::Function && is_function_decl {
            let fix = (comment_safe
                && comments_in(cx, export_span) <= comments_in(cx, decl_span)
                && !has_trailing_comment(cx, export_span)
                && !has_other_references(cx, sym, Some(exported))
                && can_convert_to_arrow(cx, decl, decl_span))
            .then(|| separate_arrow_text(cx, exported_name, decl))
            .flatten()
            .map(|t| Fix::replace(Span::new(decl_span.start, export_span.end), t));
            self.report(
                cx,
                export_span,
                "default-export-style/separate",
                "Prefer declaring this default-exported function as a separate const arrow function.".into(),
                Outcome::Suggest(
                    "default-export-style/suggestion-separate",
                    "Use a separate const arrow function default export.".into(),
                    fix,
                ),
            );
            return true;
        }
        false
    }
}

impl Rule for DefaultExportStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts } = *cx.node(node) else {
            return;
        };
        let stmts = ast.list(stmts);
        for (i, &stmt) in stmts.iter().enumerate() {
            let n = ast.node(stmt);
            let inline_kind = match n {
                Node::FunctionDecl { modifiers, .. } if modifiers.contains(Modifiers::DEFAULT) => {
                    Some(Kind::Function)
                }
                Node::ClassDecl { modifiers, .. } if modifiers.contains(Modifiers::DEFAULT) => {
                    Some(Kind::Class)
                }
                _ => None,
            };
            if n.modifiers().contains(Modifiers::DEFAULT) {
                if let Some(kind) = inline_kind
                    && self.inline_declaration(cx, stmt, kind)
                {
                    return;
                }
                continue;
            }
            if ast.kind(stmt) == NodeKind::ExportDefaultExpr {
                continue;
            }
            let Some(&next) = stmts.get(i + 1) else {
                continue;
            };
            if self.separate_declaration(cx, stmt, next) {
                return;
            }
        }
    }
}
