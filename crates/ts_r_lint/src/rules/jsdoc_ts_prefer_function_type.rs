//! `jsdoc/ts-prefer-function-type`: an object type whose only member is a call signature
//! should be a function type.
//!
//! Every tag that may carry a type (but `@import`) and has one is parsed in typescript mode
//! (a type that fails to parse is skipped). Each call signature that is the only element of
//! its object type reports on the tag's line. The fix (unless `enableFixer: false`) puts an
//! arrow function type (`(params) => ret`, type parameters kept) in the object's place: as
//! the whole type, or in the parent's element list (a union, a tuple, ...); a parent
//! without one gets no fix. The tag's type is rewritten as the plugin's
//! `rewireByParsedType` does, so a multi-line type ends up on the tag's line.

use super::jsdoc_ts_no_empty_object_type::might_have_type;
use super::prelude::*;
use crate::jsdoc_type::{self, Mode, N, SigKind};
use crate::jsdoc_util::{each_block, rewire_type, structured_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/ts-prefer-function-type",
        "Prefers function types over call signatures when there are no other properties.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TsPreferFunctionType>,
};

#[derive(Debug)]
struct TsPreferFunctionType {
    fixer: bool,
}

impl FromOptions for TsPreferFunctionType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(TsPreferFunctionType {
            fixer: bool_field(obj_opt(opts, 0), "enableFixer", true),
        })
    }
}

/// Mutable access to the element list of a node, for the nodes that have `elements`.
fn elements_mut(n: &mut N) -> Option<&mut Vec<usize>> {
    match n {
        N::Union(e) | N::Intersection(e) | N::Tuple(e) | N::ParameterList(e) => Some(e),
        N::Generic { elements, .. } | N::Object { elements, .. } => Some(elements),
        _ => None,
    }
}

impl Rule for TsPreferFunctionType {
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
                for (id, parent, _) in tree.walk() {
                    let N::Signature {
                        kind: SigKind::Call,
                        type_params,
                        params,
                        ret,
                    } = &tree.nodes[id]
                    else {
                        continue;
                    };
                    let Some(object) = parent else { continue };
                    let N::Object { elements, .. } = &tree.nodes[object] else {
                        continue;
                    };
                    if elements.len() != 1 {
                        continue;
                    }
                    let span = doc.line_span(finder, doc.block.tag_line(ti));
                    let mut fixed = None;
                    if self.fixer {
                        let mut t = tree.clone();
                        let func = t.nodes.len();
                        t.nodes.push(N::Function {
                            type_params: type_params.clone(),
                            params: params.clone(),
                            ret: Some(*ret),
                            arrow: true,
                            constructor: false,
                            parenthesis: true,
                        });
                        let grand = tree
                            .walk()
                            .into_iter()
                            .find(|w| w.0 == object)
                            .and_then(|w| w.1);
                        let ok = match grand {
                            None => {
                                t.root = func;
                                true
                            }
                            Some(g) => match elements_mut(&mut t.nodes[g]) {
                                Some(els) => {
                                    for e in els.iter_mut().filter(|e| **e == object) {
                                        *e = func;
                                    }
                                    true
                                }
                                None => false,
                            },
                        };
                        if ok {
                            rewire_type(&mut doc.block, ti, &t.stringify(), &doc.indent, "");
                            fixed = Some(doc.fix());
                        }
                    }
                    let r = cx.report(span, "Call signature found; function type preferred.");
                    if let Some(f) = fixed {
                        r.fix(f);
                    }
                }
            }
        });
    }
}
