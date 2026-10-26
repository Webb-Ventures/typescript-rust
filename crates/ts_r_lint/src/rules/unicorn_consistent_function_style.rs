//! `unicorn/consistent-function-style`: one function syntax (declaration, function
//! expression, arrow, method) per role. A function gets the first role that fits:
//! `defaultExport` (only when that option is given), `typedVariables` (an annotated
//! `const x: T = ...`), `reassignedVariables`, `namedExports`, `callbacks` (an argument),
//! `objectProperties`, `namedFunctions` (a declaration or a variable's initializer), and
//! `default` for everything else. Every role defaults to `ignore`.
//!
//! Getters/setters, class members, parameter and destructuring defaults, IIFEs, bodiless
//! declarations and (without the option) anonymous default exports have no role. Generators
//! are never asked to be arrows. A callback passed to a call may get a suggestion between
//! function expression and arrow when nothing in it depends on the function kind (`this`,
//! `super`, `arguments`, `new.target`, direct `eval`), it has no name, type parameters,
//! `this` parameter or comments, and is not a generator.

use super::prelude::*;
use ts_r_ast::{Ast, MetaPropKind, MethodKind, Modifiers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-function-style",
        "Enforce function syntax by role.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ConsistentFunctionStyle>,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Declaration,
    FunctionExpression,
    Arrow,
    Method,
    Ignore,
}

impl Style {
    fn parse(s: &str) -> Option<Style> {
        Some(match s {
            "declaration" => Style::Declaration,
            "function-expression" => Style::FunctionExpression,
            "arrow-function" => Style::Arrow,
            "method" => Style::Method,
            "ignore" => Style::Ignore,
            _ => return None,
        })
    }

    fn describe(self) -> &'static str {
        match self {
            Style::Declaration => "a function declaration",
            Style::FunctionExpression => "a function expression",
            Style::Arrow => "an arrow function",
            Style::Method => "a method",
            Style::Ignore => "",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Default,
    DefaultExport,
    NamedFunctions,
    NamedExports,
    Callbacks,
    ObjectProperties,
    ReassignedVariables,
    TypedVariables,
}

impl Role {
    const ALL: [(Role, &'static str, &'static str); 8] = [
        (Role::Default, "default", "function"),
        (Role::DefaultExport, "defaultExport", "default export"),
        (Role::NamedFunctions, "namedFunctions", "named function"),
        (Role::NamedExports, "namedExports", "named export"),
        (Role::Callbacks, "callbacks", "callback"),
        (
            Role::ObjectProperties,
            "objectProperties",
            "object property",
        ),
        (
            Role::ReassignedVariables,
            "reassignedVariables",
            "reassigned variable",
        ),
        (Role::TypedVariables, "typedVariables", "typed variable"),
    ];

    fn index(self) -> usize {
        Role::ALL.iter().position(|r| r.0 == self).unwrap_or(0)
    }

    fn describe(self) -> &'static str {
        Role::ALL[self.index()].2
    }
}

#[derive(Debug)]
struct ConsistentFunctionStyle {
    styles: [Style; 8],
    /// `defaultExport` was given (an explicit `ignore` included).
    default_export_set: bool,
}

impl FromOptions for ConsistentFunctionStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut styles = [Style::Ignore; 8];
        let mut default_export_set = false;
        for (i, (role, key, _)) in Role::ALL.iter().enumerate() {
            if let Some(v) = o.get(*key) {
                let s = v
                    .as_str()
                    .and_then(Style::parse)
                    .ok_or_else(|| format!("invalid style for `{key}`"))?;
                styles[i] = s;
                if *role == Role::DefaultExport {
                    default_export_set = true;
                }
            }
        }
        Ok(ConsistentFunctionStyle {
            styles,
            default_export_set,
        })
    }
}

/// Where a function expression sits, looking through parentheses (ESTree has none).
#[derive(Clone, Copy)]
enum Slot {
    /// The initializer of `name` in a declarator with an identifier name.
    Variable {
        declarator: NodeId,
        name: NodeId,
    },
    Argument {
        call: bool,
    },
    Callee,
    ObjectValue,
    ObjectMethod,
    Accessor,
    ClassMember,
    DefaultValue,
    DefaultExport,
    Other,
}

