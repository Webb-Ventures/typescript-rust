//! `unicorn/consistent-compound-words`: identifiers spelling a closed compound as two words
//! (`callBack`, `dataBase`, `userName`, ... or the `replacements` option) are reported with
//! the closed form (`callback`). A discouraged word matches in lowercase-first form only at
//! the start of a name and in capitalised form anywhere, ending at the end, a digit, `_`,
//! `$` or a capital; longer words are tried first, and each match keeps its first letter's
//! case. All-caps names and `allowList` names are skipped.
//!
//! Variables (every declared binding, scope by scope) are checked by default and get a
//! rename suggestion when the new name is an identifier; `_`s are added until it is free in
//! the binding's scope and every scope referencing it (and not already given to another
//! rename). There is no suggestion for exported declarations or names used as JSX tags.
//! Default, namespace and `require()` bindings and shorthand import bindings are only
//! checked for internal modules (`./`, `/`) unless their option is `true`/`false`;
//! shorthand destructuring is skipped unless `checkShorthandProperties`. With
//! `checkProperties`, property names that define or write a property (object keys, class
//! members, interface members, assignment and update targets) are reported without a fix.

use super::prelude::*;
use super::unicorn_static::static_string_literal;
use super::unicorn_util_h4::{available_name, estree_ident_span, rename_symbol_fix};
use crate::semantic::{DeclKind, RefFlags, ScopeId, SymbolFlags, SymbolId};
use ts_r_ast::Modifiers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-compound-words",
        "Enforce consistent spelling of compound words in identifiers.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ConsistentCompoundWords>,
};

const DEFAULTS: &[(&str, &str)] = &[
    ("backGround", "background"),
    ("callBack", "callback"),
    ("checkBox", "checkbox"),
    ("clipBoard", "clipboard"),
    ("codeBase", "codebase"),
    ("dataBase", "database"),
    ("downLoad", "download"),
    ("feedBack", "feedback"),
    ("foreGround", "foreground"),
    ("frameWork", "framework"),
    ("headLine", "headline"),
    ("keyBoard", "keyboard"),
    ("keyFrame", "keyframe"),
    ("lifeCycle", "lifecycle"),
    ("metaData", "metadata"),
    ("midPoint", "midpoint"),
    ("nameSpace", "namespace"),
    ("overRide", "override"),
    ("passWord", "password"),
    ("payLoad", "payload"),
    ("placeHolder", "placeholder"),
    ("preView", "preview"),
    ("screenShot", "screenshot"),
    ("sideBar", "sidebar"),
    ("subClass", "subclass"),
    ("subDirectory", "subdirectory"),
    ("subDomain", "subdomain"),
    ("subMenu", "submenu"),
    ("subProcess", "subprocess"),
    ("subString", "substring"),
    ("subTree", "subtree"),
    ("subType", "subtype"),
    ("subTitle", "subtitle"),
    ("timeOut", "timeout"),
    ("timeStamp", "timestamp"),
    ("toolBar", "toolbar"),
    ("toolKit", "toolkit"),
    ("toolTip", "tooltip"),
    ("touchScreen", "touchscreen"),
    ("unSubscribe", "unsubscribe"),
    ("underScore", "underscore"),
    ("upLoad", "upload"),
    ("userName", "username"),
    ("viewPort", "viewport"),
    ("webCam", "webcam"),
    ("webHook", "webhook"),
    ("webSite", "website"),
    ("weekEnd", "weekend"),
    ("whiteSpace", "whitespace"),
    ("wildCard", "wildcard"),
    ("workFlow", "workflow"),
    ("workSpace", "workspace"),
];

/// `true`, `false` or `'internal'` for the import options.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ImportCheck {
    Always,
    Never,
    Internal,
}

fn import_check(v: Option<&Value>) -> ImportCheck {
    match v {
        Some(Value::Bool(true)) => ImportCheck::Always,
        Some(Value::Bool(false)) => ImportCheck::Never,
        _ => ImportCheck::Internal,
    }
}

