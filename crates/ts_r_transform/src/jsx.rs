//! JSX: classic (`React.createElement`), automatic (`react/jsx-runtime`) and development
//! (`react/jsx-dev-runtime`) runtimes, matching tsc's output.
//!
//! ```js
//! React.createElement("div", { className: "x", ...p }, "text", child)   // jsx: react
//! _jsxs("div", { className: "x", children: ["text", child] })            // jsx: react-jsx
//! _jsx("div", { ...p }, "key")                                           //   key → 3rd arg
//! _createElement("div", { ...p, key: "k" })                              //   key after spread
//! ```
//! JSX text is folded like tsc: lines are trimmed (except the outer edges of the first and
//! last line), empty lines dropped, the rest joined with a space; entities are decoded.

use ts_r_ast::{Atom, ImportPhase, Modifiers, Node, NodeId, NodeKind, VarKind};

use crate::builder::{Build, replace};
use crate::options::{JsxMode, TransformOptions};
use crate::scope::BindingKind;
use crate::transform::Tx;

#[derive(Default, Debug)]
pub(crate) struct State {
    pragmas_read: bool,
    /// `@jsx` / `@jsxFrag` / `@jsxImportSource` / `@jsxRuntime` pragmas.
    factory: Option<String>,
    fragment: Option<String>,
    import_source: Option<String>,
    runtime: Option<JsxMode>,
    /// Automatic runtime names in use: (imported name, local alias).
    runtime_imports: Vec<(&'static str, Atom)>,
    /// `createElement` from the import source (key after spread).
    create_element: Option<Atom>,
    /// CommonJS: the runtime module variables.
    runtime_module: Option<Atom>,
    react_module: Option<Atom>,
    file_name_var: Option<Atom>,
    /// Scope of the element being lowered (classic factory names resolve there).
    scope: Option<u32>,
}

impl State {
    pub fn factory_root<'o>(&'o self, opts: &'o TransformOptions) -> &'o str {
        let f = self.factory.as_deref().unwrap_or(&opts.jsx_factory);
        f.split('.').next().unwrap_or("")
    }

    pub fn fragment_root<'o>(&'o self, opts: &'o TransformOptions) -> &'o str {
        let f = self
            .fragment
            .as_deref()
            .unwrap_or(&opts.jsx_fragment_factory);
        f.split('.').next().unwrap_or("")
    }
}

/// Reads `/** @jsx h */`-style pragmas from the comments before the first statement.
pub(crate) fn read_pragmas(tx: &mut Tx) {
    if tx.jsx.pragmas_read {
        return;
    }
    tx.jsx.pragmas_read = true;
    let first = match tx.n(tx.ast.root) {
        Node::Program { stmts } => tx
            .ast
            .list(stmts)
            .first()
            .map_or(u32::MAX, |&s| tx.span(s).start),
        _ => u32::MAX,
    };
    let src = tx.src;
    let mut found = Vec::new();
    for c in &tx.ast.comments {
        if c.span.start >= first {
            break;
        }
        let text = &src[c.span.start as usize..c.span.end.min(src.len() as u32) as usize];
        let mut rest = text;
        while let Some(i) = rest.find('@') {
            // A pragma starts its line: only the comment's `/`, `*` and spaces before it
            // (`/** by a@b.c @jsx h */` has none, observed).
            let line = rest[..i].rsplit(['\n', '\r']).next().unwrap_or("");
            let leads = line
                .bytes()
                .all(|b| matches!(b, b'/' | b'*' | b' ' | b'\t'));
            rest = &rest[i + 1..];
            if !leads {
                continue;
            }
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let tail = &rest[name.len()..];
            let after = tail.trim_start_matches([' ', '\t']);
            if after.len() == tail.len() {
                // `@jsx: x` is not a pragma: a space must follow the name.
                continue;
            }
            let value: String = after
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '*')
                .collect();
            if value
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$' || c == '@')
            {
                found.push((name, value));
            }
        }
    }
    for (name, value) in found {
        match name.as_str() {
            "jsx" => tx.jsx.factory = Some(value),
            "jsxFrag" => tx.jsx.fragment = Some(value),
            "jsxImportSource" => tx.jsx.import_source = Some(value),
            "jsxRuntime" => {
                tx.jsx.runtime = match value.as_str() {
                    "classic" => Some(JsxMode::Classic),
                    "automatic" => Some(JsxMode::Automatic),
                    _ => None,
                }
            }
            _ => {}
        }
    }
}

