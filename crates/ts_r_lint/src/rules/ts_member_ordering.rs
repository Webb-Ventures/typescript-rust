//! `@typescript-eslint/member-ordering`: members of classes, interfaces and type literals in
//! the configured order of member groups (kind, accessibility, scope, decorators), optionally
//! sorted by name within each group and split into optional and required members.

use std::borrow::Cow;
use std::cmp::Ordering;

use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;
use super::sort_keys::natural_cmp;
use super::ts_adjacent_overload_signatures::{NameKind, member_key_name};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/member-ordering",
        "Require a consistent member declaration order",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MemberOrdering>,
};

const DEFAULT_ORDER: &[&str] = &[
    "signature",
    "call-signature",
    "public-static-field",
    "protected-static-field",
    "private-static-field",
    "#private-static-field",
    "public-decorated-field",
    "protected-decorated-field",
    "private-decorated-field",
    "public-instance-field",
    "protected-instance-field",
    "private-instance-field",
    "#private-instance-field",
    "public-abstract-field",
    "protected-abstract-field",
    "public-field",
    "protected-field",
    "private-field",
    "#private-field",
    "static-field",
    "instance-field",
    "abstract-field",
    "decorated-field",
    "field",
    "static-initialization",
    "public-constructor",
    "protected-constructor",
    "private-constructor",
    "constructor",
    "public-static-accessor",
    "protected-static-accessor",
    "private-static-accessor",
    "#private-static-accessor",
    "public-decorated-accessor",
    "protected-decorated-accessor",
    "private-decorated-accessor",
    "public-instance-accessor",
    "protected-instance-accessor",
    "private-instance-accessor",
    "#private-instance-accessor",
    "public-abstract-accessor",
    "protected-abstract-accessor",
    "public-accessor",
    "protected-accessor",
    "private-accessor",
    "#private-accessor",
    "static-accessor",
    "instance-accessor",
    "abstract-accessor",
    "decorated-accessor",
    "accessor",
    "public-static-get",
    "protected-static-get",
    "private-static-get",
    "#private-static-get",
    "public-decorated-get",
    "protected-decorated-get",
    "private-decorated-get",
    "public-instance-get",
    "protected-instance-get",
    "private-instance-get",
    "#private-instance-get",
    "public-abstract-get",
    "protected-abstract-get",
    "public-get",
    "protected-get",
    "private-get",
    "#private-get",
    "static-get",
    "instance-get",
    "abstract-get",
    "decorated-get",
    "get",
    "public-static-set",
    "protected-static-set",
    "private-static-set",
    "#private-static-set",
    "public-decorated-set",
    "protected-decorated-set",
    "private-decorated-set",
    "public-instance-set",
    "protected-instance-set",
    "private-instance-set",
    "#private-instance-set",
    "public-abstract-set",
    "protected-abstract-set",
    "public-set",
    "protected-set",
    "private-set",
    "#private-set",
    "static-set",
    "instance-set",
    "abstract-set",
    "decorated-set",
    "set",
    "public-static-method",
    "protected-static-method",
    "private-static-method",
    "#private-static-method",
    "public-decorated-method",
    "protected-decorated-method",
    "private-decorated-method",
    "public-instance-method",
    "protected-instance-method",
    "private-instance-method",
    "#private-instance-method",
    "public-abstract-method",
    "protected-abstract-method",
    "public-method",
    "protected-method",
    "private-method",
    "#private-method",
    "static-method",
    "instance-method",
    "abstract-method",
    "decorated-method",
    "method",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortOrder {
    AsWritten,
    Alpha,
    AlphaInsensitive,
    Natural,
    NaturalInsensitive,
}

/// One entry of `memberTypes`: a group name or several sharing a rank.
type MemberTypes = Vec<Vec<String>>;

#[derive(Debug, Clone)]
enum Config {
    Never,
    Order {
        member_types: Option<MemberTypes>,
        order: SortOrder,
        optional_first: Option<bool>,
    },
}

#[derive(Debug)]
struct MemberOrdering {
    classes: Option<Config>,
    class_expressions: Option<Config>,
    default: Config,
    interfaces: Option<Config>,
    type_literals: Option<Config>,
}

fn member_types(v: &Value) -> Option<MemberTypes> {
    let a = v.as_array()?;
    Some(
        a.iter()
            .map(|e| match e {
                Value::Array(xs) => xs
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect(),
                Value::String(s) => vec![s.clone()],
                _ => Vec::new(),
            })
            .collect(),
    )
}

fn default_types() -> MemberTypes {
    DEFAULT_ORDER
        .iter()
        .map(|s| vec![(*s).to_owned()])
        .collect()
}

/// Parses one config; `base` is what typescript-eslint deep-merges an object config onto.
fn config(v: &Value, base: Option<&Config>) -> Config {
    match v {
        Value::String(s) if s == "never" => Config::Never,
        Value::Array(_) => Config::Order {
            member_types: member_types(v),
            order: SortOrder::AsWritten,
            optional_first: None,
        },
        Value::Object(o) => {
            let (base_types, base_order, base_opt) = match base {
                Some(Config::Order {
                    member_types,
                    order,
                    optional_first,
                }) => (member_types.clone(), *order, *optional_first),
                _ => (None, SortOrder::AsWritten, None),
            };
            let types = match o.get("memberTypes") {
                Some(Value::String(s)) if s == "never" => None,
                Some(t) => member_types(t),
                None => base_types,
            };
            let order = match str_field(o, "order") {
                Some("alphabetically") => SortOrder::Alpha,
                Some("alphabetically-case-insensitive") => SortOrder::AlphaInsensitive,
                Some("natural") => SortOrder::Natural,
                Some("natural-case-insensitive") => SortOrder::NaturalInsensitive,
                Some(_) => SortOrder::AsWritten,
                None => base_order,
            };
            let optional_first = match str_field(o, "optionalityOrder") {
                Some(s) => Some(s == "optional-first"),
                None => base_opt,
            };
            Config::Order {
                member_types: types,
                order,
                optional_first,
            }
        }
        _ => base.cloned().unwrap_or(Config::Never),
    }
}

impl FromOptions for MemberOrdering {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let base = Config::Order {
            member_types: Some(default_types()),
            order: SortOrder::AsWritten,
            optional_first: None,
        };
        let get = |k: &str| o.get(k).map(|v| config(v, None));
        Ok(MemberOrdering {
            classes: get("classes"),
            class_expressions: get("classExpressions"),
            default: match o.get("default") {
                Some(v) => config(v, Some(&base)),
                None => base,
            },
            interfaces: get("interfaces"),
            type_literals: get("typeLiterals"),
        })
    }
}

