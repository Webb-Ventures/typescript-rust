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
    match cv {
        Cv::Tok(t) => t.t == T::Delim && sheet.text(t) == "&",
        // What the parser writes for a top-level rule left without a selector.
        Cv::Raw(r) => &**r == "&",
        _ => false,
    }
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

/// Whitespace or a combinator (`>`, `+`, `~`) between compounds.
fn is_combinator(sheet: &Sheet, cv: &Cv) -> bool {
    match cv {
        Cv::Space => true,
        Cv::Tok(t) if t.t == T::Ws => true,
        Cv::Tok(t) if t.t == T::Delim => matches!(sheet.text(t), ">" | "+" | "~"),
        _ => false,
    }
}

/// A type selector (`a`, `*`) as the first item of a compound.
fn is_type(sheet: &Sheet, cv: &Cv) -> bool {
    match cv {
        Cv::Tok(t) if t.t == T::Ident => true,
        Cv::Tok(t) if t.t == T::Delim => sheet.text(t) == "*",
        _ => false,
    }
}

/// A combinator outside the `:is(...)` groups lowering wrote (`:is(a b)` is one compound).
fn has_combinator(sheet: &Sheet, sel: &[Cv]) -> bool {
    let mut depth = 0usize;
    for c in sel {
        match c {
            Cv::Raw(r) if r.ends_with('(') => depth += 1,
            Cv::Raw(r) if &**r == ")" => depth = depth.saturating_sub(1),
            _ if depth == 0 && is_combinator(sheet, c) => return true,
            _ => {}
        }
    }
    false
}