/// The effective JSX mode for this file (pragmas can switch the runtime).
pub(crate) fn mode(tx: &mut Tx) -> JsxMode {
    read_pragmas(tx);
    let m = tx.opts.jsx;
    match (m, tx.jsx.runtime) {
        (JsxMode::Preserve, _) => JsxMode::Preserve,
        (JsxMode::AutomaticDev, Some(JsxMode::Automatic)) => JsxMode::AutomaticDev,
        (_, Some(r)) => r,
        (m, None) => m,
    }
}

fn import_source(tx: &Tx) -> String {
    tx.jsx
        .import_source
        .clone()
        .unwrap_or_else(|| tx.opts.jsx_import_source.clone())
}

/// Statements to put at the top of the module (runtime imports, `_jsxFileName`).
pub(crate) fn finish(tx: &mut Tx) -> Vec<NodeId> {
    let mut out = Vec::new();
    let source = import_source(tx);
    let dev = mode(tx) == JsxMode::AutomaticDev;
    let runtime_path = format!(
        "{source}/{}",
        if dev {
            "jsx-dev-runtime"
        } else {
            "jsx-runtime"
        }
    );
    let mut imports = std::mem::take(&mut tx.jsx.runtime_imports);
    if tx.opts.is_cjs() {
        if let Some(m) = tx.jsx.runtime_module {
            out.push(require_decl(tx, m, &runtime_path));
        }
        if let Some(m) = tx.jsx.react_module {
            out.push(require_decl(tx, m, &source));
        }
    } else {
        if !imports.is_empty() {
            imports.sort_by(|a, b| a.0.cmp(b.0));
            let specs: Vec<(&str, Atom)> = imports.iter().map(|&(n, a)| (n, a)).collect();
            out.push(import_decl(tx, &specs, &runtime_path));
        }
        if let Some(a) = tx.jsx.create_element {
            out.push(import_decl(tx, &[("createElement", a)], &source));
        }
    }
    if let Some(f) = tx.jsx.file_name_var {
        let name = tx.ident_atom(f, 0);
        let path = tx.opts.jsx_file_name.clone();
        let v = tx.string(&path, 0);
        out.push(tx.var(VarKind::Const, Modifiers::NONE, name, Some(v), 0));
    }
    out
}

fn import_decl(tx: &mut Tx, specs: &[(&str, Atom)], source: &str) -> NodeId {
    let mut list = Vec::new();
    for &(imported, local) in specs {
        let i = tx.ident(imported, 0);
        let l = tx.ident_atom(local, 0);
        list.push(tx.mk(
            Node::ImportSpecifier {
                type_only: false,
                imported: Some(i),
                local: l,
            },
            0,
        ));
    }
    let specifiers = tx.new_list(&list);
    let source = tx.string(source, 0);
    tx.mk(
        Node::ImportDecl {
            type_only: false,
            phase: ImportPhase::Evaluation,
            has_clause: true,
            specifiers,
            source,
            attributes: None,
        },
        0,
    )
}

fn require_decl(tx: &mut Tx, module: Atom, source: &str) -> NodeId {
    let r = tx.ident("require", 0);
    let s = tx.string(source, 0);
    let call = tx.call(r, &[s], 0);
    let name = tx.ident_atom(module, 0);
    tx.var(VarKind::Const, Modifiers::NONE, name, Some(call), 0)
}