/// A member's kind as the option spells it.
fn node_type(ast: &ts_r_ast::Ast, m: NodeId) -> Option<&'static str> {
    Some(match *ast.node(m) {
        Node::MethodDef { kind, .. } | Node::MethodSignature { kind, .. } => match kind {
            MethodKind::Constructor => "constructor",
            MethodKind::Get => "get",
            MethodKind::Set => "set",
            _ => "method",
        },
        Node::CallSignature { .. } => "call-signature",
        Node::ConstructSignature { .. } => "constructor",
        Node::PropertySignature { modifiers, .. } => readonly_field(modifiers),
        Node::PropertyDef {
            modifiers, value, ..
        } => {
            if modifiers.contains(Modifiers::ACCESSOR) {
                "accessor"
            } else if !modifiers.contains(Modifiers::ABSTRACT)
                && value.is_some_and(|v| {
                    matches!(ast.node(v), Node::FunctionExpr { .. } | Node::Arrow { .. })
                })
            {
                "method"
            } else {
                readonly_field(modifiers)
            }
        }
        Node::IndexSignature { modifiers, .. } => {
            if modifiers.contains(Modifiers::READONLY) {
                "readonly-signature"
            } else {
                "signature"
            }
        }
        Node::StaticBlock { .. } => "static-initialization",
        _ => return None,
    })
}

fn readonly_field(m: Modifiers) -> &'static str {
    if m.contains(Modifiers::READONLY) {
        "readonly-field"
    } else {
        "field"
    }
}

fn raw_name<'a>(cx: &Ctx<'_, 'a>, key: NodeId) -> Cow<'a, str> {
    let (name, kind) = member_key_name(cx, key);
    match kind {
        NameKind::Quoted => Cow::Owned(name[1..name.len() - 1].to_owned()),
        NameKind::Private => Cow::Owned(name[1..].to_owned()),
        _ => name,
    }
}

