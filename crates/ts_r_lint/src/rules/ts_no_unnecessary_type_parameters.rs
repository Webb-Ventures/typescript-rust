//! `@typescript-eslint/no-unnecessary-type-parameters`: a type parameter of a function or
//! class signature should relate two places; one used once (or never) can be replaced by its
//! constraint.
//!
//! Uses are counted in the signature: parameter and `this` types, the return type (the
//! inferred one, from the checker, when there is no annotation), constraints and defaults
//! of the other type parameters; for classes, the member types, method signatures and
//! heritage type arguments. Bodies do not count. `typeof param` counts the parameter's type
//! again. Some places count a use twice: a type argument of a generic type other than an
//! array (`Set<T>`, `Promise<T>`), and an element of a mutable array or tuple in a return type
//! or class member (the caller can write into it). Readonly arrays and tuples count once.

use super::prelude::*;
use ts_r_ast::{Ast, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-type-parameters",
        "Disallow type parameters that aren't used multiple times",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create_default::<NoUnnecessaryTypeParameters>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryTypeParameters;

const MAX_DEPTH: u32 = 64;

/// The parts of a function-like signature.
struct SigParts {
    type_params: Option<NodeId>,
    params: Vec<NodeId>,
    return_type: Option<NodeId>,
    /// A function with a body and no return annotation: its return type is inferred.
    inferred: bool,
}

impl Rule for NoUnnecessaryTypeParameters {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::MethodSignature,
            NodeKind::CallSignature,
            NodeKind::ConstructSignature,
            NodeKind::FunctionType,
            NodeKind::ConstructorType,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (type_params, is_class) = match *cx.node(node) {
            Node::ClassDecl { type_params, .. } | Node::ClassExpr { type_params, .. } => {
                (type_params, true)
            }
            _ => match sig_parts(ast, node) {
                Some(p) => (p.type_params, false),
                None => return,
            },
        };
        let Some(tps_node) = type_params else {
            return;
        };
        let Node::TypeParams { params } = *cx.node(tps_node) else {
            return;
        };
        let tps: Vec<NodeId> = ast.list(params).to_vec();
        let mut found: Vec<(NodeId, u32)> = Vec::new();
        for (i, &tp) in tps.iter().enumerate() {
            let Node::TypeParam { name, .. } = *cx.node(tp) else {
                continue;
            };
            let Some(tp_name) = ident_name(ast, name) else {
                continue;
            };
            let mut c = Counter {
                cx,
                name: tp_name,
                target: None,
                target_node: tp,
                sig_params: Vec::new(),
            };
            let mut uses = 0;
            for (j, &other) in tps.iter().enumerate() {
                if j == i {
                    continue;
                }
                if let Node::TypeParam {
                    constraint,
                    default,
                    ..
                } = *cx.node(other)
                {
                    for n in [constraint, default].into_iter().flatten() {
                        uses += c.count(n, false, 0);
                    }
                }
            }
            uses += if is_class {
                c.class_uses(node)
            } else {
                c.function_uses(node)
            };
            if uses <= 1 {
                found.push((tp, uses));
            }
        }
        for (tp, uses) in found {
            report(cx, node, tps_node, &tps, tp, uses, is_class);
        }
    }
}

fn sig_parts(ast: &Ast, node: NodeId) -> Option<SigParts> {
    Some(match *ast.node(node) {
        Node::FunctionDecl {
            type_params,
            params,
            return_type,
            body,
            ..
        }
        | Node::FunctionExpr {
            type_params,
            params,
            return_type,
            body,
            ..
        } => SigParts {
            type_params,
            params: ast.list(params).to_vec(),
            return_type,
            inferred: return_type.is_none() && body.is_some(),
        },
        Node::Arrow {
            type_params,
            params,
            return_type,
            ..
        } => SigParts {
            type_params,
            params: ast.list(params).to_vec(),
            return_type,
            inferred: return_type.is_none(),
        },
        Node::MethodSignature {
            type_params,
            params,
            return_type,
            ..
        }
        | Node::CallSignature {
            type_params,
            params,
            return_type,
        }
        | Node::ConstructSignature {
            type_params,
            params,
            return_type,
        } => SigParts {
            type_params,
            params: ast.list(params).to_vec(),
            return_type,
            inferred: false,
        },
        Node::FunctionType {
            type_params,
            params,
            return_type,
        }
        | Node::ConstructorType {
            type_params,
            params,
            return_type,
            ..
        } => SigParts {
            type_params,
            params: ast.list(params).to_vec(),
            return_type: Some(return_type),
            inferred: false,
        },
        _ => return None,
    })
}

