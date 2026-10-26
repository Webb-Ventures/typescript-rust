//! `jsdoc/ts-method-signature-style`: object types in tag types use method signatures
//! (`"method"`) or function properties (`"property"`, the default).
//!
//! Every tag that may carry a type (but `@import`) and has one is parsed in typescript mode
//! and walked in the type parser's visiting order. With `"method"`, a function type that is
//! the value of an object field with a plain key reports, and so does each function type in
//! a field whose value is an intersection made only of parenthesized function types (one
//! report per function, all with the same fix: the field becomes one method signature per
//! function). With `"property"`, a method signature reports; its fix turns every method of
//! that name in the object into the field `name: (params) => ret`, or for several, an
//! intersection of the parenthesized function types.
//!
//! The plugin applies each fix to the parsed type as it reports, while its walk goes on, so
//! later reports see (and their fixes include) earlier changes: method signatures merged
//! into one field are not visited again. Without the fixer nothing changes and each one
//! reports. The fixed type is written back like the plugin's `rewireByParsedType`.

use super::jsdoc_ts_no_empty_object_type::might_have_type;
use super::prelude::*;
use crate::jsdoc_type::{self, FieldKey, Mode, N, Quote, SigKind, TypeTree};
use crate::jsdoc_util::{each_block, rewire_type, structured_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/ts-method-signature-style",
        "Prefers either function properties or method signatures",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TsMethodSignatureStyle>,
};

const TO_METHOD: &str = "Found function property; prefer method signature.";
const TO_PROPERTY: &str = "Found method signature; prefer function property.";
/// Nesting the walk follows; deeper types are left alone.
const MAX_DEPTH: u32 = 200;

#[derive(Debug)]
struct TsMethodSignatureStyle {
    method: bool,
    fixer: bool,
}

impl FromOptions for TsMethodSignatureStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let method = match opts.first() {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) if s == "method" => true,
            Some(Value::String(s)) if s == "property" => false,
            Some(v) => {
                return Err(format!(
                    "jsdoc/ts-method-signature-style: invalid style {v}"
                ));
            }
        };
        Ok(TsMethodSignatureStyle {
            method,
            fixer: bool_field(obj_opt(opts, 1), "enableFixer", true),
        })
    }
}

struct Walk<'s> {
    rule: &'s TsMethodSignatureStyle,
    tree: TypeTree,
    /// The parent of each node as first seen (ids of added nodes have none).
    parents: Vec<Option<usize>>,
    /// One entry per report: whether the tree changed for it (then its fix shows the tree).
    reports: Vec<Option<String>>,
}

/// A function type's type parameters, parameters and return type.
type Parts = (Option<Vec<usize>>, Vec<usize>, Option<usize>);

fn function_parts(n: &N) -> Option<Parts> {
    match n {
        N::Function {
            type_params,
            params,
            ret,
            ..
        } => Some((type_params.clone(), params.clone(), *ret)),
        _ => None,
    }
}