fn member_name<'a>(cx: &Ctx<'_, 'a>, m: NodeId) -> Option<Cow<'a, str>> {
    let ast = cx.ast();
    Some(match *ast.node(m) {
        Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        } => "constructor".into(),
        Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. } => raw_name(cx, key),
        Node::ConstructSignature { .. } => "new".into(),
        Node::CallSignature { .. } => "call".into(),
        Node::IndexSignature { params, .. } => ast
            .list(params)
            .iter()
            .find_map(|&p| match *ast.node(p) {
                Node::Param { pattern, .. } => ident_name(ast, pattern),
                _ => ident_name(ast, p),
            })
            .unwrap_or("(index signature)")
            .into(),
        Node::StaticBlock { .. } => "static block".into(),
        _ => return None,
    })
}

fn is_optional(ast: &ts_r_ast::Ast, m: NodeId) -> bool {
    match *ast.node(m) {
        Node::PropertySignature { optional, .. }
        | Node::MethodSignature { optional, .. }
        | Node::PropertyDef { optional, .. }
        | Node::MethodDef { optional, .. } => optional,
        _ => false,
    }
}

fn initializer(ast: &ts_r_ast::Ast, m: NodeId) -> Option<NodeId> {
    match *ast.node(m) {
        Node::PropertyDef { value, .. } => value,
        _ => None,
    }
}

/// `this.x` read while the initializer runs (not inside nested functions or classes).
struct ThisNames<'s> {
    src: &'s str,
    names: Vec<String>,
}

impl Visitor for ThisNames<'_> {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassExpr { .. }
            | Node::ClassDecl { .. } => Walk::SkipChildren,
            Node::Member { obj, prop, .. } if matches!(ast.node(obj), Node::This {}) => {
                if let Node::Ident { name } | Node::PrivateName { name } = *ast.node(prop) {
                    self.names.push(ast.atom(name).to_owned());
                }
                Walk::Continue
            }
            Node::Index { obj, index, .. } if matches!(ast.node(obj), Node::This {}) => {
                if let Some(n) = static_value_string(ast, self.src, index) {
                    self.names.push(n.into_owned());
                }
                Walk::Continue
            }
            _ => Walk::Continue,
        }
    }
}

impl MemberOrdering {
    fn rank(ast: &ts_r_ast::Ast, m: NodeId, order: &MemberTypes, modifiers_ok: bool) -> i64 {
        if let Node::MethodDef {
            modifiers, value, ..
        } = *ast.node(m)
            && !modifiers.contains(Modifiers::ABSTRACT)
            && matches!(ast.node(value), Node::FunctionExpr { body: None, .. })
        {
            return -1;
        }
        let Some(ty) = node_type(ast, m) else {
            return order.len() as i64 - 1;
        };
        let (mods, decorated, private_name) = match *ast.node(m) {
            Node::MethodDef {
                modifiers,
                decorators,
                key,
                ..
            }
            | Node::PropertyDef {
                modifiers,
                decorators,
                key,
                ..
            } => (
                modifiers,
                !ast.list(decorators).is_empty(),
                matches!(ast.node(key), Node::PrivateName { .. }),
            ),
            _ => (Modifiers::default(), false, false),
        };
        let scope = if mods.contains(Modifiers::STATIC) {
            "static"
        } else if mods.contains(Modifiers::ABSTRACT) {
            "abstract"
        } else {
            "instance"
        };
        let access = if mods.contains(Modifiers::PRIVATE) {
            "private"
        } else if mods.contains(Modifiers::PROTECTED) {
            "protected"
        } else if mods.contains(Modifiers::PUBLIC) {
            "public"
        } else if private_name {
            "#private"
        } else {
            "public"
        };
        let mut groups: Vec<String> = Vec::new();
        if modifiers_ok {
            if decorated
                && matches!(
                    ty,
                    "readonly-field" | "field" | "method" | "accessor" | "get" | "set"
                )
            {
                groups.push(format!("{access}-decorated-{ty}"));
                groups.push(format!("decorated-{ty}"));
                if ty == "readonly-field" {
                    groups.push(format!("{access}-decorated-field"));
                    groups.push("decorated-field".into());
                }
            }
            if !matches!(
                ty,
                "readonly-signature" | "signature" | "static-initialization"
            ) {
                if ty != "constructor" {
                    groups.push(format!("{access}-{scope}-{ty}"));
                    groups.push(format!("{scope}-{ty}"));
                    if ty == "readonly-field" {
                        groups.push(format!("{access}-{scope}-field"));
                        groups.push(format!("{scope}-field"));
                    }
                }
                groups.push(format!("{access}-{ty}"));
                if ty == "readonly-field" {
                    groups.push(format!("{access}-field"));
                }
            }
        }
        groups.push(ty.into());
        match ty {
            "readonly-signature" => groups.push("signature".into()),
            "readonly-field" => groups.push("field".into()),
            _ => {}
        }
        for g in &groups {
            if let Some(i) = order.iter().position(|e| e.contains(g)) {
                return i as i64;
            }
        }
        -1
    }