fn slot(cx: &Ctx<'_, '_>, node: NodeId) -> Slot {
    let ast = cx.ast();
    let mut cur = node;
    let mut parent = None;
    for _ in 0..1000 {
        match cx.parent(cur) {
            Some(p) if matches!(ast.node(p), Node::Paren { .. }) => cur = p,
            p => {
                parent = p;
                break;
            }
        }
    }
    let Some(p) = parent else {
        return Slot::Other;
    };
    match *ast.node(p) {
        Node::VarDeclarator { name, init, .. }
            if init == Some(cur) && matches!(ast.node(name), Node::Ident { .. }) =>
        {
            Slot::Variable {
                declarator: p,
                name,
            }
        }
        Node::Call { callee, .. } | Node::New { callee, .. } if callee == cur => Slot::Callee,
        Node::Call { args, .. } if ast.list(args).contains(&cur) => Slot::Argument { call: true },
        Node::New { args, .. } if ast.list(args).contains(&cur) => Slot::Argument { call: false },
        Node::Property { value, .. }
            if value == cur
                && cx
                    .parent(p)
                    .is_some_and(|o| matches!(ast.node(o), Node::ObjectLit { .. })) =>
        {
            Slot::ObjectValue
        }
        Node::MethodDef { kind, value, .. } if value == cur => {
            let in_object = cx
                .parent(p)
                .is_some_and(|o| matches!(ast.node(o), Node::ObjectLit { .. }));
            match (in_object, kind) {
                (true, MethodKind::Method) => Slot::ObjectMethod,
                (true, _) => Slot::Accessor,
                (false, _) => Slot::ClassMember,
            }
        }
        Node::PropertyDef { value, .. } if value == Some(cur) => Slot::ClassMember,
        Node::Param { init, .. }
        | Node::BindingProperty { init, .. }
        | Node::ShorthandProp { init, .. }
            if init == Some(cur) =>
        {
            Slot::DefaultValue
        }
        Node::AssignPattern { right, .. } if right == cur => Slot::DefaultValue,
        Node::ExportDefaultExpr { expr } if expr == cur => Slot::DefaultExport,
        _ => Slot::Other,
    }
}

fn is_reassigned(cx: &Ctx<'_, '_>, name: NodeId) -> bool {
    let sem = cx.semantic();
    sem.symbol_of_decl(name).is_some_and(|sym| {
        sem.symbol(sym).refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT)
        })
    })
}

fn is_exported_var(cx: &Ctx<'_, '_>, declarator: NodeId) -> bool {
    cx.parent(declarator).is_some_and(|d| {
        matches!(*cx.node(d), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
    })
}

/// `this`, `super`, `arguments`, `new.target` or a direct `eval` anywhere inside.
fn has_unsafe_reference(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    let unsafe_here = match *ast.node(id) {
        Node::This {} | Node::Super {} => true,
        Node::MetaProperty {
            kind: MetaPropKind::NewTarget,
        } => true,
        Node::Ident { .. } => ident_name(ast, id) == Some("arguments"),
        Node::Call {
            callee,
            optional: false,
            ..
        } => ident_name(ast, callee) == Some("eval"),
        _ => false,
    };
    unsafe_here
        || ast
            .children(id)
            .into_iter()
            .any(|c| has_unsafe_reference(ast, c, depth + 1))
}

/// The span ESTree gives a declaration: without `export` / `export default`.
fn declaration_span(cx: &Ctx<'_, '_>, node: NodeId) -> Span {
    let span = cx.span(node);
    let src = cx.src();
    let mut start = span.start as usize;
    for word in ["export", "default"] {
        let rest = &src[start..span.end as usize];
        if let Some(after) = rest.strip_prefix(word)
            && after.starts_with(|c: char| !is_ident_char(c))
        {
            start = skip_trivia_forward(src, start + word.len());
        }
    }
    Span::new(start as u32, span.end)
}

/// The return type annotation with its colon (`: number`).
fn return_type_text(cx: &Ctx<'_, '_>, ty: Option<NodeId>) -> String {
    let Some(ty) = ty else {
        return String::new();
    };
    let src = cx.src();
    let span = cx.span(ty);
    let before = &src[..span.start as usize];
    match before.trim_end().strip_suffix(':') {
        Some(head) => src[head.len()..span.end as usize].to_owned(),
        None => format!(": {}", cx.text(ty)),
    }
}

struct Parts<'a> {
    flags: FnFlags,
    name: Option<NodeId>,
    type_params: Option<NodeId>,
    params: &'a [NodeId],
    return_type: Option<NodeId>,
    body: Option<NodeId>,
}

fn parts(ast: &Ast, node: NodeId) -> Option<Parts<'_>> {
    Some(match *ast.node(node) {
        Node::FunctionDecl {
            flags,
            name,
            type_params,
            params,
            return_type,
            body,
            ..
        }
        | Node::FunctionExpr {
            flags,
            name,
            type_params,
            params,
            return_type,
            body,
            ..
        } => Parts {
            flags,
            name,
            type_params,
            params: ast.list(params),
            return_type,
            body,
        },
        Node::Arrow {
            flags,
            type_params,
            params,
            return_type,
            body,
        } => Parts {
            flags,
            name: None,
            type_params,
            params: ast.list(params),
            return_type,
            body: Some(body),
        },
        _ => return None,
    })
}