impl Walk<'_> {
    fn add(&mut self, n: N) -> usize {
        self.tree.nodes.push(n);
        self.parents.push(None);
        self.tree.nodes.len() - 1
    }

    fn report(&mut self, changed: bool) {
        let text = (changed && self.rule.fixer).then(|| self.tree.stringify());
        self.reports.push(text);
    }

    fn method_from(&mut self, func: usize, name: &str, quote: Quote) -> Option<usize> {
        let (type_params, params, ret) = function_parts(&self.tree.nodes[func])?;
        let ret = ret?;
        Some(self.add(N::Signature {
            kind: SigKind::Method {
                name: name.to_owned(),
                quote,
            },
            type_params,
            params,
            ret,
        }))
    }

    fn visit(&mut self, id: usize, parent: Option<usize>, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        if self.parents[id].is_none() {
            self.parents[id] = parent;
        }
        match self.tree.nodes[id].clone() {
            N::Function { .. } if self.rule.method => self.on_function(id, parent),
            N::Signature {
                kind: SigKind::Method { name, quote },
                ..
            } if !self.rule.method => self.on_method(id, parent, &name, quote),
            _ => {}
        }
        if let N::Object { elements, .. } = &self.tree.nodes[id] {
            // `forEach` over a list the visit may shorten or rewrite.
            let len = elements.len();
            for k in 0..len {
                let Some(&el) = (match &self.tree.nodes[id] {
                    N::Object { elements, .. } => elements.get(k),
                    _ => None,
                }) else {
                    break;
                };
                self.visit(el, Some(id), depth + 1);
            }
            return;
        }
        for (c, _) in self.tree.children(id) {
            self.visit(c, Some(id), depth + 1);
        }
    }

    fn field_key(&self, field: usize) -> Option<(String, Quote)> {
        match &self.tree.nodes[field] {
            N::ObjectField {
                key: FieldKey::Text(k),
                quote,
                ..
            } => Some((k.clone(), *quote)),
            _ => None,
        }
    }

    fn replace_in_object(&mut self, object: usize, old: usize, new: &[usize]) {
        if let N::Object { elements, .. } = &mut self.tree.nodes[object]
            && let Some(i) = elements.iter().position(|&e| e == old)
        {
            elements.splice(i..=i, new.iter().copied());
        }
    }

    fn on_function(&mut self, id: usize, parent: Option<usize>) {
        let Some(parent) = parent else { return };
        if let Some((name, quote)) = self.field_key(parent) {
            let mut changed = false;
            if self.rule.fixer
                && let Some(object) = self.parents[parent]
                && let Some(m) = self.method_from(id, &name, quote)
            {
                self.replace_in_object(object, parent, &[m]);
                changed = true;
            }
            self.report(changed);
            return;
        }
        // `field: ((a) => x) & ((b) => y)`.
        if !matches!(self.tree.nodes[parent], N::Parenthesis(_)) {
            return;
        }
        let Some(inter) = self.parents[parent] else {
            return;
        };
        let N::Intersection(parts) = self.tree.nodes[inter].clone() else {
            return;
        };
        let Some(field) = self.parents[inter] else {
            return;
        };
        let Some((name, quote)) = self.field_key(field) else {
            return;
        };
        let mut funcs = Vec::new();
        for &p in &parts {
            match &self.tree.nodes[p] {
                N::Parenthesis(f) if matches!(self.tree.nodes[*f], N::Function { .. }) => {
                    funcs.push(*f);
                }
                _ => return,
            }
        }
        let mut changed = false;
        if self.rule.fixer
            && let Some(object) = self.parents[field]
        {
            let methods: Vec<usize> = funcs
                .iter()
                .filter_map(|&f| self.method_from(f, &name, quote))
                .collect();
            // The plugin removes the object's elements at the intersection's indexes, then
            // inserts the methods at the first of them (0).
            if let N::Object { elements, .. } = &mut self.tree.nodes[object] {
                for i in (0..parts.len()).rev() {
                    if i < elements.len() {
                        elements.remove(i);
                    }
                }
                elements.splice(0..0, methods);
            }
            changed = true;
        }
        self.report(changed);
    }

    fn on_method(&mut self, id: usize, parent: Option<usize>, name: &str, quote: Quote) {
        let Some(object) = parent.filter(|&p| matches!(self.tree.nodes[p], N::Object { .. }))
        else {
            self.report(false);
            return;
        };
        if !self.rule.fixer {
            self.report(false);
            return;
        }
        let N::Object { elements, .. } = self.tree.nodes[object].clone() else {
            return;
        };
        let mut funcs = Vec::new();
        let mut removals = Vec::new();
        for &e in &elements {
            if let N::Signature {
                kind: SigKind::Method { name: n, .. },
                type_params,
                params,
                ret,
            } = &self.tree.nodes[e]
                && n == name
            {
                funcs.push((type_params.clone(), params.clone(), *ret));
                if e != id {
                    removals.push(e);
                }
            }
        }
        let funcs: Vec<usize> = funcs
            .into_iter()
            .map(|(type_params, params, ret)| {
                self.add(N::Function {
                    type_params,
                    params,
                    ret: Some(ret),
                    arrow: true,
                    constructor: false,
                    parenthesis: true,
                })
            })
            .collect();
        let right = if funcs.len() == 1 {
            funcs[0]
        } else {
            let parts: Vec<usize> = funcs.iter().map(|&f| self.add(N::Parenthesis(f))).collect();
            self.add(N::Intersection(parts))
        };
        let field = self.add(N::ObjectField {
            key: FieldKey::Text(name.to_owned()),
            right: Some(right),
            optional: false,
            readonly: false,
            quote,
        });
        if let N::Object { elements, .. } = &mut self.tree.nodes[object] {
            elements.retain(|e| !removals.contains(e));
        }
        self.replace_in_object(object, id, &[field]);
        self.report(true);
    }
}

impl Rule for TsMethodSignatureStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let structured = structured_tags(cx);
        each_block(cx, |cx, finder, doc| {
            for ti in 0..doc.block.tags.len() {
                let tag = &doc.block.tags[ti];
                if tag.tag == "import"
                    || tag.typ.is_empty()
                    || !might_have_type(structured, &tag.tag)
                {
                    continue;
                }
                let Some(tree) = jsdoc_type::parse(&tag.typ, Mode::Typescript, false) else {
                    continue;
                };
                let root = tree.root;
                let n = tree.nodes.len();
                let mut walk = Walk {
                    rule: self,
                    tree,
                    parents: vec![None; n],
                    reports: Vec::new(),
                };
                walk.visit(root, None, 0);
                let span = doc.line_span(finder, doc.block.tag_line(ti));
                for text in walk.reports {
                    let message = if self.method { TO_METHOD } else { TO_PROPERTY };
                    let fix = text.map(|t| {
                        rewire_type(&mut doc.block, ti, &t, &doc.indent, "");
                        doc.fix()
                    });
                    let r = cx.report(span, message);
                    if let Some(f) = fix {
                        r.fix(f);
                    }
                }
            }
        });
    }
}