/// A runtime function reference: `_jsx`, or `(0, jsx_runtime_1.jsx)` under CommonJS.
fn runtime_fn(tx: &mut Tx, name: &'static str, at: u32) -> NodeId {
    if tx.opts.is_cjs() {
        let m = match tx.jsx.runtime_module {
            Some(m) => m,
            None => {
                let dev = mode(tx) == JsxMode::AutomaticDev;
                let base = if dev {
                    "jsx_dev_runtime"
                } else {
                    "jsx_runtime"
                };
                let m = tx.fresh(base, true);
                tx.jsx.runtime_module = Some(m);
                m
            }
        };
        return detached_member(tx, m, name, at);
    }
    let alias = match tx.jsx.runtime_imports.iter().find(|(n, _)| *n == name) {
        Some(&(_, a)) => a,
        None => {
            let a = tx.fresh(&format!("_{name}"), false);
            tx.jsx.runtime_imports.push((name, a));
            a
        }
    };
    tx.ident_atom(alias, at)
}

/// `(0, mod.name)` for calls, `mod.name` otherwise (`Fragment`).
fn detached_member(tx: &mut Tx, module: Atom, name: &str, at: u32) -> NodeId {
    let m = tx.ident_atom(module, at);
    let member = tx.member(m, name, at);
    if name == "Fragment" {
        return member;
    }
    let zero = tx.number(0.0, at);
    let seq = tx.seq(&[zero, member], at);
    tx.paren(seq, at)
}

fn create_element_fn(tx: &mut Tx, at: u32) -> NodeId {
    if tx.opts.is_cjs() {
        let m = match tx.jsx.react_module {
            Some(m) => m,
            None => {
                let src = import_source(tx);
                let base = src.rsplit('/').next().unwrap_or("react").replace('-', "_");
                let m = tx.fresh(&base, true);
                tx.jsx.react_module = Some(m);
                m
            }
        };
        return detached_member(tx, m, "createElement", at);
    }
    let a = match tx.jsx.create_element {
        Some(a) => a,
        None => {
            let a = tx.fresh("_createElement", false);
            tx.jsx.create_element = Some(a);
            a
        }
    };
    tx.ident_atom(a, at)
}

/// A dotted entity name (`React.createElement`), resolving the root through CommonJS imports.
fn entity(tx: &mut Tx, path: &str, at: u32) -> NodeId {
    let mut parts = path.split('.');
    let root = parts.next().unwrap_or("");
    let mut e = root_ref(tx, root, at);
    for p in parts {
        e = tx.member(e, p, at);
    }
    e
}

fn root_ref(tx: &mut Tx, name: &str, at: u32) -> NodeId {
    // `React` exported by another block of the enclosing namespace: `y.React` (observed).
    if let Some(scope) = tx.jsx.scope
        && let Some(atom) = tx.ast.atoms.find(name)
        && let Some(b) = tx.sc.lookup(scope, atom)
    {
        let binding = tx.sc.binding(b);
        let in_ns = matches!(
            tx.sc.scopes[binding.scope as usize].kind,
            crate::scope::ScopeKind::Namespace(_)
        );
        let qualify = in_ns
            && match binding.kind {
                BindingKind::MergedNsExport => true,
                BindingKind::Var | BindingKind::Let | BindingKind::Const => binding.exported,
                _ => false,
            };
        if qualify && let Some(param) = crate::namespace::param_for_scope(tx, binding.scope) {
            let obj = tx.ident_atom(param, at);
            return tx.member(obj, name, at);
        }
    }
    let id = tx.ident(name, at);
    if tx.opts.is_cjs()
        && let Some(atom) = tx.ast.atoms.find(name)
        && let Some(b) = tx.sc.module_binding(atom)
    {
        crate::cjs::rewrite_ref(tx, id, b);
        tx.cjs.detached.remove(&id);
    }
    id
}

