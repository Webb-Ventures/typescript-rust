//! `unicorn/consistent-destructuring`: after `const {a} = object` (object a name or
//! `this`), `object.a` should use the destructured `a`.
//!
//! A read matches the latest earlier declaration (same source text for the object) whose
//! binding for the property is the one visible at the read, in a scope the read is inside,
//! with the same root variable (or `this` boundary), when nothing could have changed the
//! value in between: no write to the root variable or to `object.a` after the declaration
//! and before the read (or anywhere in a loop around the read that started after the
//! declaration, or anywhere in another function), and no call in the declaration's
//! function between the two. Computed members, callees, tags, `new` targets, `delete`
//! operands, written members and reads guarded by `'a' in object` are left alone. The
//! suggestion replaces the member with the binding (not for a member's object, nor with
//! comments inside).

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_conditions::{is_loop, unwrap_ts_and_parens};
use super::unicorn_no_duplicate_logical_operands::is_reference_context;
use super::unicorn_text::static_string_value;
use crate::semantic::{ScopeId, SymbolId};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-destructuring",
        "Use destructured variables over properties.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ConsistentDestructuring>,
};

#[derive(Debug, Default)]
struct ConsistentDestructuring;

impl FromOptions for ConsistentDestructuring {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(ConsistentDestructuring)
    }
}

struct Declaration {
    scope: ScopeId,
    object: NodeId,
    root_name: Option<String>,
    root_variable: Option<SymbolId>,
    this_boundary: Option<NodeId>,
    pattern: NodeId,
}

struct Write {
    property: String,
    root_name: Option<String>,
    root_variable: Option<SymbolId>,
    this_boundary: Option<NodeId>,
    start: u32,
    scope: ScopeId,
}

fn var_scope(cx: &Ctx<'_, '_>, mut scope: ScopeId) -> ScopeId {
    let sem = cx.semantic();
    for _ in 0..100_000 {
        let s = sem.scope(scope);
        if s.kind.is_var_scope() {
            return scope;
        }
        match s.parent {
            Some(p) => scope = p,
            None => return scope,
        }
    }
    scope
}

fn is_inside_scope(cx: &Ctx<'_, '_>, mut child: ScopeId, parent: ScopeId) -> bool {
    let sem = cx.semantic();
    for _ in 0..100_000 {
        if child == parent {
            return true;
        }
        match sem.scope(child).parent {
            Some(p) => child = p,
            None => return false,
        }
    }
    false
}

/// The function, method, field, static block or program `this` belongs to.
fn this_boundary(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    for _ in 0..100_000 {
        let p = cx.parent(cur)?;
        if matches!(
            cx.node(p),
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::MethodDef { .. }
                | Node::PropertyDef { .. }
                | Node::StaticBlock { .. }
                | Node::Program { .. }
        ) {
            return Some(p);
        }
        cur = p;
    }
    None
}

struct Root {
    name: Option<String>,
    variable: Option<SymbolId>,
    this_boundary: Option<NodeId>,
}

fn root_of(cx: &Ctx<'_, '_>, object: NodeId, scope: ScopeId) -> Root {
    let ast = cx.ast();
    match ast.node(object) {
        Node::Ident { .. } => {
            let name = ident_name(ast, object).map(str::to_owned);
            let variable = name.as_deref().and_then(|n| cx.semantic().lookup(scope, n));
            Root {
                name,
                variable,
                this_boundary: None,
            }
        }
        Node::This {} => Root {
            name: None,
            variable: None,
            this_boundary: this_boundary(cx, object),
        },
        _ => Root {
            name: None,
            variable: None,
            this_boundary: None,
        },
    }
}

struct Collect<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    declarations: Vec<(String, Declaration)>,
    members: Vec<NodeId>,
    writes: Vec<Write>,
}