/// True when the type parameter list `tps` declares `name`.
fn declares(ast: &Ast, tps: Option<NodeId>, name: &str) -> bool {
    let Some(tps) = tps else {
        return false;
    };
    let Node::TypeParams { params } = *ast.node(tps) else {
        return false;
    };
    ast.list(params).iter().any(|&p| match *ast.node(p) {
        Node::TypeParam { name: n, .. } => ident_name(ast, n) == Some(name),
        _ => false,
    })
}

fn param_type_ann(ast: &Ast, p: NodeId) -> Option<NodeId> {
    match *ast.node(p) {
        Node::Param { type_ann, .. } => type_ann,
        _ => None,
    }
}

fn param_name(ast: &Ast, p: NodeId) -> Option<&str> {
    match *ast.node(p) {
        Node::Param { pattern, .. } => ident_name(ast, pattern),
        _ => None,
    }
}

struct Counter<'c, 'f, 'a> {
    cx: &'c Ctx<'f, 'a>,
    name: &'a str,
    /// The type parameter's type (looked up when an inferred type is walked).
    target: Option<Option<Ty>>,
    target_node: NodeId,
    /// Parameters of the signature being counted (for `typeof param`).
    sig_params: Vec<NodeId>,
}

impl<'a> Counter<'_, '_, 'a> {
    fn ast(&self) -> &'a Ast {
        self.cx.ast()
    }

    fn target(&mut self) -> Option<Ty> {
        if let Some(t) = self.target {
            return t;
        }
        let ti = self.cx.types();
        let Node::TypeParam { name, .. } = *self.cx.node(self.target_node) else {
            return None;
        };
        let t = ti.type_at(name);
        let t = t.filter(|&t| ti.flags(t).contains(TypeFlags::TYPE_PARAMETER));
        self.target = Some(t);
        t
    }

    fn function_uses(&mut self, node: NodeId) -> u32 {
        let Some(parts) = sig_parts(self.ast(), node) else {
            return 0;
        };
        self.signature_uses(node, &parts, false)
    }

    /// Uses in a signature: parameters (`multi` for class members), the return type.
    fn signature_uses(&mut self, node: NodeId, parts: &SigParts, multi: bool) -> u32 {
        let ast = self.ast();
        let saved = std::mem::replace(&mut self.sig_params, parts.params.clone());
        let mut uses = 0;
        for &p in &parts.params {
            if let Some(t) = param_type_ann(ast, p) {
                uses += self.count(t, multi, 0);
            }
        }
        if let Some(rt) = parts.return_type {
            uses += self.count(rt, true, 0);
        } else if parts.inferred {
            uses += self.inferred_return_uses(node);
        }
        self.sig_params = saved;
        uses
    }

    fn inferred_return_uses(&mut self, func: NodeId) -> u32 {
        let Some(target) = self.target() else {
            return 0;
        };
        let ti = self.cx.types();
        let ft = match *self.cx.node(func) {
            Node::FunctionDecl { name: Some(n), .. } => ti.type_at(n),
            _ => ti.type_at(func),
        };
        let Some(ft) = ft else {
            return 0;
        };
        let Some(&sig) = ti.signatures(ft, false).first() else {
            return 0;
        };
        let rt = ti.return_type(sig);
        let mut stack = Vec::new();
        count_type(ti, rt, target, true, 0, &mut stack)
    }

    fn class_uses(&mut self, class: NodeId) -> u32 {
        let ast = self.ast();
        let (heritage, members) = match *self.cx.node(class) {
            Node::ClassDecl {
                heritage, members, ..
            }
            | Node::ClassExpr {
                heritage, members, ..
            } => (heritage, members),
            _ => return 0,
        };
        let mut uses = 0;
        for &h in ast.list(heritage) {
            let Node::HeritageClause { types, .. } = *self.cx.node(h) else {
                continue;
            };
            for &t in ast.list(types) {
                if let Node::ExprWithTypeArgs {
                    type_args: Some(ta),
                    ..
                } = *self.cx.node(t)
                    && let Node::TypeArgs { args } = *self.cx.node(ta)
                {
                    for &a in ast.list(args) {
                        uses += 2 * self.count(a, true, 0);
                    }
                }
            }
        }
        for &m in ast.list(members) {
            match *self.cx.node(m) {
                Node::PropertyDef {
                    type_ann: Some(t), ..
                } => uses += self.count(t, true, 0),
                Node::IndexSignature {
                    params, type_ann, ..
                } => {
                    for &p in ast.list(params) {
                        if let Some(t) = param_type_ann(ast, p) {
                            uses += self.count(t, true, 0);
                        }
                    }
                    if let Some(t) = type_ann {
                        uses += self.count(t, true, 0);
                    }
                }
                Node::MethodDef { value, .. } => {
                    let Some(parts) = sig_parts(ast, value) else {
                        continue;
                    };
                    if declares(ast, parts.type_params, self.name) {
                        continue;
                    }
                    uses += self.signature_uses(value, &parts, true);
                }
                _ => {}
            }
        }
        uses
    }

    /// Uses of the type parameter in type node `n`, saturated at 2: only none, one and
    /// several matter, and nested array layers double the count past `u32` otherwise.
    fn count(&mut self, n: NodeId, multi: bool, depth: u32) -> u32 {
        self.count_uncapped(n, multi, depth).min(2)
    }

    fn count_uncapped(&mut self, n: NodeId, multi: bool, depth: u32) -> u32 {
        if depth > MAX_DEPTH {
            return 0;
        }
        let ast = self.ast();
        let d = depth + 1;
        let twice = if multi { 2 } else { 1 };
        match *ast.node(n) {
            Node::TypeRef { name, type_args } => {
                let args: Vec<NodeId> = match type_args.map(|ta| ast.node(ta)) {
                    Some(&Node::TypeArgs { args }) => ast.list(args).to_vec(),
                    _ => Vec::new(),
                };
                let ref_name = ident_name(ast, name);
                if args.is_empty() {
                    return u32::from(ref_name == Some(self.name));
                }
                let mut uses = 0;
                for a in args {
                    uses += match ref_name {
                        Some("Array") => self.count(a, multi, d) * twice,
                        Some("ReadonlyArray") => self.count(a, false, d),
                        _ => self.count(a, multi, d) * 2,
                    };
                }
                uses
            }
            Node::ArrayType { elem } => self.count(elem, multi, d) * twice,
            Node::TupleType { elems } => ast
                .list(elems)
                .iter()
                .map(|&e| self.count(e, multi, d) * twice)
                .sum(),
            Node::NamedTupleMember { ty, .. }
            | Node::OptionalType { ty }
            | Node::RestType { ty }
            | Node::ParenType { ty } => self.count(ty, multi, d),
            Node::TypeOperator {
                op: TypeOperatorKind::Readonly,
                ty,
            } => self.count(ty, false, d),
            Node::TypeOperator { ty, .. } => self.count(ty, multi, d),
            Node::UnionType { types } | Node::IntersectionType { types } => ast
                .list(types)
                .iter()
                .map(|&t| self.count(t, multi, d))
                .sum(),
            Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. } => {
                let Some(parts) = sig_parts(ast, n) else {
                    return 0;
                };
                if declares(ast, parts.type_params, self.name) {
                    return 0;
                }
                let mut uses = 0;
                for &p in &parts.params {
                    if let Some(t) = param_type_ann(ast, p) {
                        uses += self.count(t, false, d);
                    }
                }
                if let Some(rt) = parts.return_type {
                    uses += self.count(rt, multi, d);
                }
                uses
            }
            Node::TypeLiteral { members } => {
                let mut uses = 0;
                for &m in ast.list(members) {
                    uses += match *ast.node(m) {
                        Node::PropertySignature {
                            type_ann: Some(t), ..
                        } => self.count(t, multi, d),
                        Node::IndexSignature {
                            params, type_ann, ..
                        } => {
                            let mut u = 0;
                            for &p in ast.list(params) {
                                if let Some(t) = param_type_ann(ast, p) {
                                    u += self.count(t, false, d);
                                }
                            }
                            if let Some(t) = type_ann {
                                u += self.count(t, multi, d);
                            }
                            u
                        }
                        _ => self.count(m, multi, d),
                    };
                }
                uses
            }
            Node::MappedType {
                type_param,
                name_type,
                ty,
                ..
            } => {
                let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                else {
                    return 0;
                };
                if ident_name(ast, name) == Some(self.name) {
                    return 0;
                }
                [constraint, name_type, ty]
                    .into_iter()
                    .flatten()
                    .map(|t| self.count(t, multi, d))
                    .sum()
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => [check, extends, true_ty, false_ty]
                .into_iter()
                .map(|t| self.count(t, multi, d))
                .sum(),
            Node::InferType { type_param } => match *ast.node(type_param) {
                Node::TypeParam {
                    constraint: Some(c),
                    ..
                } => self.count(c, multi, d),
                _ => 0,
            },
            Node::IndexedAccessType { obj, index } => {
                self.count(obj, multi, d) + self.count(index, multi, d)
            }
            Node::TemplateLitType { parts } => ast
                .list(parts)
                .iter()
                .filter(|&&p| !matches!(ast.node(p), Node::TemplateElement { .. }))
                .map(|&p| self.count(p, multi, d))
                .sum(),
            Node::TypeQuery { expr_name, .. } => {
                let Some(x) = ident_name(ast, expr_name) else {
                    return 0;
                };
                let param = self
                    .sig_params
                    .iter()
                    .copied()
                    .find(|&p| param_name(ast, p) == Some(x));
                match param.and_then(|p| param_type_ann(ast, p)) {
                    Some(t) => self.count(t, false, d),
                    None => 0,
                }
            }
            Node::TypePredicate { ty: Some(t), .. } => self.count(t, multi, d),
            Node::ImportType {
                type_args: Some(ta),
                ..
            } => match *ast.node(ta) {
                Node::TypeArgs { args } => ast
                    .list(args)
                    .iter()
                    .map(|&a| self.count(a, multi, d) * 2)
                    .sum(),
                _ => 0,
            },
            _ => 0,
        }
    }
}