/// Transforms a `JsxElement`/`JsxFragment` in place.
pub(crate) fn jsx(tx: &mut Tx, id: NodeId) {
    let m = mode(tx);
    if m == JsxMode::Preserve {
        preserve(tx, id);
        return;
    }
    let at = tx.at(id);
    let (tag, attrs, children) = match tx.n(id) {
        Node::JsxElement {
            opening, children, ..
        } => match tx.n(opening) {
            Node::JsxOpening { name, attrs, .. } => {
                (Some(name), tx.items(attrs), tx.items(children))
            }
            _ => return,
        },
        Node::JsxFragment { children } => (None, Vec::new(), tx.items(children)),
        _ => return,
    };
    let kids = children_exprs(tx, &children);
    tx.jsx.scope = tx.sc.jsx_scope.get(&id).copied();
    let call = match m {
        JsxMode::Classic => classic(tx, tag, &attrs, kids, at),
        _ => automatic(tx, tag, &attrs, kids, at, m == JsxMode::AutomaticDev),
    };
    replace(tx.ast, id, call);
}

/// `jsx: preserve`: only the embedded expressions are transformed.
fn preserve(tx: &mut Tx, id: NodeId) {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match tx.n(n) {
            Node::JsxElement {
                opening, children, ..
            } => {
                stack.push(opening);
                stack.extend(tx.items(children));
            }
            Node::JsxFragment { children } => stack.extend(tx.items(children)),
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                self_closing,
            } => {
                tx.erase_opt(type_args);
                tx.set(
                    n,
                    Node::JsxOpening {
                        name,
                        type_args: None,
                        attrs,
                        self_closing,
                    },
                );
                stack.extend(tx.items(attrs));
            }
            Node::JsxAttr { value: Some(v), .. } => stack.push(v),
            Node::JsxSpreadAttr { arg } => tx.expr(arg),
            Node::JsxExprContainer { expr: Some(e) } => tx.expr(e),
            Node::JsxSpreadChild { expr } => tx.expr(expr),
            _ => {}
        }
    }
}

enum Child {
    Expr(NodeId),
    Spread(NodeId),
}

fn children_exprs(tx: &mut Tx, children: &[NodeId]) -> Vec<Child> {
    let mut out = Vec::new();
    for &c in children {
        match tx.n(c) {
            Node::JsxText {} => {
                let span = tx.span(c);
                let raw = &tx.src[span.start as usize..span.end as usize];
                if let Some(text) = fold_text(raw) {
                    let s = tx.string(&text, span.start);
                    out.push(Child::Expr(s));
                }
            }
            Node::JsxExprContainer { expr: Some(e) } => {
                tx.expr(e);
                out.push(Child::Expr(e));
            }
            Node::JsxExprContainer { expr: None } => {}
            Node::JsxSpreadChild { expr } => {
                tx.expr(expr);
                out.push(Child::Spread(expr));
            }
            Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                jsx(tx, c);
                out.push(Child::Expr(c));
            }
            _ => {}
        }
    }
    out
}

/// tsc's JSX whitespace rule. `None` if nothing remains.
pub(crate) fn fold_text(raw: &str) -> Option<String> {
    let mut out = String::new();
    let lines: Vec<&str> = raw.split('\n').collect();
    let n = lines.len();
    if n == 1 {
        let decoded = crate::entities::decode(raw);
        return (!decoded.is_empty()).then_some(decoded);
    }
    for (i, line) in lines.iter().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut t: &str = line;
        if i > 0 {
            t = t.trim_start_matches([' ', '\t', '\u{b}', '\u{c}', '\r']);
        }
        if i + 1 < n {
            t = t.trim_end_matches([' ', '\t', '\u{b}', '\u{c}', '\r']);
        }
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    if out.is_empty() {
        return None;
    }
    Some(crate::entities::decode(&out))
}

/// The element type: `"div"`, `Comp`, `a.b`, `"a:b"`.
fn tag_expr(tx: &mut Tx, tag: NodeId) -> NodeId {
    let at = tx.at(tag);
    match tx.n(tag) {
        Node::Ident { name } => {
            let s = tx.ast.atom(name).to_owned();
            if crate::scope::is_intrinsic_tag(&s) {
                tx.string(&s, at)
            } else {
                tx.expr(tag);
                tag
            }
        }
        Node::This {} => tag,
        Node::JsxNamespacedName { ns, name } => {
            let s = format!("{}:{}", tx.name(ns), tx.name(name));
            tx.string(&s, at)
        }
        Node::JsxMemberExpr { obj, prop } => {
            let o = tag_expr_object(tx, obj);
            let p = tx.name(prop).to_owned();
            tx.member(o, &p, at)
        }
        _ => tag,
    }
}