impl Collect<'_, '_, '_> {
    fn direct_write(&mut self, member: NodeId) {
        let cx = self.cx;
        let ast = cx.ast();
        let Node::Member { obj, prop, .. } = *ast.node(member) else {
            return;
        };
        let object = unwrap_ts_and_parens(ast, obj);
        if !matches!(ast.node(prop), Node::Ident { .. })
            || !matches!(ast.node(object), Node::Ident { .. } | Node::This {})
        {
            return;
        }
        let scope = cx.semantic().enclosing_scope(member);
        let root = root_of(cx, object, scope);
        self.writes.push(Write {
            property: ident_name(ast, prop).unwrap_or("").to_owned(),
            root_name: root.name,
            root_variable: root.variable,
            this_boundary: root.this_boundary,
            start: cx.span(member).start,
            scope,
        });
    }

    fn pattern_writes(&mut self, id: NodeId, depth: u32) {
        if depth > 200 {
            return;
        }
        let ast = self.cx.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::Member { .. } => self.direct_write(id),
            Node::AssignPattern { left, .. } | Node::Assign { left, .. } => {
                self.pattern_writes(left, depth + 1);
            }
            Node::Rest { arg } | Node::Spread { arg } => self.pattern_writes(arg, depth + 1),
            Node::ObjectPattern { props } | Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, .. } => {
                            self.pattern_writes(value.unwrap_or(key), depth + 1);
                        }
                        Node::Property { value, .. } => self.pattern_writes(value, depth + 1),
                        Node::Rest { arg } | Node::Spread { arg } => {
                            self.pattern_writes(arg, depth + 1);
                        }
                        _ => {}
                    }
                }
            }
            Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    self.pattern_writes(e, depth + 1);
                }
            }
            _ => {}
        }
    }
}

impl Visitor for Collect<'_, '_, '_> {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        let cx = self.cx;
        match *ast.node(id) {
            Node::Assign { left, .. } => self.pattern_writes(left, 0),
            Node::ForIn { left, .. } | Node::ForOf { left, .. }
                if !matches!(ast.node(left), Node::VarDecl { .. }) =>
            {
                self.pattern_writes(left, 0);
            }
            Node::Update { arg, .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                arg,
            } => {
                let arg = skip_parens(ast, arg);
                if matches!(ast.node(arg), Node::Member { .. }) {
                    self.direct_write(arg);
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if matches!(ast.node(name), Node::ObjectPattern { .. })
                && cx.parent(id).is_some_and(|p| {
                    matches!(
                        cx.node(p),
                        Node::VarDecl {
                            kind: VarKind::Const,
                            ..
                        }
                    )
                }) =>
            {
                let object = skip_parens(ast, init);
                if matches!(ast.node(object), Node::Ident { .. } | Node::This {}) {
                    let scope = cx.semantic().enclosing_scope(id);
                    let root = root_of(cx, object, scope);
                    self.declarations.push((
                        cx.text(object).to_owned(),
                        Declaration {
                            scope,
                            object,
                            root_name: root.name,
                            root_variable: root.variable,
                            this_boundary: root.this_boundary,
                            pattern: name,
                        },
                    ));
                }
            }
            Node::Member { .. } => self.members.push(id),
            _ => {}
        }
        Walk::Continue
    }
}

fn should_ignore(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let ast = cx.ast();
    let mut outer = member;
    for _ in 0..100_000 {
        match cx.parent(outer).map(|p| (p, cx.node(p))) {
            Some((
                p,
                Node::Paren { .. }
                | Node::As { .. }
                | Node::Satisfies { .. }
                | Node::NonNull { .. }
                | Node::TypeAssertion { .. }
                | Node::Instantiation { .. },
            )) => outer = p,
            _ => break,
        }
    }
    let new_callee = cx
        .parent(outer)
        .is_some_and(|p| matches!(*ast.node(p), Node::New { callee, .. } if callee == outer));
    new_callee || is_reference_context(cx, member) || is_left_hand_side(cx, member)
}

/// The destructured `name` binding of `pattern` visible at `scope`, if any.
fn destructured_binding(
    cx: &Ctx<'_, '_>,
    pattern: NodeId,
    name: &str,
    scope: ScopeId,
) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Node::ObjectPattern { props } = *ast.node(pattern) else {
        return None;
    };
    ast.list(props).iter().find_map(|&p| {
        let Node::BindingProperty {
            key,
            value,
            init: None,
        } = *ast.node(p)
        else {
            return None;
        };
        if !matches!(ast.node(key), Node::Ident { .. }) || ident_name(ast, key) != Some(name) {
            return None;
        }
        let binding = value.unwrap_or(key);
        if !matches!(ast.node(binding), Node::Ident { .. }) {
            return None;
        }
        let variable = sem.symbol_of_decl(binding)?;
        let visible = sem.lookup(scope, ident_name(ast, binding)?)?;
        (visible == variable).then_some(binding)
    })
}

