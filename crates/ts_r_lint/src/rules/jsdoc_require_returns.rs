//! `jsdoc/require-returns`: a function that returns a value documents it with `@returns`.

use super::prelude::*;
use crate::jsdoc::{self, Block, Contexts, Doc, Finder, Settings, Target, Tokens};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-returns",
        "Requires that returns are documented with `@returns`",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireReturns>,
};

#[derive(Debug)]
struct PublicOnly {
    ancestors_only: bool,
    cjs: bool,
    esm: bool,
    window: bool,
}

#[derive(Debug)]
struct RequireReturns {
    opts: serde_json::Map<String, Value>,
    enable_fixer: bool,
    force: bool,
    force_async: bool,
    public_only: Option<PublicOnly>,
    check_getters: bool,
    check_constructors: bool,
    exempted_by: Vec<String>,
}

impl FromOptions for RequireReturns {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0).clone();
        if o.contains_key("contexts") {
            Contexts::from_options(&o, &Settings::default_for_validation())?;
        }
        let public_only = match o.get("publicOnly") {
            Some(Value::Bool(true)) => Some(serde_json::Map::new()),
            Some(Value::Object(p)) => Some(p.clone()),
            _ => None,
        }
        .map(|p| PublicOnly {
            ancestors_only: bool_field(&p, "ancestorsOnly", false),
            cjs: bool_field(&p, "cjs", true),
            esm: bool_field(&p, "esm", true),
            window: bool_field(&p, "window", false),
        });
        let exempted_by = if o.contains_key("exemptedBy") {
            str_list(&o, "exemptedBy")
        } else {
            vec!["inheritDoc".to_owned(), "inheritdoc".to_owned()]
        };
        Ok(RequireReturns {
            enable_fixer: bool_field(&o, "enableFixer", false),
            force: bool_field(&o, "forceRequireReturn", false),
            force_async: bool_field(&o, "forceReturnsWithAsync", false),
            public_only,
            check_getters: bool_field(&o, "checkGetters", true),
            check_constructors: bool_field(&o, "checkConstructors", false),
            exempted_by,
            opts: o,
        })
    }
}

/// The method around a method's function (or the method itself), with its kind.
fn method_of(f: &Finder<'_>, t: Option<Target>) -> (Option<MethodKind>, Option<MethodKind>) {
    let Some(Target::Node(id)) = t else {
        return (None, None);
    };
    let own = match *f.ast.node(id) {
        Node::MethodDef { kind, .. } => Some(kind),
        _ => None,
    };
    let parent = f.parents.parent(id).and_then(|p| match *f.ast.node(p) {
        Node::MethodDef { kind, .. } => Some(kind),
        _ => None,
    });
    (own, parent)
}

/// `avoidDocs` plus this rule's own exemptions.
fn exempt(rule: &RequireReturns, f: &Finder<'_>, doc: &Doc, class: Option<&Block>) -> bool {
    let b = &doc.block;
    if b.has_any_tag(&[
        "abstract",
        "virtual",
        "class",
        "constructor",
        "type",
        "interface",
    ]) {
        return true;
    }
    let s = f.settings;
    let either = |t: &str| b.has_tag(t) || class.is_some_and(|c| c.has_tag(t));
    if (s.ignore_replaces_docs && either("ignore"))
        || (s.override_replaces_docs && either("override"))
        || (s.implements_replaces_docs && either("implements"))
        || (s.augments_extends_replaces_docs && (either("augments") || either("extends")))
    {
        return true;
    }
    if rule
        .exempted_by
        .iter()
        .any(|t| b.tags.iter().any(|x| &x.tag == t))
    {
        return true;
    }
    let (own, parent) = method_of(f, doc.target);
    if !rule.check_constructors
        && (own == Some(MethodKind::Constructor)
            || parent == Some(MethodKind::Constructor)
            || b.has_tag("class")
            || b.has_tag("constructor"))
    {
        return true;
    }
    if parent == Some(MethodKind::Get) && !rule.check_getters {
        return true;
    }
    parent == Some(MethodKind::Set)
}

