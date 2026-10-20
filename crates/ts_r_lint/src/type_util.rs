//! Helpers shared by type-aware rules, over [`TypeInfo`].
//!
//! The one rule every helper keeps: a type our checker could not compute
//! ([`Ty::UNRESOLVED`]) is never treated as `any`, `unknown` or anything else a rule could
//! report; questions about it answer "don't know" so rules stay silent.

use ts_r_ast::{Ast, Node, NodeId};

use crate::rule::Ctx;
use crate::types::{ObjectFlags, Sym, Ty, TypeFlags, TypeInfo};

/// What kind of `any` a type is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnyKind {
    /// `any` (written, implicit or evolving).
    Any,
    /// The error type of an unresolved name or import.
    Error,
}

/// `Some` for `any` and the error type; `None` for everything else, including types our
/// checker could not compute.
pub fn any_kind(ti: &dyn TypeInfo, t: Ty) -> Option<AnyKind> {
    if t == Ty::UNRESOLVED {
        return None;
    }
    if ti.is_error(t) {
        return Some(AnyKind::Error);
    }
    if ti.flags(t).contains(TypeFlags::ANY) {
        return Some(AnyKind::Any);
    }
    None
}

/// `any` or the error type.
pub fn is_any(ti: &dyn TypeInfo, t: Ty) -> bool {
    any_kind(ti, t).is_some()
}

/// A type our checker could not compute (or a union with such a member).
pub fn is_unresolved(ti: &dyn TypeInfo, t: Ty) -> bool {
    t == Ty::UNRESOLVED
        || (ti.flags(t).contains(TypeFlags::UNION) && ti.union_members(t).contains(&Ty::UNRESOLVED))
}

/// A type mentioning an unresolved part anywhere at the top (union/intersection members).
pub fn has_unresolved_part(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED {
        return true;
    }
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        return ti
            .union_members(t)
            .into_iter()
            .any(|m| has_unresolved_part(ti, m));
    }
    if f.contains(TypeFlags::INTERSECTION) {
        return ti
            .intersection_members(t)
            .into_iter()
            .any(|m| has_unresolved_part(ti, m));
    }
    false
}

pub fn is_unknown(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED && ti.flags(t).contains(TypeFlags::UNKNOWN)
}

/// The type at `node` with type parameters replaced by their constraints (typescript-eslint's
/// "constrained type at location").
pub fn constrained_type_at(ti: &dyn TypeInfo, node: NodeId) -> Option<Ty> {
    let t = ti.type_at(node)?;
    Some(constrained(ti, t))
}

/// `t`, or its base constraint when it is a type parameter or other instantiable type.
///
/// Like TypeScript, a type parameter constrained to `any` counts as `unknown`; so does a
/// conditional type whose constraint is `any` (typescript-eslint reports neither, observed).
pub fn constrained(ti: &dyn TypeInfo, t: Ty) -> Ty {
    let f = ti.flags(t);
    if f.intersects(TypeFlags::INSTANTIABLE) {
        let c = ti.base_constraint(t);
        if f.intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::CONDITIONAL)
            && c != Ty::UNRESOLVED
            && is_any(ti, c)
        {
            return Ty::UNKNOWN;
        }
        return c;
    }
    t
}

/// An array (or readonly array) of `any`.
pub fn is_any_array(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED && ti.is_array(t) && ti.array_element(t).is_some_and(|e| is_any(ti, e))
}

/// An array (or readonly array) of `unknown`.
pub fn is_unknown_array(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED && ti.is_array(t) && ti.array_element(t).is_some_and(|e| is_unknown(ti, e))
}

/// Passing a `sender` value where `receiver` is expected smuggles an `any` through a
/// generic: `Set<any>` to `Set<string>`, `Map<string, any[]>` to `Map<string, string[]>`.
/// Top-level `any` is the caller's business; this compares type arguments of references to
/// the same generic type, recursively.
pub fn is_unsafe_assignment(ti: &dyn TypeInfo, sender: Ty, receiver: Ty) -> bool {
    unsafe_assignment_at(ti, sender, receiver, 0)
}

fn unsafe_assignment_at(ti: &dyn TypeInfo, sender: Ty, receiver: Ty, depth: u32) -> bool {
    if depth > 16 || sender == Ty::UNRESOLVED || receiver == Ty::UNRESOLVED {
        return false;
    }
    if is_any(ti, receiver) || is_unknown(ti, receiver) {
        return false;
    }
    if depth > 0 && is_any(ti, sender) {
        return true;
    }
    if sender == receiver {
        return false;
    }
    let (Some(st), Some(rt)) = (ti.reference_target(sender), ti.reference_target(receiver)) else {
        return false;
    };
    if st != rt {
        return false;
    }
    let sa = ti.type_args(sender);
    let ra = ti.type_args(receiver);
    sa.iter()
        .zip(ra.iter())
        .any(|(&s, &r)| unsafe_assignment_at(ti, s, r, depth + 1))
}

