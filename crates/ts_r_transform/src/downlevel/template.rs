//! ES2018 template literal revision: below ES2018 a tagged template with an invalid escape
//! (`` tag`\unicode` ``) becomes a call with the template object built by
//! `__makeTemplateObject(cooked, raw)` (observed):
//!
//! - `cooked` holds each segment's value, `void 0` for a segment with an invalid escape;
//!   `raw` the segments' source text (line terminators normalized to `\n`).
//! - In a module the object is cached: `tag(templateObject_1 || (templateObject_1 =
//!   __makeTemplateObject([..], [..])), x)`, with `var templateObject_1, ...;` as the last
//!   statement of the file. A script builds it at every call.
//! - Tagged templates without an invalid escape are kept.

use ts_r_ast::{Atom, BinaryOp, Modifiers, Node, NodeId, VarKind};

use super::lower::Lower;
use crate::builder::Build;
use crate::helpers::Helper;

impl Lower<'_, '_> {
    /// A tagged template literal with a segment whose escape is invalid.
    pub fn has_invalid_escape(&self, quasi: NodeId) -> bool {
        let Node::TemplateLit { parts } = self.n(quasi) else {
            return false;
        };
        self.tx.ast.list(parts).iter().any(|&p| {
            matches!(
                self.n(p),
                Node::TemplateElement {
                    invalid_escape: true,
                    ..
                }
            )
        })
    }

    /// `` tag`a\u${x}` `` → `tag(__makeTemplateObject([void 0, ""], ["a\\u", ""]), x)`.
    pub fn tagged_template(&mut self, id: NodeId) {
        let Node::TaggedTemplate { tag, quasi, .. } = self.n(id) else {
            return;
        };
        let Node::TemplateLit { parts } = self.n(quasi) else {
            return;
        };
        self.visit(tag);
        let at = self.at(id);
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        let mut exprs = Vec::new();
        for p in self.tx.items(parts) {
            let Node::TemplateElement {
                cooked: c,
                invalid_escape,
            } = self.n(p)
            else {
                self.visit(p);
                exprs.push(p);
                continue;
            };
            let value = if invalid_escape {
                self.void0(at)
            } else {
                let text = self.tx.ast.atom(c).to_owned();
                self.string(&text, at)
            };
            cooked.push(value);
            let span = self.tx.ast.span(p);
            let text = self
                .tx
                .src
                .get(span.start as usize..span.end as usize)
                .unwrap_or("")
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            raw.push(self.string(&text, at));
        }
        let c = self.array(&cooked, at);
        let r = self.array(&raw, at);
        let f = self.tx.helper_ref(Helper::MakeTemplateObject, at);
        let mut object = self.call(f, &[c, r], at);
        if self.tx.is_module {
            let name = self.tx.fresh("templateObject", true);
            self.template_objects.push(name);
            let a = self.ident_atom(name, at);
            let set = self.assign(a, object, at);
            let par = self.paren(set, at);
            let b = self.ident_atom(name, at);
            object = self.binary(b, BinaryOp::LogicalOr, par, at);
        }
        let mut args = vec![object];
        args.extend(exprs);
        let call = self.call(tag, &args, at);
        self.put(id, call);
    }

    /// `var templateObject_1, ...;` for the end of the file.
    pub fn template_objects_var(&mut self, names: &[Atom], at: u32) -> NodeId {
        let mut decls = Vec::with_capacity(names.len());
        for &n in names {
            let name = self.ident_atom(n, at);
            decls.push(self.mk(
                Node::VarDeclarator {
                    name,
                    definite: false,
                    type_ann: None,
                    init: None,
                },
                at,
            ));
        }
        let decls = self.new_list(&decls);
        self.mk(
            Node::VarDecl {
                modifiers: Modifiers::NONE,
                kind: VarKind::Var,
                decls,
            },
            at,
        )
    }
}