/// Uses of type parameter `target` in the (inferred) type `t`, saturated at 2 like
/// `Counter::count`.
fn count_type(
    ti: &dyn TypeInfo,
    t: Ty,
    target: Ty,
    multi: bool,
    depth: u32,
    stack: &mut Vec<Ty>,
) -> u32 {
    count_type_uncapped(ti, t, target, multi, depth, stack).min(2)
}

fn count_type_uncapped(
    ti: &dyn TypeInfo,
    t: Ty,
    target: Ty,
    multi: bool,
    depth: u32,
    stack: &mut Vec<Ty>,
) -> u32 {
    if t == target {
        return 1;
    }
    if depth > 16 || t == Ty::UNRESOLVED || stack.contains(&t) {
        return 0;
    }
    let f = ti.flags(t);
    let twice = if multi { 2 } else { 1 };
    let d = depth + 1;
    if f.contains(TypeFlags::UNION) {
        return ti
            .union_members(t)
            .into_iter()
            .map(|m| count_type(ti, m, target, multi, d, stack))
            .sum();
    }
    if f.contains(TypeFlags::INTERSECTION) {
        return ti
            .intersection_members(t)
            .into_iter()
            .map(|m| count_type(ti, m, target, multi, d, stack))
            .sum();
    }
    if !f.contains(TypeFlags::OBJECT) {
        if f.intersects(TypeFlags::TYPE_PARAMETER) {
            return 0;
        }
        return ti
            .type_parts(t)
            .into_iter()
            .map(|p| count_type(ti, p, target, multi, d, stack))
            .sum();
    }
    stack.push(t);
    let readonly = ti.object_flags(t).contains(ObjectFlags::READONLY)
        || ti
            .type_symbol(t)
            .is_some_and(|s| ti.symbol_name(s) == "ReadonlyArray");
    let uses = if ti.is_array(t) || ti.is_tuple(t) {
        let factor = if readonly { 1 } else { twice };
        let inner = multi && !readonly;
        ti.type_args(t)
            .into_iter()
            .map(|a| count_type(ti, a, target, inner, d, stack) * factor)
            .sum()
    } else if ti.reference_target(t).is_some() {
        ti.type_args(t)
            .into_iter()
            .map(|a| count_type(ti, a, target, multi, d, stack) * 2)
            .sum()
    } else if ti.object_flags(t).contains(ObjectFlags::MAPPED) {
        ti.type_parts(t)
            .into_iter()
            .map(|p| count_type(ti, p, target, multi, d, stack))
            .sum()
    } else if ti
        .object_flags(t)
        .intersects(ObjectFlags::CLASS | ObjectFlags::INTERFACE)
    {
        0
    } else {
        let mut uses = 0;
        for construct in [false, true] {
            for sig in ti.signatures(t, construct) {
                let (count, rest) = ti.param_count(sig);
                for i in 0..count {
                    if let Some(p) = ti.param_type(sig, i) {
                        uses += count_type(ti, p, target, false, d, stack);
                    }
                }
                if rest && let Some(r) = ti.rest_param_type(sig) {
                    uses += count_type(ti, r, target, false, d, stack);
                }
                uses += count_type(ti, ti.return_type(sig), target, multi, d, stack);
            }
        }
        for p in ti.properties_of(t) {
            uses += count_type(ti, ti.type_of_symbol(p), target, multi, d, stack);
        }
        for number in [false, true] {
            if let Some(it) = ti.index_type(t, number) {
                uses += count_type(ti, it, target, multi, d, stack);
            }
        }
        uses
    };
    stack.pop();
    uses
}