/// Union members of `t` (just `t` when it is not a union).
pub fn parts(ti: &dyn TypeInfo, t: Ty) -> Vec<Ty> {
    ti.union_members(t)
}

/// Union members, with intersections flattened too.
pub fn intersection_parts(ti: &dyn TypeInfo, t: Ty) -> Vec<Ty> {
    ti.intersection_members(t)
}

/// True when the type's symbol is one of `names` declared in a default library file
/// (typescript-eslint's "builtin symbol like"), following the bases of classes and
/// interfaces and union/intersection members.
pub fn is_builtin_symbol_like(ti: &dyn TypeInfo, t: Ty, names: &[&str]) -> bool {
    is_builtin_like_depth(ti, t, names, 0)
}

fn is_builtin_like_depth(ti: &dyn TypeInfo, t: Ty, names: &[&str], depth: u32) -> bool {
    if depth > 16 || t == Ty::UNRESOLVED {
        return false;
    }
    let f = ti.flags(t);
    if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
        let members = if f.contains(TypeFlags::UNION) {
            ti.union_members(t)
        } else {
            ti.intersection_members(t)
        };
        return members
            .into_iter()
            .any(|m| is_builtin_like_depth(ti, m, names, depth + 1));
    }
    if f.contains(TypeFlags::TYPE_PARAMETER) {
        let c = ti.base_constraint(t);
        return c != t && is_builtin_like_depth(ti, c, names, depth + 1);
    }
    if let Some(s) = ti.type_symbol(t)
        && names.contains(&ti.symbol_name(s).as_str())
        && is_lib_symbol(ti, s)
    {
        return true;
    }
    if ti.flags(t).contains(TypeFlags::OBJECT) {
        for b in ti.base_types(t) {
            if is_builtin_like_depth(ti, b, names, depth + 1) {
                return true;
            }
        }
    }
    false
}

/// Every declaration of `s` is in a default library file.
pub fn is_lib_symbol(ti: &dyn TypeInfo, s: Sym) -> bool {
    let decls = ti.symbol_decls(s);
    !decls.is_empty() && decls.iter().all(|d| d.in_lib)
}

/// The type is a thenable as `await` treats it: some union member has a `then` method whose
/// first parameter is callable. `None` when unknown (`any`, `unknown`, or a part our checker
/// could not compute).
pub fn thenable(ti: &dyn TypeInfo, t: Ty) -> Option<bool> {
    thenable_depth(ti, t, 0)
}

/// [`thenable`] with "don't know" as `false`.
pub fn is_thenable(ti: &dyn TypeInfo, t: Ty) -> bool {
    thenable(ti, t) == Some(true)
}

fn thenable_depth(ti: &dyn TypeInfo, t: Ty, depth: u32) -> Option<bool> {
    if depth > 8 || t == Ty::UNRESOLVED {
        return None;
    }
    if ti.flags(t).contains(TypeFlags::UNION) {
        let mut unknown = false;
        for m in ti.union_members(t) {
            match thenable_depth(ti, m, depth + 1) {
                Some(true) => return Some(true),
                None => unknown = true,
                Some(false) => {}
            }
        }
        return if unknown { None } else { Some(false) };
    }
    let t = constrained(ti, t);
    if t == Ty::UNRESOLVED || is_any(ti, t) || ti.flags(t).contains(TypeFlags::UNKNOWN) {
        return None;
    }
    if ti.flags(t).contains(TypeFlags::UNION) {
        return thenable_depth(ti, t, depth + 1);
    }
    let Some(then) = ti.property_of_type(t, "then") else {
        return Some(false);
    };
    let tt = ti.type_of_symbol(then);
    if tt == Ty::UNRESOLVED {
        return None;
    }
    let mut unknown = false;
    for p in parts(ti, ti.non_nullable(tt)) {
        for sig in ti.signatures(p, false) {
            let (n, rest) = ti.param_count(sig);
            if n == 0 && !rest {
                continue;
            }
            let Some(first) = ti.param_type(sig, 0) else {
                continue;
            };
            if first == Ty::UNRESOLVED {
                unknown = true;
                continue;
            }
            for fp in parts(ti, first) {
                if fp == Ty::UNRESOLVED {
                    unknown = true;
                } else if is_any(ti, fp) || !ti.signatures(fp, false).is_empty() {
                    return Some(true);
                }
            }
        }
    }
    if unknown { None } else { Some(false) }
}