/// Loops around `member` whose body starts after `after`.
fn repeating_loops(cx: &Ctx<'_, '_>, member: NodeId, after: u32) -> Vec<Span> {
    let ast = cx.ast();
    let mut out = Vec::new();
    let mut cur = member;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            break;
        };
        let body = match *ast.node(p) {
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::While { body, .. }
            | Node::DoWhile { body, .. } => Some(body),
            _ => None,
        };
        if let Some(body) = body
            && is_loop(ast.node(p))
            && cx.span(body).start > after
        {
            out.push(cx.span(p));
        }
        cur = p;
    }
    out
}

fn in_spans(pos: u32, spans: &[Span]) -> bool {
    spans.iter().any(|s| pos >= s.start && pos < s.end)
}

/// The call `id` is the callee (or the callee's object chain) of.
fn enclosing_call(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..100_000 {
        let p = cx.parent(cur)?;
        match *ast.node(p) {
            Node::Paren { .. } => cur = p,
            Node::Call { callee, .. } | Node::New { callee, .. } if callee == cur => {
                return Some(p);
            }
            Node::TaggedTemplate { tag, .. } if tag == cur => return Some(p),
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == cur => cur = p,
            _ => return None,
        }
    }
    None
}

fn has_call_between(cx: &Ctx<'_, '_>, object: NodeId, member: NodeId) -> bool {
    struct Find<'c, 'a, 'b> {
        cx: &'c Ctx<'a, 'b>,
        range: Span,
        member: Span,
        scope: ScopeId,
        found: bool,
    }
    impl Visitor for Find<'_, '_, '_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let span = ast.span(id);
            if span.end <= self.range.start || span.start >= self.range.end {
                return Walk::SkipChildren;
            }
            if matches!(ast.node(id), Node::Ident { .. })
                && span.start >= self.range.start
                && span.end <= self.range.end
                && let Some(call) = enclosing_call(self.cx, id)
            {
                let call_span = self.cx.span(call);
                let same_scope =
                    var_scope(self.cx, self.cx.semantic().enclosing_scope(call)) == self.scope;
                if same_scope
                    && (self.member.start < call_span.start || self.member.start > call_span.end)
                {
                    self.found = true;
                    return Walk::Stop;
                }
            }
            Walk::Continue
        }
    }
    let scope = var_scope(cx, cx.semantic().enclosing_scope(object));
    let mut f = Find {
        cx,
        range: Span::new(cx.span(object).end, cx.span(member).start),
        member: cx.span(member),
        scope,
        found: false,
    };
    walk(cx.ast(), cx.ast().root, &mut f);
    f.found
}

struct Read {
    member: NodeId,
    property: String,
    scope: ScopeId,
    root: Root,
}

fn is_matching_declaration(cx: &Ctx<'_, '_>, d: &Declaration, r: &Read, writes: &[Write]) -> bool {
    let sem = cx.semantic();
    let declaration_end = cx.span(d.object).end;
    let member_start = cx.span(r.member).start;
    if declaration_end >= member_start {
        return false;
    }
    if d.root_name.is_some() && r.root.name == d.root_name && r.root.variable != d.root_variable {
        return false;
    }
    if d.this_boundary.is_some() && r.root.this_boundary != d.this_boundary {
        return false;
    }
    if !is_inside_scope(cx, r.scope, d.scope) {
        return false;
    }
    let loops = repeating_loops(cx, r.member, cx.span(d.object).start);
    let member_var_scope = var_scope(cx, r.scope);
    if let Some(root) = d.root_variable {
        let reassigned = sem.symbol(root).refs.iter().any(|&rid| {
            let reference = sem.reference(rid);
            if !reference.is_write() {
                return false;
            }
            let start = cx.span(reference.node).start;
            if start < declaration_end {
                return false;
            }
            var_scope(cx, sem.enclosing_scope(reference.node)) != member_var_scope
                || start <= member_start
                || in_spans(start, &loops)
        });
        if reassigned {
            return false;
        }
    }
    let member_written = writes.iter().any(|w| {
        if w.property != r.property
            || w.root_name != r.root.name
            || w.root_variable != r.root.variable
            || w.this_boundary != r.root.this_boundary
        {
            return false;
        }
        if var_scope(cx, w.scope) != member_var_scope {
            return true;
        }
        in_spans(w.start, &loops) || (w.start >= declaration_end && w.start <= member_start)
    });
    !member_written && !has_call_between(cx, d.object, r.member)
}

