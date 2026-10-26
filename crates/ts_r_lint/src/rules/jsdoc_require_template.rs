//! `jsdoc/require-template`: type parameters a block uses have a `@template` (under its
//! preferred name).
//!
//! The declared names are every `@template`'s comma-separated names (`[T=x]` gives `T`).
//! With `requireSeparateTemplates`, a `@template` declaring several names reports the second.
//! Which names count as used depends on the block:
//! - with `@callback` or `@function`: the `@param` and `@returns` types;
//! - with exactly one `@typedef`: its type and the `@property` types;
//! - otherwise the type parameters of the documented class, function, interface or type
//!   alias (directly or through `export`; `export default` only for a class, a function with
//!   a body or an interface), reported on the whole block.
//!
//! In a type, a single capital letter used as a name is a used name, and a type parameter or
//! `infer` binding declared inside the type also counts as declared. A used name without a
//! declaration reports on the line of the first tag using it. Blocks with replaced docs,
//! an `exemptedBy` tag (default `@inheritdoc`), constructors, `@class`/`@constructor`,
//! getters and setters are skipped.

use super::jsdoc_valid_types::closure_template_names;
use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Preferred, Target, preferred_tag};
use crate::jsdoc_type::{self as jt, N};
use crate::jsdoc_util::{each_block, exempted_by, has_exempting_tag, replaced_docs};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-template",
        "Requires `@template` tags be present when type parameters are used",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireTemplate>,
};

#[derive(Debug)]
struct RequireTemplate {
    opts: serde_json::Map<String, Value>,
    separate: bool,
}

impl FromOptions for RequireTemplate {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(RequireTemplate {
            separate: bool_field(o, "requireSeparateTemplates", false),
            opts: o.clone(),
        })
    }
}

/// The names in use and where they were first seen.
#[derive(Default)]
struct Names {
    declared: Vec<String>,
    /// (name, tag it was first used in); in first-use order.
    used: Vec<(String, Option<usize>)>,
}

impl Names {
    fn use_name(&mut self, name: &str, tag: Option<usize>) {
        if !self.used.iter().any(|(n, _)| n == name) {
            self.used.push((name.to_owned(), tag));
        }
    }

    /// Reads tag `ti`'s type: single-capital names are used, inner type parameters declared.
    fn scan_type(&mut self, b: &Block, ti: usize, mode: &str) {
        let Some(tree) = jt::parse_for_setting(&b.tags[ti].typ, mode, false) else {
            return;
        };
        for (id, _, _) in tree.walk() {
            match &tree.nodes[id] {
                N::TypeParameter { name, .. } | N::Infer(name) => {
                    if let N::Name(n) = &tree.nodes[*name] {
                        self.declared.push(n.clone());
                    }
                }
                N::Name(n) if n.len() == 1 && n.as_bytes()[0].is_ascii_uppercase() => {
                    self.use_name(n, Some(ti));
                }
                _ => {}
            }
        }
    }
}

/// The declaration whose type parameters the block covers (`handleTypes`).
fn declaration(f: &Finder<'_>, t: Target) -> Option<NodeId> {
    let ok = |id: NodeId, default_export: bool| match *f.ast.node(id) {
        Node::ClassDecl { .. } | Node::Interface { .. } => true,
        Node::FunctionDecl { body, .. } => !default_export || body.is_some(),
        Node::TypeAlias { .. } => !default_export,
        _ => false,
    };
    match t {
        Target::Node(id) => ok(id, false).then_some(id),
        Target::Export(id) => {
            let default_export = f.es_type(t) == "ExportDefaultDeclaration";
            ok(id, default_export).then_some(id)
        }
    }
}

fn type_param_names(f: &Finder<'_>, id: NodeId) -> Vec<String> {
    let tp = match *f.ast.node(id) {
        Node::ClassDecl { type_params, .. }
        | Node::Interface { type_params, .. }
        | Node::FunctionDecl { type_params, .. }
        | Node::TypeAlias { type_params, .. } => type_params,
        _ => None,
    };
    let Some(tp) = tp else {
        return Vec::new();
    };
    let Node::TypeParams { params } = *f.ast.node(tp) else {
        return Vec::new();
    };
    f.ast
        .list(params)
        .iter()
        .filter_map(|&p| match *f.ast.node(p) {
            Node::TypeParam { name, .. } => ident_name(f.ast, name).map(str::to_owned),
            _ => None,
        })
        .collect()
}

impl RequireTemplate {
    fn exempt(&self, f: &Finder<'_>, doc: &Doc) -> bool {
        let node = match doc.target {
            Some(Target::Node(id)) => Some(id),
            _ => None,
        };
        let kind = |n: NodeId| match *f.ast.node(n) {
            Node::MethodDef { kind, .. } => Some(kind),
            _ => None,
        };
        let own = node.and_then(kind);
        let parent = node.and_then(|id| f.parents.parent(id)).and_then(kind);
        let exempted = exempted_by(&self.opts, f.settings.mode == "closure");
        replaced_docs(f, doc)
            || own == Some(MethodKind::Constructor)
            || matches!(
                parent,
                Some(MethodKind::Constructor | MethodKind::Get | MethodKind::Set)
            )
            || doc.block.has_any_tag(&["class", "constructor"])
            || has_exempting_tag(&doc.block, &exempted)
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, f: &Finder<'_>, doc: &Doc) {
        if self.exempt(f, doc) {
            return;
        }
        let Some(tg) = doc.preferred(cx, f, "template") else {
            return;
        };
        let b = &doc.block;
        let mut names = Names::default();
        for t in b.tags.iter().filter(|t| t.tag == tg) {
            names.declared.extend(closure_template_names(&t.name));
        }
        if self.separate {
            for (ti, t) in b.tags.iter().enumerate().filter(|(_, t)| t.tag == tg) {
                if let Some(second) = closure_template_names(&t.name).get(1) {
                    let span = doc.line_span(f, b.tag_line(ti));
                    cx.report(span, format!("Missing separate @{tg} for {second}"));
                }
            }
        }
        let mode = f.settings.mode.as_str();
        let tags_named = |name: &str| -> Vec<usize> {
            let Preferred::Name(pref) = preferred_tag(f.settings, name) else {
                return Vec::new();
            };
            (0..b.tags.len())
                .filter(|&i| b.tags[i].tag == pref)
                .collect()
        };
        let report_used = |cx: &mut Ctx<'_, '_>, names: &Names| {
            for (name, tag) in &names.used {
                if names.declared.contains(name) {
                    continue;
                }
                let span = match tag {
                    Some(ti) => doc.line_span(f, b.tag_line(*ti)),
                    None => doc.comment,
                };
                cx.report(span, format!("Missing @{tg} {name}"));
            }
        };
        if b.has_tag("callback") || b.has_tag("function") {
            for name in ["param", "returns"] {
                for ti in tags_named(name) {
                    names.scan_type(b, ti, mode);
                }
            }
            report_used(cx, &names);
            return;
        }
        let typedefs: Vec<usize> = (0..b.tags.len())
            .filter(|&i| b.tags[i].tag == "typedef")
            .collect();
        if typedefs.len() != 1 {
            let Some(decl) = doc.target.and_then(|t| declaration(f, t)) else {
                return;
            };
            for n in type_param_names(f, decl) {
                names.use_name(&n, None);
            }
            report_used(cx, &names);
            return;
        }
        names.scan_type(b, typedefs[0], mode);
        for ti in tags_named("property") {
            names.scan_type(b, ti, mode);
        }
        report_used(cx, &names);
    }
}

impl Rule for RequireTemplate {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, f, doc| self.check(cx, f, doc));
    }
}