fn tag_expr_object(tx: &mut Tx, obj: NodeId) -> NodeId {
    let at = tx.at(obj);
    match tx.n(obj) {
        Node::Ident { .. } => {
            tx.expr(obj);
            obj
        }
        Node::JsxMemberExpr { obj: o, prop } => {
            let oo = tag_expr_object(tx, o);
            let p = tx.name(prop).to_owned();
            tx.member(oo, &p, at)
        }
        _ => obj,
    }
}

/// Property key for an attribute name.
fn attr_key(tx: &mut Tx, name: NodeId) -> NodeId {
    let at = tx.at(name);
    let s = match tx.n(name) {
        Node::JsxNamespacedName { ns, name } => format!("{}:{}", tx.name(ns), tx.name(name)),
        _ => tx.name(name).to_owned(),
    };
    if ts_r_codegen::literals::is_identifier_name(&s) {
        tx.ident(&s, at)
    } else {
        tx.string(&s, at)
    }
}

fn attr_value(tx: &mut Tx, value: Option<NodeId>, at: u32) -> NodeId {
    let Some(v) = value else {
        return tx.bool_lit(true, at);
    };
    match tx.n(v) {
        Node::StringLit { .. } => {
            let span = tx.span(v);
            let text = tx
                .src
                .get(span.start as usize..span.end as usize)
                .unwrap_or("");
            // Strip the quotes; an unterminated string (error recovery) has no closing one.
            let raw = match text.chars().next() {
                Some(q @ ('"' | '\'')) => text[1..].strip_suffix(q).unwrap_or(&text[1..]),
                _ => text,
            };
            let decoded = crate::entities::decode(raw);
            tx.string(&decoded, span.start)
        }
        Node::JsxExprContainer { expr: Some(e) } => {
            tx.expr(e);
            e
        }
        Node::JsxExprContainer { expr: None } => tx.bool_lit(true, at),
        Node::JsxElement { .. } | Node::JsxFragment { .. } => {
            jsx(tx, v);
            v
        }
        _ => {
            tx.expr(v);
            v
        }
    }
}

fn classic(
    tx: &mut Tx,
    tag: Option<NodeId>,
    attrs: &[NodeId],
    kids: Vec<Child>,
    at: u32,
) -> NodeId {
    let factory = tx
        .jsx
        .factory
        .clone()
        .unwrap_or_else(|| tx.opts.jsx_factory.clone());
    let callee = entity(tx, &factory, at);
    let ty = match tag {
        Some(t) => tag_expr(tx, t),
        None => {
            let frag = tx
                .jsx
                .fragment
                .clone()
                .unwrap_or_else(|| tx.opts.jsx_fragment_factory.clone());
            entity(tx, &frag, at)
        }
    };
    let props = if attrs.is_empty() {
        tx.null(at)
    } else {
        let mut ps = Vec::new();
        for &a in attrs {
            attr_props(tx, a, &mut ps);
        }
        props_object(tx, &ps, at)
    };
    let mut args = vec![ty, props];
    for k in kids {
        args.push(match k {
            Child::Expr(e) => e,
            Child::Spread(e) => {
                let sat = tx.at(e);
                tx.spread(e, sat)
            }
        });
    }
    tx.call(callee, &args, at)
}

/// The props object: `{ ...a }` from ES2018, else [`spread_props`] when there are spreads.
fn props_object(tx: &mut Tx, ps: &[NodeId], at: u32) -> NodeId {
    let spreads = ps.iter().any(|&p| matches!(tx.n(p), Node::Spread { .. }));
    if spreads && tx.opts.target < ts_r_config::ScriptTarget::ES2018 {
        spread_props(tx, ps, at)
    } else {
        tx.object(ps, at)
    }
}