#[derive(Debug)]
struct ConsistentCompoundWords {
    check_properties: bool,
    check_variables: bool,
    default_imports: ImportCheck,
    shorthand_imports: ImportCheck,
    shorthand_properties: bool,
    /// Discouraged word -> replacement, longest word first.
    replacements: Vec<(String, String)>,
    allow: Vec<String>,
}

impl FromOptions for ConsistentCompoundWords {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let mut merged: Vec<(String, Option<String>)> = Vec::new();
        if flag("extendDefaultReplacements", true) {
            merged.extend(
                DEFAULTS
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), Some((*v).to_owned()))),
            );
        }
        if let Some(user) = o.get("replacements").and_then(Value::as_object) {
            for (k, v) in user {
                let v = v.as_str().map(str::to_owned);
                match merged.iter_mut().find(|(m, _)| m == k) {
                    Some(e) => e.1 = v,
                    None => merged.push((k.clone(), v)),
                }
            }
        }
        let mut replacements: Vec<(String, String)> = merged
            .into_iter()
            .filter_map(|(k, v)| Some((k, v?)))
            .collect();
        replacements.sort_by_key(|(k, _)| std::cmp::Reverse(k.encode_utf16().count()));
        let allow = o
            .get("allowList")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        Ok(ConsistentCompoundWords {
            check_properties: flag("checkProperties", false),
            check_variables: flag("checkVariables", true),
            default_imports: import_check(o.get("checkDefaultAndNamespaceImports")),
            shorthand_imports: import_check(o.get("checkShorthandImports")),
            shorthand_properties: flag("checkShorthandProperties", false),
            replacements,
            allow,
        })
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn is_upper_first(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.to_uppercase().eq(std::iter::once(c)))
}

/// A compound word may end here: the end, a digit, `_`, `$` or an uppercase letter.
fn at_boundary(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|c| c.is_ascii_digit() || c == '_' || c == '$' || c.is_uppercase())
}

impl ConsistentCompoundWords {
    fn part_replacement(&self, part: &str) -> String {
        let found = self
            .replacements
            .iter()
            .find(|(k, _)| k == part)
            .or_else(|| {
                let lower = lower_first(part);
                self.replacements.iter().find(|(k, _)| *k == lower)
            });
        match found {
            None => part.to_owned(),
            Some((_, r)) if is_upper_first(part) => upper_first(r),
            Some((_, r)) => lower_first(r),
        }
    }

    /// The name with every discouraged word replaced, when that changes it.
    fn name_replacement(&self, name: &str) -> Option<String> {
        if self.replacements.is_empty()
            || name.to_uppercase() == name
            || self.allow.iter().any(|a| a == name)
        {
            return None;
        }
        let uppers: Vec<String> = self
            .replacements
            .iter()
            .map(|(k, _)| upper_first(k))
            .collect();
        let mut out = String::with_capacity(name.len());
        let mut i = 0;
        while i < name.len() {
            let rest = &name[i..];
            let lower = if i == 0 {
                self.replacements
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .find(|k| rest.starts_with(k) && at_boundary(&rest[k.len()..]))
            } else {
                None
            };
            let hit = lower.or_else(|| {
                uppers
                    .iter()
                    .map(String::as_str)
                    .find(|k| !k.is_empty() && rest.starts_with(k) && at_boundary(&rest[k.len()..]))
            });
            match hit {
                Some(k) if !k.is_empty() => {
                    out.push_str(&self.part_replacement(k));
                    i += k.len();
                }
                _ => {
                    let c = rest.chars().next().unwrap_or(' ');
                    out.push(c);
                    i += c.len_utf8();
                }
            }
        }
        (out != name).then_some(out)
    }
}

fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '$' || c == '_' || ts_r_lexer::is_id_start(c))
        && chars.all(|c| {
            c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || ts_r_lexer::is_id_part(c)
        })
}

/// `./x`, `/x` and not through `node_modules`.
fn is_internal(source: &str) -> bool {
    !source.contains("node_modules") && (source.starts_with('.') || source.starts_with('/'))
}