/// `cv` with each `&` inside it (`:is(&, .b)`) replaced by `parent(k)` as written, `k`
/// counting the `&`s replaced so far.
fn replace_in<'p>(
    sheet: &Sheet,
    cv: Cv,
    parent: &dyn Fn(usize) -> &'p Vec<Cv>,
    k: &mut usize,
    out: &mut Vec<Cv>,
) {
    match cv {
        _ if is_amp(sheet, &cv) => {
            out.extend(parent(*k).iter().cloned());
            *k += 1;
        }
        Cv::Func { name, items } => {
            let mut inner = Vec::with_capacity(items.len());
            for c in items {
                replace_in(sheet, c, parent, k, &mut inner);
            }
            out.push(Cv::Func { name, items: inner });
        }
        Cv::Block { open, items } => {
            let mut inner = Vec::with_capacity(items.len());
            for c in items {
                replace_in(sheet, c, parent, k, &mut inner);
            }
            out.push(Cv::Block { open, items: inner });
        }
        other => out.push(other),
    }
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
    /// `:is()` may be written (`--supported:is-pseudo-class` not `false`).
    is_supported: bool,
    /// Positions already warned about, and the warnings.
    warned: Vec<u32>,
    warnings: Vec<CssMessage>,
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
    /// result is over the budget. Without `:is()` (`--supported:is-pseudo-class=false`) a
    /// parent list is expanded: one selector per combination of parent selectors.
    fn resolve(&mut self, parent: &List, child: List, at: &[Cv]) -> Option<List> {
        let expand = !self.is_supported && parent.selectors.len() > 1;
        let max_weight = parent
            .selectors
            .iter()
            .map(|s| weight(s))
            .max()
            .unwrap_or(0);
        let compound_weight = if expand {
            max_weight + 2
        } else {
            parent.weight() + if parent.selectors.len() > 1 { 2 } else { 0 }
        };
        let mut cost = 0usize;
        let mut found = Vec::with_capacity(child.selectors.len());
        for sel in &child.selectors {
            let n = amps(self.sheet, sel);
            found.push(n);
            let own = weight(sel);
            let per = (own.saturating_sub(n))
                .saturating_add(n.max(1).saturating_mul(compound_weight))
                .saturating_add(1);
            let copies = if expand {
                (parent.selectors.len()).saturating_pow(n.max(1).min(u32::MAX as usize) as u32)
            } else {
                1
            };
            cost = cost.saturating_add(per.saturating_mul(copies));
        }
        if !self.spend(cost.saturating_add(compound_weight), at) {
            return None;
        }
        // The parents each `&` (in order) can stand for: the whole list as `:is(...)`, or
        // one selector of it at a time.
        let whole = [parent.as_compound()];
        let choices: &[Vec<Cv>] = if expand {
            &parent.selectors
        } else if parent.selectors.len() > 1 {
            &whole
        } else {
            &parent.selectors
        };
        let mut needs_is = false;
        let mut selectors: Vec<Vec<Cv>> = Vec::new();
        let mut separators = Vec::new();
        let expanded = choices.len() > 1;
        // Parent-major, as esbuild orders them: for each choice for the first `&` (or the
        // parent of a selector without one), each child selector, then every combination
        // for its other `&`s, the earlier ones varying slower.
        for first in 0..choices.len() {
            for (i, (sel, &n)) in child.selectors.iter().zip(&found).enumerate() {
                let mut push = |out: Vec<Cv>, selectors: &mut Vec<Vec<Cv>>| {
                    if !selectors.is_empty() {
                        separators.push(match child.separators.get(i.wrapping_sub(1)) {
                            Some(s) if !expanded => s.clone(),
                            _ => vec![Cv::Raw(",".into()), Cv::Space],
                        });
                    }
                    selectors.push(out);
                };
                if n == 0 {
                    // A descendant of the parent (a leading combinator joins it to the parent).
                    let mut out = choices[first].clone();
                    if !sel.is_empty() {
                        out.push(Cv::Space);
                        out.extend(sel.iter().cloned());
                    }
                    push(out, &mut selectors);
                    continue;
                }
                let mut pick = vec![0usize; n];
                pick[0] = first;
                loop {
                    let out = self.substitute(
                        sel,
                        &|k| &choices[pick[k].min(choices.len() - 1)],
                        &mut needs_is,
                    );
                    push(out, &mut selectors);
                    let mut done = true;
                    let mut k = n;
                    while k > 1 {
                        k -= 1;
                        pick[k] += 1;
                        if pick[k] < choices.len() {
                            done = false;
                            break;
                        }
                        pick[k] = 0;
                    }
                    if done {
                        break;
                    }
                }
            }
        }
        if needs_is && !self.is_supported && !self.warned.contains(&first_pos(at)) {
            self.warned.push(first_pos(at));
            self.warnings.push(CssMessage {
                error: false,
                pos: first_pos(at),
                text: "Transforming this CSS nesting syntax is not supported in the configured target environment\nThe nesting transform for this case must generate an \":is(...)\" but the configured target environment does not support the \":is\" pseudo-class.".to_owned(),
            });
        }
        Some(List {
            selectors,
            separators,
        })
    }

    /// `sel` with its `k`-th `&` replaced by `parent(k)`, compound by compound: in the
    /// selector's first compound, a leading `&` is the parent as written; elsewhere a parent
    /// with combinators is `:is(parent)`; a parent compound joins the compound the `&` is in,
    /// its type selector first (the compound's own type selector, if it has one too, becomes
    /// `:is(type)` after it). Sets `needs_is` when it had to write an `:is()`.
    fn substitute<'p>(
        &self,
        sel: &[Cv],
        parent: &dyn Fn(usize) -> &'p Vec<Cv>,
        needs_is: &mut bool,
    ) -> Vec<Cv> {
        let sheet = self.sheet;
        let mut out = Vec::with_capacity(sel.len() + 8);
        let mut k = 0usize;
        let mut first = true;
        let mut i = 0;
        while i < sel.len() {
            if is_combinator(sheet, &sel[i]) {
                out.push(sel[i].clone());
                i += 1;
                continue;
            }
            let end = (i..sel.len())
                .find(|&j| is_combinator(sheet, &sel[j]))
                .unwrap_or(sel.len());
            let compound = &sel[i..end];
            i = end;
            let starts_with_type = compound.first().is_some_and(|c| is_type(sheet, c));
            let leading = first && !starts_with_type;
            first = false;
            if !compound.iter().any(|c| is_amp(sheet, c)) {
                // `&` inside a function (`:is(&, .b)`) is the parent as written.
                for cv in compound {
                    replace_in(sheet, cv.clone(), parent, &mut k, &mut out);
                }
                continue;
            }
            let own_type = compound.first().filter(|c| is_type(sheet, c));
            // The parent compound's type selector, from the first `&`'s parent.
            let p0 = parent(k);
            let p0_type = (!has_combinator(sheet, p0))
                .then(|| p0.first().filter(|c| is_type(sheet, c)))
                .flatten();
            if let Some(t) = p0_type {
                out.push(t.clone());
                if let Some(own) = own_type {
                    out.push(Cv::Raw(":is(".into()));
                    out.push(own.clone());
                    out.push(Cv::Raw(")".into()));
                    *needs_is = true;
                }
            } else if let Some(own) = own_type {
                out.push(own.clone());
            }
            let mut first_amp_seen = false;
            // The `&`s come right after the type selector, then the compound's other parts
            // (`b.q&` under `a.p` is `a:is(b).p.q`). `&`s inside functions count after them.
            let mut order: Vec<usize> = (0..compound.len())
                .filter(|&j| is_amp(sheet, &compound[j]))
                .collect();
            let amp_count = order.len();
            order.extend(
                (0..compound.len())
                    .filter(|&j| !(is_amp(sheet, &compound[j]) || (j == 0 && own_type.is_some()))),
            );
            for (pos, &j) in order.iter().enumerate() {
                let cv = &compound[j];
                if pos >= amp_count {
                    replace_in(sheet, cv.clone(), parent, &mut k, &mut out);
                    continue;
                }
                let leading = leading && pos == 0;
                let p = parent(k);
                k += 1;
                if has_combinator(sheet, p) {
                    if leading {
                        out.extend(p.iter().cloned());
                    } else {
                        out.push(Cv::Raw(":is(".into()));
                        out.extend(p.iter().cloned());
                        out.push(Cv::Raw(")".into()));
                        *needs_is = true;
                    }
                    continue;
                }
                // A compound: its type selector went first (only the first `&`'s).
                let skip_type = !first_amp_seen && p0_type.is_some();
                first_amp_seen = true;
                let rest = if skip_type { &p[1..] } else { &p[..] };
                if !skip_type && p.first().is_some_and(|c| is_type(sheet, c)) {
                    // A second type selector in the same compound.
                    out.push(Cv::Raw(":is(".into()));
                    out.push(p[0].clone());
                    out.push(Cv::Raw(")".into()));
                    out.extend(p[1..].iter().cloned());
                    *needs_is = true;
                } else {
                    out.extend(rest.iter().cloned());
                }
            }
        }
        out
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
                        // A top-level `&` is `:scope` (observed in esbuild 0.28.2).
                        None if amps(self.sheet, &prelude) > 0 => {
                            let scope = [Cv::Raw(":scope".into())];
                            List {
                                selectors: own
                                    .selectors
                                    .into_iter()
                                    .map(|sel| {
                                        let mut out = Vec::with_capacity(sel.len());
                                        replace_amps(self.sheet, sel, &scope, &mut out);
                                        out
                                    })
                                    .collect(),
                                separators: own.separators,
                            }
                        }
                        None => own,
                    };
                    let rewritten = parent.is_some() || empty || amps(self.sheet, &prelude) > 0;
                    let (decls, nested): (Vec<Node>, Vec<Node>) = body
                        .into_iter()
                        .partition(|n| matches!(n, Node::Decl { .. }));
                    if !decls.is_empty() || nested.is_empty() {
                        let prelude = if rewritten {
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
pub fn lower(sheet: &mut Sheet, is_supported: bool) {
    let rules = std::mem::take(&mut sheet.rules);
    let mut out = Vec::with_capacity(rules.len());
    let mut l = Lowerer {
        sheet,
        left: budget(sheet.src.len()),
        over: None,
        is_supported,
        warned: Vec::new(),
        warnings: Vec::new(),
    };
    l.lower_list(rules, None, &mut out);
    let over = l.over;
    let warnings = std::mem::take(&mut l.warnings);
    sheet.rules = out;
    sheet.messages.extend(warnings);
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
        super::lower(&mut s, true);
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

    fn lowered_without_is(src: &str) -> (String, usize) {
        let mut s = parse(Arc::from(src), NameMode::Global);
        super::lower(&mut s, false);
        let warnings = s.messages.iter().filter(|m| !m.error).count();
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
        (out, warnings)
    }

    #[test]
    fn nesting_selector_joins_compounds() {
        // As esbuild 0.28.2 prints them (observed).
        assert_eq!(
            lowered(
                "a.p { .x b& { x: y } } .p { b& { x: y } } a b { .c &.d { x: y } } a > b { & + & { x: y } } a { *& { x: y } } &, a { x: y }",
                NameMode::Global
            ),
            ".x a:is(b).p{x:y}b.p{x:y}.c :is(a b).d{x:y}a>b + :is(a>b){x:y}a:is(*){x:y}:scope,a{x:y}"
        );
    }

    #[test]
    fn nesting_without_is_expands_parent_lists() {
        let (out, warnings) =
            lowered_without_is("a, b { c, d { & e, f & { x: y } } } .a, .b .c { & > & { x: y } }");
        assert_eq!(
            out,
            "a c e,f :is(a c),a d e,f :is(a d),b c e,f :is(b c),b d e,f :is(b d){x:y}.a>.a,.a>:is(.b .c),.b .c>.a,.b .c>:is(.b .c){x:y}"
        );
        // One warning per rule that still needs `:is()`.
        assert_eq!(warnings, 2);
    }

    #[test]
    fn doubling_nesting_stops_at_the_budget() {
        let src = format!("a {{{} x: y {}", "& & {".repeat(30), "}".repeat(31));
        let mut s = parse(Arc::from(src.as_str()), NameMode::Global);
        super::lower(&mut s, true);
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
