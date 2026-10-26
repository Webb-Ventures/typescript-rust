//! Lowers CSS nesting (`--supported:nesting=false`), as esbuild 0.28.2 does (observed):
//!
//! ```css
//! a { color: red; .b { x: y } & > c, d { x: y } &:hover { x: y } }
//! e, f { g { x: y } }
//! ```
//! is
//! ```css
//! a { color: red }  a .b { x: y }  a > c, a d { x: y }  a:hover { x: y }
//! :is(e, f) g { x: y }
//! ```
//! A nested selector without `&` is a descendant of its parent (a leading combinator joins
//! it to the parent); each `&` is the parent, wrapped in `:is()` when the parent is a list.
//! A rule's own declarations stay in it, ahead of its nested rules (a rule left with none is
//! dropped); a nested at-rule (`@media`) moves out with the parent's selector around its
//! declarations. A top-level rule with an empty selector (what `:global { ... }` leaves in a
//! CSS module) is `:scope`.

use super::lexer::T;
use super::{Cv, Node, Sheet};

/// A selector list: each complex selector, and the tokens between them (`,` and spaces).
#[derive(Debug, Clone, Default)]
struct List {
    selectors: Vec<Vec<Cv>>,
    separators: Vec<Vec<Cv>>,
}

impl List {
    fn split(prelude: &[Cv]) -> List {
        let mut list = List::default();
        let mut cur: Vec<Cv> = Vec::new();
        let mut sep: Vec<Cv> = Vec::new();
        for cv in prelude {
            match cv {
                Cv::Tok(t) if t.t == T::Comma => {
                    list.selectors.push(trim(std::mem::take(&mut cur)));
                    sep.clear();
                    sep.push(cv.clone());
                }
                Cv::Tok(t) if t.t == T::Ws && cur.is_empty() && !list.selectors.is_empty() => {
                    sep.push(cv.clone());
                }
                _ => {
                    if cur.is_empty() && !list.selectors.is_empty() {
                        list.separators.push(std::mem::take(&mut sep));
                    }
                    cur.push(cv.clone());
                }
            }
        }
        list.selectors.push(trim(cur));
        while list.separators.len() + 1 < list.selectors.len() {
            list.separators.push(vec![Cv::Raw(",".into()), Cv::Space]);
        }
        list
    }

    fn join(&self) -> Vec<Cv> {
        let mut out = Vec::new();
        for (i, s) in self.selectors.iter().enumerate() {
            if i > 0 {
                out.extend(self.separators[i - 1].iter().cloned());
            }
            out.extend(s.iter().cloned());
        }
        out
    }

    /// The list as one compound: itself, or `:is(<list>)`.
    fn as_compound(&self) -> Vec<Cv> {
        if self.selectors.len() == 1 {
            return self.selectors[0].clone();
        }
        let mut out = vec![Cv::Raw(":is(".into())];
        out.extend(self.join());
        out.push(Cv::Raw(")".into()));
        out
    }
}

fn trim(mut v: Vec<Cv>) -> Vec<Cv> {
    let ws = |cv: &Cv| matches!(cv, Cv::Tok(t) if t.t == T::Ws);
    while v.last().is_some_and(ws) {
        v.pop();
    }
    let lead = v.iter().take_while(|c| ws(c)).count();
    v.drain(..lead);
    v
}

fn is_amp(sheet: &Sheet, cv: &Cv) -> bool {
    matches!(cv, Cv::Tok(t) if t.t == T::Delim && sheet.text(t) == "&")
}

/// `child` (a nested rule's selector list) resolved against `parent`.
fn resolve(sheet: &Sheet, parent: &List, child: List) -> List {
    let compound = parent.as_compound();
    let selectors = child
        .selectors
        .into_iter()
        .map(|sel| {
            if sel.iter().any(|c| is_amp(sheet, c)) {
                let mut out = Vec::new();
                for cv in sel {
                    if is_amp(sheet, &cv) {
                        out.extend(compound.iter().cloned());
                    } else {
                        out.push(cv);
                    }
                }
                out
            } else if sel.is_empty() {
                compound.clone()
            } else {
                let mut out = compound.clone();
                out.push(Cv::Space);
                out.extend(sel);
                out
            }
        })
        .collect();
    List {
        selectors,
        separators: child.separators,
    }
}