/// The `this` expression at the root of a member chain (`this.a.b` -> `this`).
pub fn this_root(ast: &Ast, mut n: NodeId) -> Option<NodeId> {
    for _ in 0..1000 {
        match *ast.node(n) {
            Node::This {} => return Some(n),
            Node::Member { obj, .. } | Node::Index { obj, .. } => n = obj,
            Node::Paren { expr } | Node::NonNull { expr } => n = expr,
            Node::Call { callee, .. } => n = callee,
            _ => return None,
        }
    }
    None
}

/// `a`/`an` for a type name in messages (`an \`any\``, `a \`Function\``).
pub fn article(word: &str) -> &'static str {
    match word.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U') => "an",
        _ => "a",
    }
}

/// The type is an object type created from a function expression or declaration.
pub fn is_function_like_object(ti: &dyn TypeInfo, t: Ty) -> bool {
    ti.object_flags(t).contains(ObjectFlags::FUNCTION_LIKE)
}

/// A type like TypeScript's global `Function`: the `Function` interface itself, or a type
/// deriving from it (an interface extending `Function`, `typeof x === 'function'` on
/// `unknown`), with no signatures of the kind used.
pub fn is_unsafe_function_type(ti: &dyn TypeInfo, t: Ty, construct: bool) -> bool {
    if t == Ty::UNRESOLVED || !ti.flags(t).contains(TypeFlags::OBJECT) {
        return false;
    }
    if !is_builtin_symbol_like(ti, t, &["Function"]) {
        return false;
    }
    ti.signatures(t, construct).is_empty()
}

/// Where a [`Specifier`] says a type or value is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecFrom {
    /// A bare name: declared anywhere.
    Any,
    /// `{ from: "file", name, path? }`: in the project's own files.
    File(Option<String>),
    /// `{ from: "lib", name }`: in a default library file.
    Lib,
    /// `{ from: "package", name, package }`: in a dependency.
    Package(String),
}

/// typescript-eslint's "type or value specifier" option format (`allowForKnownSafePromises`,
/// `allow`, ...).
#[derive(Clone, Debug)]
pub struct Specifier {
    pub names: Vec<String>,
    pub from: SpecFrom,
}

/// Parses a specifier list (`["Foo", { "from": "lib", "name": ["A", "B"] }]`); malformed
/// entries are skipped.
pub fn parse_specifiers(v: Option<&serde_json::Value>) -> Vec<Specifier> {
    let mut out = Vec::new();
    let Some(list) = v.and_then(|v| v.as_array()) else {
        return out;
    };
    for e in list {
        if let Some(n) = e.as_str() {
            out.push(Specifier {
                names: vec![n.to_owned()],
                from: SpecFrom::Any,
            });
            continue;
        }
        let Some(o) = e.as_object() else { continue };
        let names: Vec<String> = match o.get("name") {
            Some(serde_json::Value::String(n)) => vec![n.clone()],
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => continue,
        };
        let from = match o.get("from").and_then(|f| f.as_str()) {
            Some("file") => {
                SpecFrom::File(o.get("path").and_then(|p| p.as_str()).map(str::to_owned))
            }
            Some("lib") => SpecFrom::Lib,
            Some("package") => match o.get("package").and_then(|p| p.as_str()) {
                Some(p) => SpecFrom::Package(p.to_owned()),
                None => continue,
            },
            _ => continue,
        };
        out.push(Specifier { names, from });
    }
    out
}

/// The symbol `s` (named `name`) matches one of `specs`.
pub fn symbol_matches(ti: &dyn TypeInfo, s: Sym, name: &str, specs: &[Specifier]) -> bool {
    specs.iter().any(|sp| {
        if !sp.names.iter().any(|n| n == name) {
            return false;
        }
        let decls = ti.symbol_decls(s);
        match &sp.from {
            SpecFrom::Any => true,
            SpecFrom::Lib => !decls.is_empty() && decls.iter().all(|d| d.in_lib),
            SpecFrom::File(path) => decls.iter().any(|d| {
                if d.in_lib || d.in_external {
                    return false;
                }
                match path {
                    None => true,
                    Some(p) => ti.decl_path(d).is_some_and(|f| path_matches(&f, p)),
                }
            }),
            SpecFrom::Package(pkg) => decls.iter().any(|d| {
                ti.decl_path(d).is_some_and(|f| {
                    f.contains(&format!("/node_modules/{pkg}/"))
                        || f.contains(&format!("/node_modules/@types/{}/", types_name(pkg)))
                }) || in_ambient_module(ti, d, pkg)
            }),
        }
    })
}

