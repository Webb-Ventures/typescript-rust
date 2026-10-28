//! `func-names`: named function expressions (`always`, `as-needed`, `never`, and a
//! `generators` override).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "func-names",
        "Require or disallow named `function` expressions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<FuncNames>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    Always,
    AsNeeded,
    Never,
}

fn mode(v: Option<&str>) -> Option<Mode> {
    match v? {
        "always" => Some(Mode::Always),
        "as-needed" => Some(Mode::AsNeeded),
        "never" => Some(Mode::Never),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct FuncNames {
    mode: Mode,
    generators: Option<Mode>,
}

impl FromOptions for FuncNames {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(FuncNames {
            mode: mode(opts.first().and_then(Value::as_str)).unwrap_or_default(),
            generators: mode(str_field(obj_opt(opts, 1), "generators")),
        })
    }
}

fn is_method(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    cx.parent(f)
        .is_some_and(|p| matches!(cx.node(p), Node::MethodDef { .. }))
}

/// The function gets a name from where it is (`const a = function () {}`, `{ a: .. }`, ...).
fn has_inferred_name(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    if is_method(cx, f) {
        return true;
    }
    let Some(p) = cx.parent(f) else {
        return false;
    };
    let is_ident = |n: NodeId| matches!(ast.node(n), Node::Ident { .. });
    match *ast.node(p) {
        Node::VarDeclarator { name, init, .. } => init == Some(f) && is_ident(name),
        Node::Property { value, .. } => value == f,
        Node::PropertyDef { value, .. } => value == Some(f),
        Node::Assign {
            op: AssignOp::Assign,
            left,
            right,
        } => right == f && is_ident(left),
        Node::AssignPattern { left, right } => right == f && is_ident(left),
        Node::Param { pattern, init, .. } => init == Some(f) && is_ident(pattern),
        Node::ShorthandProp { init, .. } => init == Some(f),
        _ => false,
    }
}

/// ESLint's `getFunctionNameWithKind`: like [`function_name_with_kind`], without the name of
/// a variable or assignment target the function is stored in.
pub(crate) fn own_name_with_kind(cx: &Ctx<'_, '_>, f: NodeId) -> String {
    let mut kind = function_name_with_kind(cx.ast(), cx.src(), cx.file.parents, f);
    let own = match *cx.node(f) {
        Node::FunctionDecl { name, .. } | Node::FunctionExpr { name, .. } => name.is_some(),
        _ => false,
    };
    if !own
        && cx
            .parent(f)
            .is_some_and(|p| matches!(cx.node(p), Node::VarDeclarator { .. } | Node::Assign { .. }))
        && let Some(i) = kind.rfind(" '")
    {
        kind.truncate(i);
    }
    kind
}

impl Rule for FuncNames {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr, NodeKind::FunctionDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (flags, name, decl) = match *cx.node(node) {
            Node::FunctionExpr { flags, name, .. } => (flags, name, false),
            Node::FunctionDecl {
                flags,
                name,
                modifiers,
                body: Some(_),
                ..
            } if modifiers.contains(Modifiers::DEFAULT) => (flags, name, true),
            _ => return,
        };
        if let Some(n) = name
            && !decl
        {
            let sem = cx.semantic();
            if sem
                .symbol_of_decl(n)
                .is_some_and(|s| !sem.symbol(s).refs.is_empty())
            {
                return;
            }
        }
        let mode = match self.generators {
            Some(g) if flags.contains(FnFlags::GENERATOR) => g,
            _ => self.mode,
        };
        let named = match mode {
            Mode::Never if name.is_some() && !decl => true,
            Mode::AsNeeded if name.is_none() && !has_inferred_name(cx, node) => false,
            Mode::Always if name.is_none() && !is_method(cx, node) => false,
            _ => return,
        };
        let ast = cx.ast();
        let src = cx.src();
        let parents = cx.file.parents;
        let kind = own_name_with_kind(cx, node);
        let head = function_head_span(ast, src, parents, node);
        if named {
            cx.report(head, format!("Unexpected named {kind}."))
                .id("named");
        } else {
            cx.report(head, format!("Unexpected unnamed {kind}."))
                .id("unnamed");
        }
    }
}