/// Props with spreads below ES2018: one `Object.assign({ a }, b, { c }, d)` call, runs of
/// members as object literals, a leading spread after `{}` (observed).
fn spread_props(tx: &mut Tx, ps: &[NodeId], at: u32) -> NodeId {
    let mut args = Vec::new();
    let mut run = Vec::new();
    for &p in ps {
        if let Node::Spread { arg } = tx.n(p) {
            if !run.is_empty() || args.is_empty() {
                args.push(tx.object(&run, at));
                run.clear();
            }
            args.push(arg);
        } else {
            run.push(p);
        }
    }
    if !run.is_empty() {
        args.push(tx.object(&run, at));
    }
    let object = tx.ident("Object", at);
    let assign = tx.member(object, "assign", at);
    tx.call(assign, &args, at)
}

/// Pushes the object members for an attribute. A spread of an object literal is flattened
/// into its members, as tsc does.
fn attr_props(tx: &mut Tx, a: NodeId, out: &mut Vec<NodeId>) {
    let aat = tx.at(a);
    match tx.n(a) {
        Node::JsxSpreadAttr { arg } => {
            tx.expr(arg);
            let mut inner = arg;
            while let Node::Paren { expr } = tx.n(inner) {
                inner = expr;
            }
            if let Node::ObjectLit { props } = tx.n(inner)
                && !sets_proto(tx, props)
            {
                out.extend(tx.items(props));
                return;
            }
            out.push(tx.spread(arg, aat));
        }
        Node::JsxAttr { name, value } => {
            let k = attr_key(tx, name);
            let v = attr_value(tx, value, aat);
            out.push(tx.prop_node(k, v, aat));
        }
        _ => out.push(a),
    }
}

/// The literal sets its prototype (`{ __proto__: x }` or `{ "__proto__": x }`, not computed
/// or shorthand): flattening it would make `__proto__` an own property, so tsc keeps the
/// spread.
fn sets_proto(tx: &Tx, props: ts_r_ast::NodeList) -> bool {
    tx.items(props).into_iter().any(|p| match tx.n(p) {
        Node::Property { key, .. } => match tx.n(key) {
            Node::Ident { .. } | Node::StringLit { .. } => tx.name(key) == "__proto__",
            _ => false,
        },
        _ => false,
    })
}