fn report(
    cx: &mut Ctx<'_, '_>,
    scope: NodeId,
    tps_node: NodeId,
    tps: &[NodeId],
    tp: NodeId,
    uses: u32,
    is_class: bool,
) {
    let ast = cx.ast();
    let Node::TypeParam {
        name, constraint, ..
    } = *cx.node(tp)
    else {
        return;
    };
    let Some(tp_name) = ident_name(ast, name) else {
        return;
    };
    let message = format!(
        "Type parameter {tp_name} is {} in the {} signature.",
        if uses == 0 {
            "never used"
        } else {
            "used only once"
        },
        if is_class { "class" } else { "function" }
    );
    let fix = replacement_fix(cx, scope, tps_node, tps, tp, tp_name, constraint);
    cx.report_node(tp, message).id("sole").suggest(
        "replaceUsagesWithConstraint",
        "Replace all usages of type parameter with its constraint.",
        fix,
    );
}

/// Removes the type parameter and replaces its uses with its constraint (or `unknown`).
fn replacement_fix(
    cx: &Ctx<'_, '_>,
    scope: NodeId,
    tps_node: NodeId,
    tps: &[NodeId],
    tp: NodeId,
    name: &str,
    constraint: Option<NodeId>,
) -> Fix {
    let ast = cx.ast();
    let i = tps.iter().position(|&p| p == tp).unwrap_or(0);
    let removal = if tps.len() == 1 {
        cx.span(tps_node)
    } else if i + 1 < tps.len() {
        Span::new(cx.span(tp).start, cx.span(tps[i + 1]).start)
    } else {
        Span::new(cx.span(tps[i - 1]).end, cx.span(tp).end)
    };
    let (text, prec) = match constraint {
        Some(c)
            if !matches!(
                cx.node(c),
                Node::KeywordType {
                    kind: KeywordTypeKind::Any
                }
            ) =>
        {
            (cx.text(c).to_string(), type_precedence(ast, c))
        }
        _ => ("unknown".to_string(), 5),
    };
    let mut refs = Vec::new();
    collect_refs(ast, scope, name, scope, &mut refs, 0);
    let mut fix = Fix::delete(removal);
    for r in refs {
        let span = cx.span(r);
        if span.start >= removal.start && span.end <= removal.end {
            continue;
        }
        let needed = cx.parent(r).map_or(0, |p| context_precedence(ast, p, r));
        let t = if prec < needed {
            format!("({text})")
        } else {
            text.clone()
        };
        fix = fix.and(span, t);
    }
    fix
}

