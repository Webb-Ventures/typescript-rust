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
//! CSS module) is `:scope`. `&` inside a selector function is replaced too
//! (`:is(&, .b)` is `:is(a, .b)`). Lowering copies parent selectors, so `& &` at each level
//! doubles them: it stops with an error once it would write more than about a million
//! component values (plus four per input byte).

use super::lexer::T;
use super::{CssMessage, Cv, Node, Sheet};

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

    /// The number of component values [`List::join`] copies (nested ones included).
    fn weight(&self) -> usize {
        self.selectors.iter().map(|s| weight(s)).sum::<usize>()
            + self.separators.iter().map(|s| weight(s)).sum::<usize>()
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

/// Component values in `cvs`, those inside functions and blocks included.
fn weight(cvs: &[Cv]) -> usize {
    cvs.iter()
        .map(|cv| match cv {
            Cv::Block { items, .. } | Cv::Func { items, .. } => 1 + weight(items),
            _ => 1,
        })
        .sum()
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

/// `&` in `cvs`, inside selector functions (`:is(&, .b)`) too. The parser bounds the
/// nesting of functions, so the recursion is bounded.
fn amps(sheet: &Sheet, cvs: &[Cv]) -> usize {
    cvs.iter()
        .map(|cv| match cv {
            Cv::Func { items, .. } | Cv::Block { items, .. } => amps(sheet, items),
            _ => usize::from(is_amp(sheet, cv)),
        })
        .sum()
}

/// `cvs` with every `&` (nested ones included) replaced by `compound`.
fn replace_amps(sheet: &Sheet, cvs: Vec<Cv>, compound: &[Cv], out: &mut Vec<Cv>) {
    for cv in cvs {
        match cv {
            _ if is_amp(sheet, &cv) => out.extend(compound.iter().cloned()),
            Cv::Func { name, items } => {
                let mut inner = Vec::with_capacity(items.len());
                replace_amps(sheet, items, compound, &mut inner);
                out.push(Cv::Func { name, items: inner });
            }
            Cv::Block { open, items } => {
                let mut inner = Vec::with_capacity(items.len());
                replace_amps(sheet, items, compound, &mut inner);
                out.push(Cv::Block { open, items: inner });
            }
            other => out.push(other),
        }
    }
}

/// How many component values lowering may write for one stylesheet: nesting copies each
/// parent selector into its children, so `& &` at every level doubles the selector and 30
/// levels of it would need billions of values.
fn budget(src_len: usize) -> usize {
    (1usize << 20).saturating_add(src_len.saturating_mul(4))
}

struct Lowerer<'s> {
    sheet: &'s Sheet,
    /// Component values still allowed ([`budget`]).
    left: usize,
    /// Where the budget ran out (the first selector token of the rule).
    over: Option<u32>,
}

impl Lowerer<'_> {
    /// Takes `n` from the budget; `false` (and the error recorded) when it is used up.
    fn spend(&mut self, n: usize, at: &[Cv]) -> bool {
        if self.over.is_some() {
            return false;
        }
        match self.left.checked_sub(n) {
            Some(left) => {
                self.left = left;
                true
            }
            None => {
                self.over = Some(first_pos(at));
                false
            }
        }
    }

    /// `child` (a nested rule's selector list) resolved against `parent`; `None` when the
    /// result is over the budget.
    fn resolve(&mut self, parent: &List, child: List, at: &[Cv]) -> Option<List> {
        let multi = parent.selectors.len() > 1;
        let compound_weight = parent.weight() + if multi { 2 } else { 0 };
        let mut cost = 0usize;
        let mut found = Vec::with_capacity(child.selectors.len());
        for sel in &child.selectors {
            let n = amps(self.sheet, sel);
            found.push(n);
            let own = weight(sel);
            let c = if n > 0 {
                (own - n).saturating_add(n.saturating_mul(compound_weight))
            } else {
                compound_weight.saturating_add(own + 1)
            };
            cost = cost.saturating_add(c);
        }
        if !self.spend(cost.saturating_add(compound_weight), at) {
            return None;
        }
        let compound = parent.as_compound();
        let selectors = child
            .selectors
            .into_iter()
            .zip(found)
            .map(|(sel, n)| {
                if n > 0 {
                    let mut out = Vec::with_capacity(sel.len());
                    replace_amps(self.sheet, sel, &compound, &mut out);
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
        Some(List {
            selectors,
            separators: child.separators,
        })
    }

    /// `list.join()`, paid for from the budget.
    fn join(&mut self, list: &List, at: &[Cv]) -> Option<Vec<Cv>> {
        self.spend(list.weight(), at).then(|| list.join())
    }

    fn lower_list(&mut self, nodes: Vec<Node>, parent: Option<&List>, out: &mut Vec<Node>) {
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
            let Some(prelude) = self.join(p, &[]) else {
                return;
            };
            out.push(Node::Rule {
                prelude,
                body: decls,
            });
        }
        for n in rest {
            if self.over.is_some() {
                return;
            }
            match n {
                Node::Rule { prelude, body } => {
                    let own = List::split(&prelude);
                    let empty = own.selectors.len() == 1 && own.selectors[0].is_empty();
                    let list = match parent {
                        Some(p) => match self.resolve(p, own, &prelude) {
                            Some(l) => l,
                            None => return,
                        },
                        // A top-level rule left without a selector (`:global { ... }` in a
                        // CSS module) is `:scope`.
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
                        let prelude = if parent.is_some() || empty {
                            match self.join(&list, &prelude) {
                                Some(p) => p,
                                None => return,
                            }
                        } else {
                            prelude
                        };
                        out.push(Node::Rule {
                            prelude,
                            body: decls,
                        });
                    }
                    self.lower_list(nested, Some(&list), out);
                }
                Node::At {
                    name,
                    raw,
                    prelude,
                    body: Some(body),
                } if !name.ends_with("keyframes") => {
                    let mut inner = Vec::new();
                    self.lower_list(body, parent, &mut inner);
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
}

/// The position of the first token in `cvs` (0 when there is none).
fn first_pos(cvs: &[Cv]) -> u32 {
    cvs.iter()
        .find_map(|cv| match cv {
            Cv::Tok(t) => Some(t.start),
            Cv::Func { name, .. } => Some(name.start),
            Cv::Block { items, .. } => Some(first_pos(items)),
            _ => None,
        })
        .unwrap_or(0)
}

/// Rewrites `sheet`'s rules without nesting. When the lowered selectors would be too large
/// ([`budget`]), the sheet gets an error and lowering stops there.
pub fn lower(sheet: &mut Sheet) {
    let rules = std::mem::take(&mut sheet.rules);
    let mut out = Vec::with_capacity(rules.len());
    let mut l = Lowerer {
        sheet,
        left: budget(sheet.src.len()),
        over: None,
    };
    l.lower_list(rules, None, &mut out);
    let over = l.over;
    sheet.rules = out;
    if let Some(pos) = over {
        sheet.messages.push(CssMessage {
            error: true,
            pos,
            text: "Lowering this CSS nesting would make its selectors too large".to_owned(),
        });
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

    #[test]
    fn nesting_selector_inside_functions() {
        // As esbuild 0.28.2 prints them (observed).
        assert_eq!(
            lowered(
                "a { :is(&, .b) { color: red } } e, f { :not(&) .c { x: y } } g { :where(.h &) { x: y } }",
                NameMode::Global
            ),
            ":is(a,.b){color:red}:not(:is(e,f)) .c{x:y}:where(.h g){x:y}"
        );
    }

    #[test]
    fn doubling_nesting_stops_at_the_budget() {
        let src = format!("a {{{} x: y {}", "& & {".repeat(30), "}".repeat(31));
        let mut s = parse(Arc::from(src.as_str()), NameMode::Global);
        super::lower(&mut s);
        let errors: Vec<_> = s.messages.iter().filter(|m| m.error).collect();
        assert_eq!(errors.len(), 1, "{:?}", s.messages);
        assert!(errors[0].text.contains("too large"), "{}", errors[0].text);
        // Normal nesting is far below the budget.
        assert_eq!(
            lowered("a { & b { & c { x: y } } }", NameMode::Global),
            "a b c{x:y}"
        );
    }
}