/// The static `require("x")` initialising the declarator `decl` whose name is `ident`.
fn require_source(cx: &Ctx<'_, '_>, decl: NodeId, ident: NodeId) -> Option<String> {
    let ast = cx.ast();
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl)
    else {
        return None;
    };
    if name != ident {
        return None;
    }
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, init))
    else {
        return None;
    };
    let args = ast.list(args);
    if ident_name(ast, callee) != Some("require") || args.len() != 1 {
        return None;
    }
    if matches!(ast.node(args[0]), Node::Spread { .. }) {
        return None;
    }
    static_string_literal(ast, args[0])
}

fn import_source(cx: &Ctx<'_, '_>, decl_parent: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(decl_parent) {
        Node::ImportDecl { source, .. } => match *ast.node(source) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            _ => None,
        },
        _ => None,
    }
}

impl ConsistentCompoundWords {
    fn check_import_rules(&self, cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
        let sem = cx.semantic();
        let ast = cx.ast();
        let Some(def) = sem.symbol(sym).decls.first() else {
            return false;
        };
        let def = *def;
        let source_of = || match def.kind {
            DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
                import_source(cx, def.parent)
            }
            _ => require_source(cx, def.node, def.name),
        };
        let allowed = |check: ImportCheck| match check {
            ImportCheck::Always => true,
            ImportCheck::Never => false,
            ImportCheck::Internal => source_of().is_some_and(|s| is_internal(&s)),
        };
        let default_like = match def.kind {
            DeclKind::ImportDefault | DeclKind::ImportNamespace => true,
            DeclKind::ImportNamed => matches!(
                *ast.node(def.node),
                Node::ImportSpecifier { imported: Some(i), local, .. }
                    if local == def.name && matches!(*ast.node(i), Node::Ident { name } if ast.atom(name) == "default")
            ),
            _ => require_source(cx, def.node, def.name).is_some(),
        };
        if default_like && !allowed(self.default_imports) {
            return false;
        }
        let shorthand_import = def.kind == DeclKind::ImportNamed
            && matches!(*ast.node(def.node), Node::ImportSpecifier { imported: None, local, .. } if local == def.name);
        if shorthand_import && !allowed(self.shorthand_imports) {
            return false;
        }
        if !self.shorthand_properties
            && cx.parent(def.name).is_some_and(|p| {
                matches!(*ast.node(p), Node::BindingProperty { key, value: None, init: None } if key == def.name)
            })
        {
            return false;
        }
        true
    }

    /// No identifier of the binding is an exported declaration name or a JSX tag.
    fn can_rename(&self, cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
        let sem = cx.semantic();
        let ast = cx.ast();
        let s = sem.symbol(sym);
        if s.refs.iter().any(|&r| {
            sem.reference(r)
                .flags
                .intersects(RefFlags::JSX | RefFlags::IMPLICIT)
        }) {
            return false;
        }
        !s.decls.iter().any(|d| {
            let exported = |m: Modifiers| m.contains(Modifiers::EXPORT) && !m.contains(Modifiers::DEFAULT);
            match *ast.node(d.node) {
                Node::VarDeclarator { name, .. } if name == d.name => {
                    matches!(*ast.node(d.parent), Node::VarDecl { modifiers, .. } if exported(modifiers))
                }
                Node::FunctionDecl { modifiers, .. }
                | Node::ClassDecl { modifiers, .. }
                | Node::TypeAlias { modifiers, .. }
                | Node::Interface { modifiers, .. }
                | Node::Enum { modifiers, .. } => exported(modifiers),
                _ => false,
            }
        })
    }

    fn check_variables(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let mut order: Vec<(ScopeId, SymbolId)> = (0..sem.symbols().len())
            .map(|i| {
                let id = SymbolId(i as u32);
                (sem.symbol(id).scope, id)
            })
            .collect();
        order.sort_by_key(|&(s, id)| (s.0, id.0));
        let mut generated: Vec<(ScopeId, String)> = Vec::new();
        for (scope, sym) in order {
            let s = sem.symbol(sym);
            if s.decls.is_empty()
                || s.flags
                    .intersects(SymbolFlags::ARGUMENTS | SymbolFlags::COMMENT_GLOBAL)
            {
                continue;
            }
            let name = sem.name(sym).to_owned();
            let Some(replacement) = self.name_replacement(&name) else {
                continue;
            };
            if !self.check_import_rules(cx, sym) {
                continue;
            }
            let mut scopes: Vec<ScopeId> = s.refs.iter().map(|&r| sem.reference(r).scope).collect();
            scopes.push(scope);
            let safe = if is_identifier_name(&replacement) {
                available_name(cx, &replacement, &scopes, &|n| {
                    !generated
                        .iter()
                        .any(|(gs, gn)| gn == n && scopes.contains(gs))
                })
            } else {
                None
            };
            let ident = s.decls[0].name;
            let span = estree_ident_span(cx, ident);
            let shown = safe.clone().unwrap_or(replacement);
            let rename = safe.filter(|_| self.can_rename(cx, sym));
            let fix = rename.as_ref().map(|n| rename_symbol_fix(cx, sym, n));
            if let Some(n) = &rename {
                generated.extend(scopes.iter().map(|&sc| (sc, n.clone())));
            }
            let r = cx
                .report(span, format!("Prefer `{shown}` over `{name}`."))
                .id("consistent-compound-words/error");
            if let (Some(n), Some(fix)) = (rename, fix) {
                r.suggest(
                    "consistent-compound-words/rename",
                    format!("Rename to `{n}`."),
                    fix,
                );
            }
        }
    }

    /// A property-name identifier that defines or writes the property.
    fn is_property_name(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        let ast = cx.ast();
        let Some(p) = cx.parent(id) else {
            return false;
        };
        match *ast.node(p) {
            Node::Member { prop, .. } if prop == id => {
                let Some(target) = parent_skip_parens(ast, cx.file.parents, p) else {
                    return false;
                };
                matches!(*ast.node(target), Node::Assign { left, .. } if skip_parens(ast, left) == p)
                    || matches!(*ast.node(target), Node::Update { arg, .. } if skip_parens(ast, arg) == p)
            }
            Node::Property { key, .. } if key == id => true,
            Node::MethodDef { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. }
                if key == id =>
            {
                true
            }
            Node::Param { pattern, .. } if pattern == id && !self.check_variables => {
                cx.semantic().symbol_of_decl(id).is_some_and(|s| {
                    cx.semantic()
                        .symbol(s)
                        .flags
                        .contains(SymbolFlags::PARAM_PROPERTY)
                })
            }
            _ => false,
        }
    }
}

impl Rule for ConsistentCompoundWords {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident, NodeKind::PrivateName]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if self.check_variables {
            self.check_variables(cx);
        }
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.check_properties {
            return;
        }
        let ast = cx.ast();
        let name = match *ast.node(node) {
            Node::Ident { name } | Node::PrivateName { name } => ast.atom(name),
            _ => return,
        };
        if name == "__proto__" {
            return;
        }
        let Some(replacement) = self.name_replacement(name) else {
            return;
        };
        if !self.is_property_name(cx, node) {
            return;
        }
        let name = name.to_owned();
        cx.report(
            estree_ident_span(cx, node),
            format!("Prefer `{replacement}` over `{name}`."),
        )
        .id("consistent-compound-words/error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_words() {
        let r = ConsistentCompoundWords::from_options(&[]).unwrap();
        assert_eq!(r.name_replacement("callBack").as_deref(), Some("callback"));
        assert_eq!(
            r.name_replacement("onCallBack").as_deref(),
            Some("onCallback")
        );
        assert_eq!(r.name_replacement("callBacks"), None);
        assert_eq!(r.name_replacement("USER_NAME"), None);
        assert_eq!(
            r.name_replacement("userName2").as_deref(),
            Some("username2")
        );
    }
}