    /// Reports members whose group comes before a group already seen. The groups in order
    /// when nothing was reported.
    fn check_group_sort(
        cx: &mut Ctx<'_, '_>,
        members: &[NodeId],
        order: &MemberTypes,
        modifiers_ok: bool,
    ) -> Option<Vec<Vec<NodeId>>> {
        let ast = cx.ast();
        let mut previous: Vec<i64> = Vec::new();
        let mut groups: Vec<Vec<NodeId>> = Vec::new();
        let mut sorted = true;
        for &m in members {
            let rank = Self::rank(ast, m, order, modifiers_ok);
            if rank == -1 {
                continue;
            }
            let last = previous.last().copied();
            if last.is_some_and(|l| rank < l) {
                let mut lowest = *previous.last().unwrap_or(&rank);
                for &r in &previous {
                    if r > rank {
                        lowest = lowest.min(r);
                    }
                }
                let rank_text = order
                    .get(lowest as usize)
                    .map(|g| {
                        g.iter()
                            .map(|s| s.replace('-', " "))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let name = member_name(cx, m).unwrap_or(Cow::Borrowed("null"));
                let msg =
                    format!("Member {name} should be declared before all {rank_text} definitions.");
                cx.report_node(m, msg).id("incorrectGroupOrder");
                sorted = false;
            } else if last == Some(rank) {
                if let Some(g) = groups.last_mut() {
                    g.push(m);
                }
            } else {
                previous.push(rank);
                groups.push(vec![m]);
            }
        }
        sorted.then_some(groups)
    }

    /// Runs of adjacent members of one rank (as typescript-eslint groups them: the last
    /// member of the list never joins a run).
    fn group_by_type(
        ast: &ts_r_ast::Ast,
        members: &[NodeId],
        order: &MemberTypes,
        modifiers_ok: bool,
    ) -> Vec<Vec<NodeId>> {
        let ranks: Vec<i64> = members
            .iter()
            .map(|&m| Self::rank(ast, m, order, modifiers_ok))
            .collect();
        let mut groups: Vec<Vec<NodeId>> = Vec::new();
        let mut previous: Option<i64> = None;
        for i in 0..members.len().saturating_sub(1) {
            if Some(ranks[i]) == previous {
                if let Some(g) = groups.last_mut() {
                    g.push(members[i]);
                }
            } else if ranks[i] == ranks[i + 1] {
                groups.push(vec![members[i]]);
                previous = Some(ranks[i]);
            }
        }
        groups
    }

    fn out_of_order(name: &str, prev: &str, order: SortOrder) -> bool {
        if name == prev {
            return false;
        }
        match order {
            SortOrder::AsWritten => false,
            SortOrder::Alpha => name < prev,
            SortOrder::AlphaInsensitive => name.to_lowercase() < prev.to_lowercase(),
            SortOrder::Natural => natural_cmp(name, prev) != Ordering::Greater,
            SortOrder::NaturalInsensitive => {
                natural_cmp(&name.to_lowercase(), &prev.to_lowercase()) != Ordering::Greater
            }
        }
    }

    /// `member`'s initializer reads `this.x` of a member with an initializer among
    /// `members[from..to]`, so moving it up would change what it reads.
    fn blocked(
        cx: &Ctx<'_, '_>,
        member: NodeId,
        members: &[NodeId],
        from: usize,
        to: usize,
    ) -> bool {
        let ast = cx.ast();
        let Some(init) = initializer(ast, member) else {
            return false;
        };
        let mut v = ThisNames {
            src: cx.src(),
            names: Vec::new(),
        };
        walk(ast, init, &mut v);
        members[from..to].iter().any(|&o| {
            initializer(ast, o).is_some()
                && member_name(cx, o).is_some_and(|n| v.names.iter().any(|x| *x == *n))
        })
    }

    fn check_alpha_sort(cx: &mut Ctx<'_, '_>, members: &[NodeId], order: SortOrder) {
        let mut prev = String::new();
        let mut prev_index = 0;
        for (i, &m) in members.iter().enumerate() {
            let Some(name) = member_name(cx, m).filter(|n| !n.is_empty()) else {
                continue;
            };
            if Self::out_of_order(&name, &prev, order) {
                if Self::blocked(cx, m, members, prev_index, i) {
                    continue;
                }
                let msg = format!("Member {name} should be declared before member {prev}.");
                cx.report_node(m, msg).id("incorrectOrder");
            }
            prev = name.into_owned();
            prev_index = i;
        }
    }

    fn check_order(
        cx: &mut Ctx<'_, '_>,
        set: &[NodeId],
        all: &[NodeId],
        types: Option<&MemberTypes>,
        order: SortOrder,
        modifiers_ok: bool,
    ) {
        let alpha = order != SortOrder::AsWritten;
        match types {
            Some(types) => match Self::check_group_sort(cx, set, types, modifiers_ok) {
                None => {
                    if alpha {
                        for g in Self::group_by_type(cx.ast(), all, types, modifiers_ok) {
                            Self::check_alpha_sort(cx, &g, order);
                        }
                    }
                }
                Some(groups) => {
                    if alpha {
                        for g in groups {
                            Self::check_alpha_sort(cx, &g, order);
                        }
                    }
                }
            },
            None if alpha => Self::check_alpha_sort(cx, set, order),
            None => {}
        }
    }

    fn validate(cx: &mut Ctx<'_, '_>, members: &[NodeId], cfg: &Config, modifiers_ok: bool) {
        let Config::Order {
            member_types,
            order,
            optional_first,
        } = cfg
        else {
            return;
        };
        let types = member_types.as_ref();
        let Some(optional_first) = *optional_first else {
            Self::check_order(cx, members, members, types, *order, modifiers_ok);
            return;
        };
        let ast = cx.ast();
        let switch = (1..members.len())
            .find(|&i| is_optional(ast, members[i]) != is_optional(ast, members[i - 1]));
        let Some(switch) = switch else {
            Self::check_order(cx, members, members, types, *order, modifiers_ok);
            return;
        };
        let report = |cx: &mut Ctx<'_, '_>, m: NodeId| {
            let name = member_name(cx, m).unwrap_or(Cow::Borrowed("null"));
            let which = if optional_first {
                "optional"
            } else {
                "required"
            };
            let msg = format!("Member {name} should be declared after all {which} members.");
            cx.report_node(m, msg).id("incorrectRequiredMembersOrder");
        };
        if is_optional(ast, members[0]) != optional_first {
            report(cx, members[0]);
            return;
        }
        let at_switch = is_optional(ast, members[switch]);
        if members[switch + 1..]
            .iter()
            .any(|&m| is_optional(ast, m) != at_switch)
        {
            report(cx, members[switch]);
            return;
        }
        Self::check_order(cx, &members[..switch], members, types, *order, modifiers_ok);
        Self::check_order(cx, &members[switch..], members, types, *order, modifiers_ok);
    }
}

impl Rule for MemberOrdering {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
            NodeKind::TypeLiteral,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (members, cfg, modifiers_ok) = match *cx.node(node) {
            Node::ClassDecl { members, .. } => (members, self.classes.as_ref(), true),
            Node::ClassExpr { members, .. } => (members, self.class_expressions.as_ref(), true),
            Node::Interface { body, .. } => (body, self.interfaces.as_ref(), false),
            Node::TypeLiteral { members } => (members, self.type_literals.as_ref(), false),
            _ => return,
        };
        let members = cx.ast().list(members);
        Self::validate(cx, members, cfg.unwrap_or(&self.default), modifiers_ok);
    }
}