/// `TypeRef`s naming `name` under `n` that refer to the type parameter declared at `scope`
/// (not shadowed by an inner declaration of the same name).
fn collect_refs(
    ast: &Ast,
    n: NodeId,
    name: &str,
    scope: NodeId,
    out: &mut Vec<NodeId>,
    depth: u32,
) {
    if depth > 512 {
        return;
    }
    let node = ast.node(n);
    if n != scope {
        let shadows = match *node {
            Node::FunctionDecl { type_params, .. }
            | Node::FunctionExpr { type_params, .. }
            | Node::Arrow { type_params, .. }
            | Node::MethodSignature { type_params, .. }
            | Node::CallSignature { type_params, .. }
            | Node::ConstructSignature { type_params, .. }
            | Node::FunctionType { type_params, .. }
            | Node::ConstructorType { type_params, .. }
            | Node::ClassDecl { type_params, .. }
            | Node::ClassExpr { type_params, .. }
            | Node::Interface { type_params, .. }
            | Node::TypeAlias { type_params, .. } => declares(ast, type_params, name),
            Node::MappedType { type_param, .. } => match *ast.node(type_param) {
                Node::TypeParam { name: n2, .. } => ident_name(ast, n2) == Some(name),
                _ => false,
            },
            _ => false,
        };
        if shadows {
            return;
        }
    }
    if let Node::TypeRef {
        name: rn,
        type_args: None,
    } = *node
        && ident_name(ast, rn) == Some(name)
    {
        out.push(n);
        return;
    }
    for c in ast.children(n) {
        collect_refs(ast, c, name, scope, out, depth + 1);
    }
}