fn is_member_path(f: &Finder<'_>, id: NodeId, obj: &str, prop: Option<&str>) -> bool {
    let Node::Member {
        obj: o, prop: p, ..
    } = *f.ast.node(id)
    else {
        return false;
    };
    ident_name(f.ast, o) == Some(obj) && prop.is_none_or(|want| ident_name(f.ast, p) == Some(want))
}

fn cjs_target(f: &Finder<'_>, left: NodeId) -> bool {
    if is_member_path(f, left, "module", Some("exports"))
        || is_member_path(f, left, "exports", None)
    {
        return true;
    }
    matches!(*f.ast.node(left), Node::Member { obj, .. } if is_member_path(f, obj, "module", Some("exports")))
}

/// Names exported with `export default x` or `export { x }`.
fn exported_names<'a>(f: &Finder<'a>) -> Vec<&'a str> {
    let mut out = Vec::new();
    let Node::Program { stmts } = *f.ast.node(f.ast.root) else {
        return out;
    };
    for &s in f.ast.list(stmts) {
        match *f.ast.node(s) {
            Node::ExportDefaultExpr { expr } => out.extend(ident_name(f.ast, expr)),
            Node::ExportNamed {
                specifiers,
                source: None,
                ..
            } => {
                for &sp in f.ast.list(specifiers) {
                    let mut first = None;
                    f.ast.for_each_child(sp, |c| {
                        if first.is_none() {
                            first = Some(c);
                        }
                    });
                    out.extend(first.and_then(|c| ident_name(f.ast, c)));
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether the documented node is exported (a simplified `publicOnly` check).
fn is_exported(f: &Finder<'_>, p: &PublicOnly, t: Option<Target>) -> bool {
    let Some(t) = t else {
        return false;
    };
    if matches!(t, Target::Export(_)) {
        return p.esm;
    }
    let names = if p.esm && !p.ancestors_only {
        exported_names(f)
    } else {
        Vec::new()
    };
    let mut cur = Some(t.id());
    let mut guard = 0;
    while let Some(id) = cur {
        guard += 1;
        if guard > 10_000 {
            break;
        }
        match *f.ast.node(id) {
            Node::ExportDefaultExpr { .. } if p.esm => return true,
            Node::Assign { left, .. } => {
                if (p.cjs && cjs_target(f, left))
                    || (p.window && is_member_path(f, left, "window", None))
                {
                    return true;
                }
            }
            Node::VarDeclarator { name, .. } => {
                if ident_name(f.ast, name).is_some_and(|n| names.contains(&n)) {
                    return true;
                }
            }
            Node::FunctionDecl { name: Some(n), .. } | Node::ClassDecl { name: Some(n), .. } => {
                if ident_name(f.ast, n).is_some_and(|n| names.contains(&n)) {
                    return true;
                }
            }
            _ => {}
        }
        if p.esm && f.is_exported_decl(id) {
            return true;
        }
        cur = f.parents.parent(id);
    }
    false
}

const FUNCTION_TYPES: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

impl RequireReturns {
    fn check(&self, cx: &mut Ctx<'_, '_>, f: &Finder<'_>, contexts: &Contexts, doc: &mut Doc) {
        let class = f.class_block(doc.target);
        if exempt(self, f, doc, class.as_ref()) {
            return;
        }
        let ctx_force = contexts.find(doc).is_some_and(|e| e.force_require_return);
        let Some(tag) = doc.preferred(cx, f, "returns") else {
            return;
        };
        let count = doc.block.tags_named(&tag).count();
        if count > 1 {
            cx.report(
                doc.comment,
                format!("Found more than one @{tag} declaration."),
            );
        }
        if count > 0 {
            return;
        }
        if let Some(p) = &self.public_only
            && !is_exported(f, p, doc.target)
        {
            return;
        }
        let es = doc.target.map(|t| f.es_type(t));
        let iterating = !contexts.all || es.is_some_and(|t| FUNCTION_TYPES.contains(&t));
        let virtual_fn = contexts.all
            && doc
                .block
                .has_any_tag(&["callback", "function", "func", "method"]);
        let node_async = match doc.target {
            Some(Target::Node(id)) => match *f.ast.node(id) {
                Node::FunctionDecl { flags, .. }
                | Node::FunctionExpr { flags, .. }
                | Node::Arrow { flags, .. } => flags.contains(FnFlags::ASYNC),
                _ => false,
            },
            _ => false,
        };
        let is_async = (!iterating && doc.block.has_tag("async")) || (iterating && node_async);
        let returns = || match doc.target {
            Some(Target::Node(id)) => jsdoc::has_return_value(f.ast, id, self.force_async),
            _ => false,
        };
        let missing = ((self.force || ctx_force) && (iterating || virtual_fn))
            || (self.force_async && is_async)
            || (iterating && returns());
        if !missing {
            return;
        }
        let fix = self.enable_fixer.then(|| {
            add_tag(doc, &tag);
            doc.fix()
        });
        cx.report(doc.comment, format!("Missing JSDoc @{tag} declaration."))
            .fix_opt(fix);
    }
}

/// Adds an empty `@tag` line after the first line of the last tag (or before the last line).
fn add_tag(doc: &mut Doc, tag: &str) {
    let b = &mut doc.block;
    let number = match b.tags.last() {
        Some(t) => t.source.first().map_or(0, |&id| b.lines[id].number),
        None => {
            let first = b
                .source
                .iter()
                .position(|&id| !b.lines[id].tokens.tag.is_empty())
                .map_or(-1, |i| i as i64);
            first - 1
        }
    } + 1;
    let len = b.source.len() as i64;
    let at = if number < 0 {
        (len + number).max(0)
    } else {
        number.min(len)
    } as usize;
    let tokens = Tokens {
        start: format!("{} ", doc.indent),
        delimiter: "*".to_owned(),
        post_delimiter: " ".to_owned(),
        tag: format!("@{tag}"),
        ..Tokens::default()
    };
    let id = b.new_line(at as i64, tokens);
    b.source.insert(at, id);
    for &later in &b.source[at + 1..] {
        b.lines[later].number += 1;
    }
}

impl Rule for RequireReturns {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = Settings::from_cx(cx);
        let Ok(contexts) = Contexts::from_options(&self.opts, &settings) else {
            return;
        };
        jsdoc::for_each_doc(cx, &settings, &contexts, |cx, f, doc| {
            self.check(cx, f, &contexts, doc);
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_source;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    /// The 1-based lines of the rule's reports with `options`.
    fn lines(code: &str, options: serde_json::Value) -> Vec<usize> {
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "jsdoc/require-returns": ["error", options] },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        let mut out: Vec<usize> = lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("jsdoc/require-returns"))
            .map(|d| code[..d.span.start as usize].matches('\n').count() + 1)
            .collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn selector_contexts() {
        let code = "/** Doc. */
function top(): string { return ''; }
function outer() {
  /** Doc. */
  function inner(): string { return ''; }
}
interface I {
  /** Doc. */
  g: () => string;
}
";
        let opts = serde_json::json!({ "contexts": [
            ":not(BlockStatement) > FunctionDeclaration",
            "TSPropertySignature > TSTypeAnnotation > TSFunctionType",
        ]});
        // `inner` sits in a block; the function-typed property's doc is found through the
        // property.
        assert_eq!(lines(code, opts), vec![1, 8]);
    }

    #[test]
    fn comment_contexts() {
        let code = "/** Doc.\n * @private\n */
function a(): string { return ''; }
/** Doc. */
function b(): string { return ''; }
";
        let opts = serde_json::json!({ "contexts": [{
            "context": "FunctionDeclaration",
            "comment": "JsdocBlock:not(*:has(JsdocTag[tag=private]))",
        }]});
        assert_eq!(lines(code, opts), vec![5]);
        let any = serde_json::json!({ "contexts": [{
            "comment": "JsdocBlock:has(JsdocTag[tag=private])",
        }]});
        assert_eq!(lines(code, any), vec![1]);
    }
}