/// The declaration `d` is inside `declare module "<pkg>" { ... }`.
fn in_ambient_module(ti: &dyn TypeInfo, d: &crate::types::Decl, pkg: &str) -> bool {
    let Some((ast, _)) = ti.decl_file(d) else {
        return false;
    };
    let at = ast.span(d.node);
    (1..=ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .any(|id| match *ast.node(id) {
            Node::Module { name, .. } => {
                let span = ast.span(id);
                span.start <= at.start
                    && at.end <= span.end
                    && matches!(*ast.node(name), Node::StringLit { value } if ast.atom(value) == pkg)
            }
            _ => false,
        })
}

/// `@scope/pkg` -> `scope__pkg` (DefinitelyTyped naming).
fn types_name(pkg: &str) -> String {
    match pkg.strip_prefix('@') {
        Some(rest) => rest.replacen('/', "__", 1),
        None => pkg.to_owned(),
    }
}

/// A specifier `path` (relative to the project) names `file` (absolute).
fn path_matches(file: &str, path: &str) -> bool {
    let p = path.trim_start_matches("./");
    file == p || file.ends_with(&format!("/{p}"))
}

/// [`type_matches`], or some base class/interface of the type matches (transitively).
pub fn type_or_base_matches(ti: &dyn TypeInfo, t: Ty, specs: &[Specifier]) -> bool {
    fn go(ti: &dyn TypeInfo, t: Ty, specs: &[Specifier], depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        type_matches(ti, t, specs)
            || ti
                .base_types(t)
                .into_iter()
                .any(|b| go(ti, b, specs, depth + 1))
    }
    !specs.is_empty() && go(ti, t, specs, 0)
}

/// `t` has `flags` itself or, for an intersection (a branded primitive), on some member.
pub fn has_flags_or_branded(ti: &dyn TypeInfo, t: Ty, flags: TypeFlags) -> bool {
    if t == Ty::UNRESOLVED {
        return false;
    }
    let f = ti.flags(t);
    if f.intersects(flags) {
        return true;
    }
    f.contains(TypeFlags::INTERSECTION)
        && ti
            .intersection_members(t)
            .into_iter()
            .any(|m| m != Ty::UNRESOLVED && ti.flags(m).intersects(flags))
}

/// The type (its alias, or its own symbol; any union member) matches one of `specs`.
pub fn type_matches(ti: &dyn TypeInfo, t: Ty, specs: &[Specifier]) -> bool {
    if specs.is_empty() || t == Ty::UNRESOLVED {
        return false;
    }
    let check = |t: Ty| {
        for s in [ti.alias_symbol(t), ti.type_symbol(t)]
            .into_iter()
            .flatten()
        {
            let name = ti.symbol_name(s);
            if symbol_matches(ti, s, &name, specs) {
                return true;
            }
        }
        false
    };
    if check(t) {
        return true;
    }
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        return ti.union_members(t).into_iter().all(check);
    }
    false
}

/// The contextual type typescript-eslint's rules see for an expression (observed): the
/// checker's contextual type for a call or `new` argument, a JSX expression or a template
/// literal part; the annotation of the variable, class property or parameter it
/// initializes (none without one); the target's type for the right side of `=`. Anywhere
/// else (object literal properties, array elements, `return`, conditionals, parentheses,
/// `as`/`satisfies`) there is none.
pub fn eslint_contextual_type(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Ty> {
    let ti = cx.types();
    let parent = cx.parent(node)?;
    match *cx.node(parent) {
        Node::Call { callee, .. } | Node::New { callee, .. } => {
            if callee == node {
                None
            } else {
                ti.contextual_type(node)
            }
        }
        Node::VarDeclarator { type_ann, init, .. } if init == Some(node) => {
            ti.type_of_type_node(type_ann?)
        }
        Node::PropertyDef {
            type_ann, value, ..
        } if value == Some(node) => ti.type_of_type_node(type_ann?),
        Node::Param { type_ann, init, .. } if init == Some(node) => ti.type_of_type_node(type_ann?),
        Node::Assign {
            left,
            op: ts_r_ast::AssignOp::Assign,
            right,
        } if right == node => ti.type_at(left),
        Node::JsxExprContainer { .. } | Node::TemplateLit { .. } => ti.contextual_type(node),
        _ => None,
    }
}

/// Every union member (at least one) is string-like: a string type, an intersection with
/// one (`string & { brand: 1 }`), or a type parameter constrained to one.
pub fn is_string_like_type(ti: &dyn TypeInfo, t: Ty) -> bool {
    fn go(ti: &dyn TypeInfo, t: Ty, depth: u32) -> bool {
        if depth > 8 || t == Ty::UNRESOLVED || is_any(ti, t) {
            return false;
        }
        let t = constrained(ti, t);
        let f = ti.flags(t);
        if f.contains(TypeFlags::UNION) {
            let ps = ti.union_members(t);
            return !ps.is_empty() && ps.into_iter().all(|p| go(ti, p, depth + 1));
        }
        if f.contains(TypeFlags::INTERSECTION) {
            return ti
                .intersection_members(t)
                .into_iter()
                .any(|p| go(ti, p, depth + 1));
        }
        f.intersects(TypeFlags::STRING_LIKE)
    }
    go(ti, t, 0)
}