fn automatic(
    tx: &mut Tx,
    tag: Option<NodeId>,
    attrs: &[NodeId],
    kids: Vec<Child>,
    at: u32,
    dev: bool,
) -> NodeId {
    // `key` after a spread attribute: createElement semantics.
    let mut seen_spread = false;
    let mut key_after_spread = false;
    for &a in attrs {
        match tx.n(a) {
            Node::JsxSpreadAttr { .. } => seen_spread = true,
            Node::JsxAttr { name, .. } if tx.name(name) == "key" && seen_spread => {
                key_after_spread = true;
            }
            _ => {}
        }
    }
    if key_after_spread {
        let callee = create_element_fn(tx, at);
        let ty = match tag {
            Some(t) => tag_expr(tx, t),
            None => runtime_fn(tx, "Fragment", at),
        };
        let mut ps = Vec::new();
        for &a in attrs {
            attr_props(tx, a, &mut ps);
        }
        let props = props_object(tx, &ps, at);
        let mut args = vec![ty, props];
        for k in kids {
            args.push(match k {
                Child::Expr(e) => e,
                Child::Spread(e) => {
                    let sat = tx.at(e);
                    tx.spread(e, sat)
                }
            });
        }
        return tx.call(callee, &args, at);
    }
    let ty = match tag {
        Some(t) => tag_expr(tx, t),
        None => runtime_fn(tx, "Fragment", at),
    };
    let mut ps = Vec::new();
    let mut key = None;
    for &a in attrs {
        if let Node::JsxAttr { name, value } = tx.n(a)
            && tx.name(name) == "key"
        {
            let aat = tx.at(a);
            key = Some(attr_value(tx, value, aat));
            continue;
        }
        attr_props(tx, a, &mut ps);
    }
    let is_static = kids.len() > 1 || kids.iter().any(|k| matches!(k, Child::Spread(_)));
    match kids.len() {
        0 => {}
        1 if !is_static => {
            let Some(Child::Expr(e)) = kids.into_iter().next() else {
                unreachable!()
            };
            let k = tx.ident("children", at);
            ps.push(tx.prop_node(k, e, at));
        }
        _ => {
            let mut elems = Vec::new();
            for c in kids {
                elems.push(match c {
                    Child::Expr(e) => e,
                    Child::Spread(e) => {
                        let sat = tx.at(e);
                        tx.spread(e, sat)
                    }
                });
            }
            let arr = tx.array(&elems, at);
            let k = tx.ident("children", at);
            ps.push(tx.prop_node(k, arr, at));
        }
    }
    let props = props_object(tx, &ps, at);
    if dev {
        let callee = runtime_fn(tx, "jsxDEV", at);
        let key = match key {
            Some(k) => k,
            None => tx.void0(at),
        };
        let st = tx.bool_lit(is_static, at);
        let file = match tx.jsx.file_name_var {
            Some(f) => f,
            None => {
                let f = tx.fresh("_jsxFileName", false);
                tx.jsx.file_name_var = Some(f);
                f
            }
        };
        let (line, col) = line_col(tx.src, at);
        let fv = tx.ident_atom(file, at);
        let p1 = tx.prop("fileName", fv, at);
        let l = tx.number(line as f64, at);
        let p2 = tx.prop("lineNumber", l, at);
        let c = tx.number(col as f64, at);
        let p3 = tx.prop("columnNumber", c, at);
        let src_obj = tx.object(&[p1, p2, p3], at);
        let this = tx.this(at);
        return tx.call(callee, &[ty, props, key, st, src_obj, this], at);
    }
    let callee = runtime_fn(tx, if is_static { "jsxs" } else { "jsx" }, at);
    let mut args = vec![ty, props];
    if let Some(k) = key {
        args.push(k);
    }
    tx.call(callee, &args, at)
}

/// 1-based line and column (UTF-16 units) of a source offset.
fn line_col(src: &str, pos: u32) -> (u32, u32) {
    let before = &src[..(pos as usize).min(src.len())];
    let line = before.matches('\n').count() as u32 + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = before[start..].encode_utf16().count() as u32 + 1;
    (line, col)
}

/// True for nodes that are JSX syntax.
#[allow(dead_code)]
pub(crate) fn is_jsx(k: NodeKind) -> bool {
    matches!(k, NodeKind::JsxElement | NodeKind::JsxFragment)
}

#[cfg(test)]
mod tests {
    use super::fold_text;

    #[test]
    fn folding() {
        assert_eq!(fold_text("  hello  ").as_deref(), Some("  hello  "));
        assert_eq!(fold_text("\n   \n  ").as_deref(), None);
        assert_eq!(
            fold_text("  a \n   b  \n  c  ").as_deref(),
            Some("  a b c  ")
        );
        assert_eq!(fold_text("x &amp; y").as_deref(), Some("x & y"));
        assert_eq!(fold_text("\n  text\n").as_deref(), Some("text"));
    }

    /// An incomplete edit leaves an unterminated attribute string in the recovered tree.
    #[test]
    fn unterminated_attribute_strings() {
        let opts = crate::TransformOptions::from_json(r#"{"jsx":"react"}"#, "a.tsx");
        let codegen = ts_r_codegen::CodegenOptions::default();
        for src in ["const x = <div a=\"", "const x = <div a='"] {
            let out = crate::transpile("a.tsx", src, &opts, &codegen);
            assert!(!out.diagnostics.is_empty(), "{src}");
        }
        // Like tsc, the text after the quote is the value.
        let out = crate::transpile("a.tsx", "const x = <div a=\"\n", &opts, &codegen);
        assert!(out.code.contains(r#"{ a: "\n" }"#), "{}", out.code);
    }
}