/// How tightly a type node binds: function and conditional types 0, unions 1,
/// intersections 2, type operators 3, array and indexed access types 4, others 5.
fn type_precedence(ast: &Ast, n: NodeId) -> u8 {
    match ast.node(n) {
        Node::FunctionType { .. } | Node::ConstructorType { .. } | Node::ConditionalType { .. } => {
            0
        }
        Node::UnionType { .. } => 1,
        Node::IntersectionType { .. } => 2,
        Node::TypeOperator { .. } | Node::InferType { .. } => 3,
        Node::ArrayType { .. } | Node::IndexedAccessType { .. } => 4,
        _ => 5,
    }
}

/// The precedence a type needs to stand as child `child` of `parent` without parentheses.
fn context_precedence(ast: &Ast, parent: NodeId, child: NodeId) -> u8 {
    match *ast.node(parent) {
        Node::ArrayType { .. } | Node::OptionalType { .. } => 4,
        Node::IndexedAccessType { obj, .. } if obj == child => 4,
        Node::TypeOperator { .. } => 3,
        Node::IntersectionType { .. } => 2,
        Node::UnionType { .. } => 1,
        Node::ConditionalType { check, extends, .. } if check == child || extends == child => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deeply_nested_array_return_counts_as_several_uses() {
        // 32 mutable array layers double the use count 32 times.
        let code = format!("declare function f<T>(): T{};\n", "[]".repeat(32));
        let Some((msgs, _)) = crate::typed::lint_for_test(super::RULE.meta.name, &code) else {
            return;
        };
        assert!(msgs.is_empty(), "{msgs:?}");
    }
}