/// Rewrites `sheet`'s rules without nesting.
pub fn lower(sheet: &mut Sheet) {
    let rules = std::mem::take(&mut sheet.rules);
    let mut out = Vec::with_capacity(rules.len());
    lower_list(sheet, rules, None, &mut out);
    sheet.rules = out;
}

fn lower_list(sheet: &Sheet, nodes: Vec<Node>, parent: Option<&List>, out: &mut Vec<Node>) {
    // Declarations directly in a nested at-rule belong to the parent's selector.
    let mut decls = Vec::new();
    let mut rest = Vec::new();
    for n in nodes {
        match n {
            Node::Decl { .. } if parent.is_some() => decls.push(n),
            other => rest.push(other),
        }
    }
    if let Some(p) = parent
        && !decls.is_empty()
    {
        out.push(Node::Rule {
            prelude: p.join(),
            body: decls,
        });
    }
    for n in rest {
        match n {
            Node::Rule { prelude, body } => {
                let own = List::split(&prelude);
                let empty = own.selectors.len() == 1 && own.selectors[0].is_empty();
                let list = match parent {
                    Some(p) => resolve(sheet, p, own),
                    // A top-level rule left without a selector (`:global { ... }` in a CSS
                    // module) is `:scope`.
                    None if empty => List {
                        selectors: vec![vec![Cv::Raw(":scope".into())]],
                        separators: Vec::new(),
                    },
                    None => own,
                };
                let (decls, nested): (Vec<Node>, Vec<Node>) = body
                    .into_iter()
                    .partition(|n| matches!(n, Node::Decl { .. }));
                if !decls.is_empty() || nested.is_empty() {
                    out.push(Node::Rule {
                        prelude: if parent.is_some() || empty {
                            list.join()
                        } else {
                            prelude
                        },
                        body: decls,
                    });
                }
                lower_list(sheet, nested, Some(&list), out);
            }
            Node::At {
                name,
                raw,
                prelude,
                body: Some(body),
            } if !name.ends_with("keyframes") => {
                let mut inner = Vec::new();
                lower_list(sheet, body, parent, &mut inner);
                out.push(Node::At {
                    name,
                    raw,
                    prelude,
                    body: Some(inner),
                });
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::print::Printer;
    use super::super::{NameMode, parse};
    use std::sync::Arc;

    fn lowered(src: &str, mode: NameMode) -> String {
        let mut s = parse(Arc::from(src), mode);
        super::lower(&mut s);
        let mut out = String::new();
        let url = |r: u32| format!("u{r}");
        let local = |n: u32| format!("l_{}", s.locals[n as usize]);
        Printer {
            sheet: &s,
            minify: true,
            syntax: false,
            url: &url,
            local: &local,
        }
        .nodes(&s.rules, 0, &mut out);
        out
    }

    #[test]
    fn nesting_is_lowered() {
        assert_eq!(
            lowered(
                "a { color: red; .b { x: y } & > c, d { x: y } &:hover { z: w } } e, f { g { h: i } } @media x { j { k { l: m } } } n { @media y { o: p } }",
                NameMode::Global
            ),
            "a{color:red}a .b{x:y}a>c,a d{x:y}a:hover{z:w}:is(e,f) g{h:i}@media x{j k{l:m}}@media y{n{o:p}}"
        );
        assert_eq!(
            lowered(
                ":global { .a { x: y } } :local(:global) { x: y }",
                NameMode::Local
            ),
            ":scope .a{x:y}:scope{x:y}"
        );
    }
}