fn has_matching_in(
    cx: &Ctx<'_, '_>,
    test: NodeId,
    object: NodeId,
    property: &str,
    depth: u32,
) -> bool {
    if depth > 200 {
        return false;
    }
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, test)) {
        Node::Binary {
            left,
            op: BinaryOp::In,
            right,
        } => {
            static_string_value(ast, skip_parens(ast, left)).as_deref() == Some(property)
                && cx.text(skip_parens(ast, right)) == cx.text(skip_parens(ast, object))
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd,
            right,
        } => {
            has_matching_in(cx, left, object, property, depth + 1)
                || has_matching_in(cx, right, object, property, depth + 1)
        }
        _ => false,
    }
}

fn is_in_guarded_branch(cx: &Ctx<'_, '_>, member: NodeId, object: NodeId, property: &str) -> bool {
    let ast = cx.ast();
    let mut child = member;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(child) else {
            return false;
        };
        match *ast.node(p) {
            Node::FunctionDecl { .. } | Node::PropertyDef { .. } => return false,
            // Object literal methods are plain properties in ESTree: no boundary.
            Node::MethodDef { .. }
                if cx.parent(p).is_some_and(|c| {
                    matches!(cx.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                }) =>
            {
                return false;
            }
            Node::Conditional { test, cons, .. } | Node::If { test, cons, .. }
                if cons == child && has_matching_in(cx, test, object, property, 0) =>
            {
                return true;
            }
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd,
                right,
            } if right == child && has_matching_in(cx, left, object, property, 0) => {
                return true;
            }
            _ => {}
        }
        child = p;
    }
    false
}

impl Rule for ConsistentDestructuring {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut c = Collect {
            cx,
            declarations: Vec::new(),
            members: Vec::new(),
            writes: Vec::new(),
        };
        walk(ast, ast.root, &mut c);
        let Collect {
            declarations,
            members,
            writes,
            ..
        } = c;
        if declarations.is_empty() {
            return;
        }
        for member in members {
            let Node::Member { obj, prop, .. } = *ast.node(member) else {
                continue;
            };
            if should_ignore(cx, member) {
                continue;
            }
            let object = unwrap_ts_and_parens(ast, obj);
            let key = cx.text(object);
            if !declarations.iter().any(|(k, _)| k == key) {
                continue;
            }
            let scope = cx.semantic().enclosing_scope(member);
            let property = cx.text(prop).to_owned();
            let read = Read {
                member,
                root: root_of(cx, object, scope),
                property,
                scope,
            };
            let binding = declarations
                .iter()
                .rev()
                .filter(|(k, _)| k == key)
                .find_map(|(_, d)| {
                    let binding = destructured_binding(cx, d.pattern, &read.property, scope)?;
                    is_matching_declaration(cx, d, &read, &writes).then_some(binding)
                });
            let Some(binding) = binding else {
                continue;
            };
            if is_in_guarded_branch(cx, member, obj, &read.property) {
                continue;
            }
            let span = cx.span(member);
            let message = "Use destructured variables over properties.";
            let parent_is_member = parent_skip_parens(ast, cx.file.parents, member)
                .is_some_and(|p| matches!(ast.node(p), Node::Member { .. } | Node::Index { .. }));
            if parent_is_member || !ast.comments_in(span).is_empty() {
                cx.report(span, message).id("consistentDestructuring");
                continue;
            }
            let name = cx.text(binding).to_owned();
            let expression = cx.text(member).to_owned();
            cx.report(span, message)
                .id("consistentDestructuring")
                .suggest(
                    "consistentDestructuringSuggest",
                    format!("Replace `{expression}` with destructured property `{name}`."),
                    Fix::replace(span, name),
                );
        }
    }
}