fn suggestion(cx: &Ctx<'_, '_>, node: NodeId, p: &Parts<'_>, to: Style) -> Option<String> {
    let ast = cx.ast();
    let body = p.body?;
    if p.name.is_some()
        || p.flags.contains(FnFlags::GENERATOR)
        || p.type_params.is_some()
        || !ast.comments_in(cx.span(node)).is_empty()
    {
        return None;
    }
    let this_param = p.params.iter().any(|&q| {
        matches!(*ast.node(q), Node::Param { pattern, .. } if matches!(ast.node(pattern), Node::This {})
            || ident_name(ast, pattern) == Some("this"))
    });
    if this_param
        || p.params.iter().any(|&q| has_unsafe_reference(ast, q, 0))
        || has_unsafe_reference(ast, body, 0)
    {
        return None;
    }
    let async_kw = if p.flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    let params = p
        .params
        .iter()
        .map(|&q| cx.text(q))
        .collect::<Vec<_>>()
        .join(", ");
    let ret = return_type_text(cx, p.return_type);
    Some(if to == Style::Arrow {
        format!("{async_kw}({params}){ret} => {}", cx.text(body))
    } else {
        let body_text = if matches!(ast.node(body), Node::Block { .. }) {
            cx.text(body).to_owned()
        } else {
            format!("{{ return {}; }}", cx.text(body))
        };
        format!("{async_kw}function ({params}){ret} {body_text}")
    })
}

impl Rule for ConsistentFunctionStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(p) = parts(ast, node) else {
            return;
        };
        if p.body.is_none() {
            return;
        }
        let (actual, role, span) = match *ast.node(node) {
            Node::FunctionDecl { modifiers, .. } => {
                let export = modifiers.contains(Modifiers::EXPORT);
                let default = export && modifiers.contains(Modifiers::DEFAULT);
                if default && p.name.is_none() && !self.default_export_set {
                    return;
                }
                let role = if default && self.default_export_set {
                    Role::DefaultExport
                } else if export && !default {
                    Role::NamedExports
                } else {
                    Role::NamedFunctions
                };
                (Style::Declaration, role, declaration_span(cx, node))
            }
            _ => {
                let arrow = matches!(ast.node(node), Node::Arrow { .. });
                let s = slot(cx, node);
                let actual = match (arrow, s) {
                    (true, _) => Style::Arrow,
                    (false, Slot::ObjectMethod) => Style::Method,
                    (false, _) => Style::FunctionExpression,
                };
                let role = match s {
                    Slot::Accessor | Slot::ClassMember | Slot::DefaultValue | Slot::Callee => {
                        return;
                    }
                    Slot::DefaultExport if self.default_export_set => Role::DefaultExport,
                    Slot::DefaultExport if p.name.is_none() => return,
                    Slot::Variable { declarator, name } => {
                        let typed = matches!(
                            *ast.node(declarator),
                            Node::VarDeclarator {
                                type_ann: Some(_),
                                ..
                            }
                        );
                        if typed {
                            Role::TypedVariables
                        } else if is_reassigned(cx, name) {
                            Role::ReassignedVariables
                        } else if is_exported_var(cx, declarator) {
                            Role::NamedExports
                        } else {
                            Role::NamedFunctions
                        }
                    }
                    Slot::Argument { .. } => Role::Callbacks,
                    Slot::ObjectValue | Slot::ObjectMethod => Role::ObjectProperties,
                    Slot::DefaultExport | Slot::Other => Role::Default,
                };
                (actual, role, cx.span(node))
            }
        };
        let expected = self.styles[role.index()];
        if expected == Style::Ignore
            || expected == actual
            || (p.flags.contains(FnFlags::GENERATOR) && expected == Style::Arrow)
        {
            return;
        }
        let msg = format!(
            "Expected {} for this {}, but found {}.",
            expected.describe(),
            role.describe(),
            actual.describe()
        );
        let fix = (role == Role::Callbacks
            && matches!(slot(cx, node), Slot::Argument { call: true })
            && matches!(
                (actual, expected),
                (Style::FunctionExpression, Style::Arrow)
                    | (Style::Arrow, Style::FunctionExpression)
            ))
        .then(|| suggestion(cx, node, &p, expected))
        .flatten();
        let node_span = cx.span(node);
        let r = cx.report(span, msg).id("consistent-function-style");
        if let Some(text) = fix {
            r.suggest(
                "consistent-function-style/suggestion",
                format!("Replace with {}.", expected.describe()),
                Fix::replace(node_span, text),
            );
        }
    }
}
