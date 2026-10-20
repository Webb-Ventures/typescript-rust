//! JavaScript for the validator IR: a compiled predicate per declaration (the fast path, no
//! allocation) and a descriptor per declaration (walked by the runtime only after the
//! predicate fails, to build the `ParseError`).

use std::fmt::Write as _;

use crate::eval::{Decl, G, Lit, Obj, Tuple, subst};

/// A JS string literal.
pub(crate) fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn js_lit(l: &Lit) -> String {
    match l {
        Lit::Str(s) => js_str(s),
        Lit::Num(n) => {
            let s = ts_r_binder::js_number_to_string(*n);
            if s.starts_with('-') {
                format!("({s})")
            } else {
                s
            }
        }
        Lit::Bool(b) => b.to_string(),
        Lit::BigInt(b) => format!("({b}n)"),
    }
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn access(v: &str, key: &str) -> String {
    if is_ident(key) {
        format!("{v}.{key}")
    } else {
        format!("{v}[{}]", js_str(key))
    }
}

/// `g` with references expanded until it is not a reference (bounded).
fn deref(decls: &[Decl], g: &G) -> G {
    let mut g = g.clone();
    for _ in 0..16 {
        match &g {
            G::Ref(id, args) => match decls.get(*id).and_then(|d| d.body.as_ref()) {
                Some(b) => g = subst(b, args),
                None => return g,
            },
            _ => return g,
        }
    }
    g
}

fn has_param(g: &G) -> bool {
    match g {
        G::Param(_) => true,
        G::Obj(o) => {
            o.props.iter().any(|p| has_param(&p.ty)) || o.index.as_ref().is_some_and(has_param)
        }
        G::Arr(e) | G::Set(e) => has_param(e),
        G::Map(k, v) => has_param(k) || has_param(v),
        G::Tuple(t) => t.elems.iter().any(has_param) || t.rest.as_ref().is_some_and(has_param),
        G::Union(ms) => ms.iter().any(has_param),
        G::Ref(_, a) => a.iter().any(has_param),
        G::Refine(b, _) => has_param(b),
        _ => false,
    }
}

/// How a validator function value is passed to a runtime loop (`$isArr`, `$rec`, ...).
enum FnArg {
    /// A shared primitive check (`$str`): ignores the depth.
    Prim(&'static str),
    /// A declaration's predicate by name: must be passed the depth plus one.
    Ref(String),
    /// Anything else: an arrow taking the current depth.
    Other,
}

/// A discriminated union (see [`Emitter::split`]).
struct Split {
    key: String,
    /// Member index, object type, tags.
    objs: Vec<(usize, Obj, Vec<Lit>)>,
    /// Members that are not object types.
    others: Vec<usize>,
}

/// The literal types property `key` of `o` has (required, literals only).
fn tag_lits(o: &Obj, key: &str) -> Option<Vec<Lit>> {
    let p = o.props.iter().find(|p| p.key == key && !p.optional)?;
    match &p.ty {
        G::Lit(l) => Some(vec![l.clone()]),
        G::Union(xs) => xs
            .iter()
            .map(|x| match x {
                G::Lit(l) => Some(l.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

pub(crate) struct Emitter<'d> {
    decls: &'d [Decl],
    /// The JS name of each declaration's functions (`$is_<name>`, `$d_<name>`).
    names: Vec<String>,
    fresh: u32,
    /// Template literal patterns, each one `const $reN = /../;` shared by the predicates and
    /// descriptors of this output; and whether it has been written yet.
    regexes: std::cell::RefCell<Vec<(String, bool)>>,
    /// Emit descriptors (`$d_X`). Off for inline output with `explain = "none"` when no
    /// form converts its input: nothing would read them.
    pub descriptors: bool,
    /// Per user class (`G::Class`): its JS binding (`$c_<Name>`, imported, or the class's own
    /// name in the file that declares it) and its name for messages.
    pub classes: Vec<(String, String)>,
    /// Per refinement (`G::Refine`): the predicate's binding (`$r_<Type>`) and the type name.
    pub refines: Vec<(String, String)>,
}

impl<'d> Emitter<'d> {
    pub fn new(decls: &'d [Decl]) -> Emitter<'d> {
        Emitter {
            decls,
            names: decls.iter().map(|d| d.name.clone()).collect(),
            fresh: 0,
            regexes: Default::default(),
            descriptors: true,
            classes: Vec::new(),
            refines: Vec::new(),
        }
    }

    /// An emitter that names declaration `i` `names[i]`.
    pub fn with_names(decls: &'d [Decl], names: Vec<String>) -> Emitter<'d> {
        Emitter {
            decls,
            names,
            fresh: 0,
            regexes: Default::default(),
            descriptors: true,
            classes: Vec::new(),
            refines: Vec::new(),
        }
    }

    /// Sets the bindings of user classes and refinement predicates (see the fields).
    pub fn with_bindings(
        mut self,
        classes: Vec<(String, String)>,
        refines: Vec<(String, String)>,
    ) -> Emitter<'d> {
        self.classes = classes;
        self.refines = refines;
        self
    }

    /// `(binding, shown name)` of class `i`.
    fn class(&self, i: usize) -> (String, String) {
        self.classes
            .get(i)
            .cloned()
            .unwrap_or_else(|| (format!("$c_{i}"), "object".into()))
    }

    /// `(binding, type name)` of refinement `i`.
    fn refine(&self, i: usize) -> (String, String) {
        self.refines
            .get(i)
            .cloned()
            .unwrap_or_else(|| (format!("$r_{i}"), "refined".into()))
    }

    fn name(&self, id: usize) -> &str {
        &self.names[id]
    }

    fn var(&mut self) -> String {
        self.fresh += 1;
        format!("x{}", self.fresh)
    }

    /// Whether `g` may accept `undefined` (so a required property also needs `$has`).
    fn may_be_undefined(&self, g: &G, depth: u32) -> bool {
        if depth > 16 {
            return true;
        }
        match g {
            G::Undef | G::Unknown | G::Param(_) => true,
            G::Union(ms) => ms.iter().any(|m| self.may_be_undefined(m, depth + 1)),
            G::Refine(b, _) => self.may_be_undefined(b, depth + 1),
            G::Ref(..) => {
                let d = deref(self.decls, g);
                // A reference that stays one (still being evaluated): assume it may.
                matches!(d, G::Ref(..)) || self.may_be_undefined(&d, depth + 1)
            }
            _ => false,
        }
    }

    fn fn_arg(&self, g: &G) -> FnArg {
        match g {
            G::Str => FnArg::Prim("$str"),
            G::Num => FnArg::Prim("$num"),
            G::Bool => FnArg::Prim("$bool"),
            G::Ref(id, a) if a.is_empty() => FnArg::Ref(format!("$is_{}", self.name(*id))),
            _ => FnArg::Other,
        }
    }

    /// Predicate function values for the elements `gs` of a runtime loop and the depth to
    /// pass the loop: shared primitive checks and declaration predicates go by name (the loop
    /// then gets `d + 1` when a declaration is among them), anything else is an arrow.
    fn fn_args(&mut self, gs: &[&G]) -> (Vec<String>, &'static str) {
        let args: Vec<FnArg> = gs.iter().map(|g| self.fn_arg(g)).collect();
        let other = args.iter().any(|a| matches!(a, FnArg::Other));
        let refs = args.iter().any(|a| matches!(a, FnArg::Ref(_)));
        let mut fns = Vec::with_capacity(gs.len());
        for (a, g) in args.into_iter().zip(gs) {
            fns.push(match a {
                FnArg::Prim(p) => p.to_owned(),
                FnArg::Ref(name) if !other => name,
                FnArg::Ref(_) | FnArg::Other => self.pred_fn(g),
            });
        }
        (fns, if refs && !other { "d + 1" } else { "d" })
    }

    /// A predicate function value for `g` (`(x, d) => ...`; references add one to the depth
    /// there).
    fn pred_fn(&mut self, g: &G) -> String {
        match g {
            G::Param(i) => format!("t{i}"),
            _ => {
                let x = self.var();
                let body = self.pred(g, &x);
                format!("({x}, d) => {body}")
            }
        }
    }

    /// A JS boolean expression: `v` (an expression without side effects) has type `g`; `d` is
    /// the current depth in scope. A single operand or an `&&` chain, so it can stand as any
    /// operand of `&&` or `||`; `||` chains come parenthesised.
    pub fn pred(&mut self, g: &G, v: &str) -> String {
        match g {
            G::Unknown => "true".into(),
            G::Str => format!("typeof {v} === \"string\""),
            G::Num => format!("typeof {v} === \"number\""),
            G::Bool => format!("typeof {v} === \"boolean\""),
            G::BigInt => format!("typeof {v} === \"bigint\""),
            G::Sym => format!("typeof {v} === \"symbol\""),
            G::Null => format!("{v} === null"),
            G::Undef => format!("{v} === undefined"),
            G::Lit(l) => format!("{v} === {}", js_lit(l)),
            G::Obj(o) => self.obj(o, v, true, None),
            G::Arr(e) => {
                let (f, d) = self.fn_args(&[e]);
                format!("$isArr({v}, {}, {d})", f[0])
            }
            G::Tuple(t) => self.tuple(t, v),
            G::Union(ms) => self.union(ms, v),
            G::Ref(id, args) => {
                let mut s = format!("$is_{}({v}, d + 1", self.name(*id));
                for a in args {
                    let f = self.pred_fn(a);
                    s.push_str(", ");
                    s.push_str(&f);
                }
                s.push(')');
                s
            }
            G::Param(i) => format!("t{i}({v}, d)"),
            G::Inst(name) => match *name {
                "Date" | "RegExp" => format!("{v} instanceof {name}"),
                _ => format!("typeof {name} === \"function\" && {v} instanceof {name}"),
            },
            G::Map(k, x) => {
                let (f, d) = self.fn_args(&[k, x]);
                format!("$isMap({v}, {}, {}, {d})", f[0], f[1])
            }
            G::Set(x) => {
                let (f, d) = self.fn_args(&[x]);
                format!("$isSet({v}, {}, {d})", f[0])
            }
            G::Template(re, _) => {
                format!("typeof {v} === \"string\" && {}.test({v})", self.regex(re))
            }
            G::Class(i) => format!("{v} instanceof {}", self.class(*i).0),
            G::Refine(b, r) => {
                let base = self.pred(b, v);
                let f = self.refine(*r).0;
                if base == "true" {
                    format!("{f}({v})")
                } else {
                    format!("{base} && {f}({v})")
                }
            }
        }
    }

    /// The checks of object type `o` on `v`: the shape (`$obj`) when `shape`, then each
    /// property except `skip` (a discriminant the enclosing chain already compared).
    fn obj(&mut self, o: &Obj, v: &str, shape: bool, skip: Option<&str>) -> String {
        let mut parts = Vec::new();
        if shape {
            parts.push(format!("$obj({v})"));
        }
        for p in &o.props {
            if skip == Some(p.key.as_str()) {
                continue;
            }
            let a = access(v, &p.key);
            let check = self.pred(&p.ty, &a);
            let k = js_str(&p.key);
            // `in`, not `$has`: V8 inlines it (twice the speed of `Object.hasOwn` on a
            // missing key); the two differ only for inherited keys, which JSON never has.
            if p.optional {
                parts.push(format!("({a} === undefined && !({k} in {v}) || {check})"));
            } else if self.may_be_undefined(&p.ty, 0) {
                parts.push(format!("{k} in {v} && {check}"));
            } else {
                parts.push(check);
            }
        }
        if let Some(i) = &o.index {
            let (f, d) = self.fn_args(&[i]);
            parts.push(format!("$rec({v}, {}, {d})", f[0]));
        }
        if parts.is_empty() {
            return "true".into();
        }
        parts.join(" && ")
    }

    fn tuple(&mut self, t: &Tuple, v: &str) -> String {
        let n = t.elems.len();
        let mut parts = vec![format!("Array.isArray({v})")];
        if t.required > 0 {
            parts.push(format!("{v}.length >= {}", t.required));
        }
        if t.rest.is_none() {
            parts.push(format!("{v}.length <= {n}"));
        }
        for (i, e) in t.elems.iter().enumerate() {
            let a = format!("{v}[{i}]");
            let c = self.pred(e, &a);
            if i < t.required {
                parts.push(c);
            } else {
                parts.push(format!("({v}.length <= {i} || {c})"));
            }
        }
        if let Some(r) = &t.rest {
            let (f, d) = self.fn_args(&[r]);
            parts.push(format!("$isArr({v}, {}, {d}, {n})", f[0]));
        }
        parts.join(" && ")
    }

    fn union(&mut self, ms: &[G], v: &str) -> String {
        if let Some(chain) = self.discriminated(ms, v) {
            return chain;
        }
        // Cheap checks first (`null`, literals, primitives), then the rest in written order.
        let cheap = |m: &&G| {
            matches!(
                m,
                G::Null | G::Undef | G::Lit(_) | G::Str | G::Num | G::Bool | G::BigInt | G::Sym
            )
        };
        let ordered: Vec<&G> = ms
            .iter()
            .filter(cheap)
            .chain(ms.iter().filter(|m| !cheap(m)))
            .collect();
        let parts: Vec<String> = ordered.into_iter().map(|m| self.pred(m, v)).collect();
        format!("({})", parts.join(" || "))
    }

    /// A conditional chain on a discriminant (`v.type === "a" ? .. : v.type === "b" && ..`)
    /// when two or more object members each give one property distinct literal types: the
    /// discriminant is compared like a `switch`, and each branch checks only the rest of its
    /// member.
    fn discriminated(&mut self, ms: &[G], v: &str) -> Option<String> {
        let Split { key, objs, others } = self.split(ms)?;
        let tag = access(v, &key);
        let mut chain = String::new();
        let last = objs.len() - 1;
        for (n, (i, o, lits)) in objs.iter().enumerate() {
            let conds: Vec<String> = lits
                .iter()
                .map(|l| format!("{tag} === {}", js_lit(l)))
                .collect();
            let cond = if conds.len() == 1 {
                conds.concat()
            } else {
                format!("({})", conds.join(" || "))
            };
            let check = match &ms[*i] {
                G::Ref(..) => self.pred(&ms[*i], v),
                _ => self.obj(o, v, false, Some(&key)),
            };
            if n < last {
                let _ = write!(chain, "{cond} ? {check} : ");
            } else if check == "true" {
                chain.push_str(&cond);
            } else {
                let _ = write!(chain, "{cond} && {check}");
            }
        }
        let disc = format!("$obj({v}) && ({chain})");
        let mut parts: Vec<String> = others.iter().map(|&i| self.pred(&ms[i], v)).collect();
        if parts.is_empty() {
            return Some(disc);
        }
        parts.push(disc);
        Some(format!("({})", parts.join(" || ")))
    }

    /// The members of a discriminated union: two or more object members (references
    /// expanded) each giving one required property distinct literal types.
    fn split(&self, ms: &[G]) -> Option<Split> {
        if ms.iter().any(has_param) {
            return None;
        }
        let mut objs: Vec<(usize, Obj)> = Vec::new();
        let mut others = Vec::new();
        for (i, m) in ms.iter().enumerate() {
            match deref(self.decls, m) {
                G::Obj(o) => objs.push((i, *o)),
                _ => others.push(i),
            }
        }
        if objs.len() < 2 {
            return None;
        }
        let key = objs[0].1.props.iter().map(|p| p.key.clone()).find(|k| {
            let mut seen: Vec<Lit> = Vec::new();
            for (_, o) in &objs {
                let Some(ls) = tag_lits(o, k) else {
                    return false;
                };
                for l in ls {
                    if seen.contains(&l) {
                        return false;
                    }
                    seen.push(l);
                }
            }
            true
        })?;
        let objs = objs
            .into_iter()
            .map(|(i, o)| {
                let ls = tag_lits(&o, &key).unwrap_or_default();
                (i, o, ls)
            })
            .collect();
        Some(Split { key, objs, others })
    }

    /// A discriminated union whose members are all object types.
    fn discriminant(&self, ms: &[G]) -> Option<Split> {
        self.split(ms).filter(|s| s.others.is_empty())
    }

    /// The descriptor of `g` (a JS expression; see `runtime.js`): a primitive is its name
    /// (`"string"`), anything else `[kind, ...data]`.
    pub fn desc(&self, g: &G) -> String {
        match g {
            G::Str => "\"string\"".into(),
            G::Num => "\"number\"".into(),
            G::Bool => "\"boolean\"".into(),
            G::BigInt => "\"bigint\"".into(),
            G::Sym => "\"symbol\"".into(),
            G::Null => "\"null\"".into(),
            G::Undef => "\"undefined\"".into(),
            G::Unknown => "[$Any]".into(),
            G::Lit(l) => format!("[$L, {}]", js_lit(l)),
            G::Obj(o) => self.obj_desc(o, None),
            G::Arr(e) => format!("[$A, {}]", self.desc(e)),
            G::Tuple(t) => {
                let els: Vec<String> = t.elems.iter().map(|e| self.desc(e)).collect();
                match &t.rest {
                    Some(r) => format!(
                        "[$T, [{}], {}, {}]",
                        els.join(", "),
                        t.required,
                        self.desc(r)
                    ),
                    None => format!("[$T, [{}], {}]", els.join(", "), t.required),
                }
            }
            G::Union(ms) => {
                if ms.iter().all(|m| matches!(m, G::Lit(_))) {
                    let ls: Vec<String> = ms
                        .iter()
                        .filter_map(|m| match m {
                            G::Lit(l) => Some(js_lit(l)),
                            _ => None,
                        })
                        .collect();
                    format!("[$L, {}]", ls.join(", "))
                } else if let Some(Split { key, objs, .. }) = self.discriminant(ms) {
                    let mut parts = vec![js_str(&key)];
                    for (m, (_, o, lits)) in ms.iter().zip(objs) {
                        let ls: Vec<String> = lits.iter().map(js_lit).collect();
                        parts.push(if ls.len() == 1 {
                            ls.concat()
                        } else {
                            format!("[{}]", ls.join(", "))
                        });
                        parts.push(match m {
                            G::Ref(..) => self.desc(m),
                            // The discriminant is matched by `$D` itself.
                            _ => self.obj_desc(&o, Some(&key)),
                        });
                    }
                    format!("[$D, {}]", parts.join(", "))
                } else {
                    let ds: Vec<String> = ms.iter().map(|m| self.desc(m)).collect();
                    // `$V` when a member is an object type one reference deep (the runtime
                    // looks no deeper), else the plain `$U`.
                    let objects = ms.iter().any(|m| match m {
                        G::Obj(_) => true,
                        G::Ref(id, args) => matches!(
                            self.decls
                                .get(*id)
                                .and_then(|d| d.body.as_ref())
                                .map(|b| subst(b, args)),
                            Some(G::Obj(_))
                        ),
                        _ => false,
                    });
                    let kind = if objects { "$V" } else { "$U" };
                    format!("[{kind}, {}]", ds.join(", "))
                }
            }
            G::Ref(id, args) => {
                let name = self.name(*id);
                let shown = js_str(&self.show(g));
                if args.is_empty() {
                    format!("[$R, {shown}, $d_{name}]")
                } else {
                    let ds: Vec<String> = args.iter().map(|a| self.desc(a)).collect();
                    format!("[$R, {shown}, $d_{name}, [{}]]", ds.join(", "))
                }
            }
            G::Param(i) => format!("t{i}"),
            G::Inst(name) => format!("[$C, \"{name}\"]"),
            G::Map(k, v) => format!("[$Map, {}, {}]", self.desc(k), self.desc(v)),
            G::Set(v) => format!("[$Set, {}]", self.desc(v)),
            G::Template(re, shown) => format!("[$X, {}, {}]", self.regex(re), js_str(shown)),
            G::Class(i) => {
                let (binding, name) = self.class(*i);
                format!("[$C, {}, {binding}]", js_str(&name))
            }
            G::Refine(b, r) => {
                let (binding, name) = self.refine(*r);
                format!("[$F, {}, {binding}, {}]", self.desc(b), js_str(&name))
            }
        }
    }

    /// The descriptor of object type `o`, without property `skip`.
    fn obj_desc(&self, o: &Obj, skip: Option<&str>) -> String {
        let mut props: Vec<String> = Vec::new();
        for p in &o.props {
            if skip == Some(p.key.as_str()) {
                continue;
            }
            if p.optional {
                props.push("0".into());
            }
            props.push(js_str(&p.key));
            props.push(self.desc(&p.ty));
        }
        match &o.index {
            Some(i) => format!("[$O, [{}], {}]", props.join(", "), self.desc(i)),
            None => format!("[$O, [{}]]", props.join(", ")),
        }
    }

    /// The constant holding pattern `re` (`$re1`).
    fn regex(&self, re: &str) -> String {
        let mut rs = self.regexes.borrow_mut();
        let i = match rs.iter().position(|(r, _)| r == re) {
            Some(i) => i,
            None => {
                rs.push((re.to_owned(), false));
                rs.len() - 1
            }
        };
        format!("$re{}", i + 1)
    }

    /// `const $reN = /../;` for patterns used since the last call.
    fn flush_regexes(&self, out: &mut String) {
        for (i, (re, written)) in self.regexes.borrow_mut().iter_mut().enumerate() {
            if !*written {
                *written = true;
                let _ = writeln!(out, "const $re{} = /{re}/;", i + 1);
            }
        }
    }

    /// How a reference is named in messages (`Page<User>`).
    fn show(&self, g: &G) -> String {
        match g {
            G::Ref(id, args) => {
                let base = self.decls[*id].name.clone();
                if args.is_empty() {
                    base
                } else {
                    let a: Vec<String> = args.iter().map(|a| self.show(a)).collect();
                    format!("{base}<{}>", a.join(", "))
                }
            }
            G::Str => "string".into(),
            G::Num => "number".into(),
            G::Bool => "boolean".into(),
            G::Null => "null".into(),
            G::Undef => "undefined".into(),
            G::Unknown => "unknown".into(),
            G::Arr(e) => format!("{}[]", self.show(e)),
            G::Lit(l) => js_lit(l),
            G::Class(i) => self.class(*i).1,
            G::Refine(_, r) => self.refine(*r).1,
            _ => "object".into(),
        }
    }

    /// The functions of every declaration (see [`Emitter::decl_function`]). A declaration
    /// whose parameters and body equal an earlier one's shares its functions
    /// (`const $is_B = $is_A, $d_B = $d_A;`).
    pub fn decl_functions(&mut self, out: &mut String, max_depth: u32) {
        for i in 0..self.decls.len() {
            let d = &self.decls[i];
            let same = d.body.as_ref().and_then(|b| {
                (0..i).find(|&j| {
                    let e = &self.decls[j];
                    e.params == d.params && e.body.as_ref() == Some(b)
                })
            });
            match same {
                Some(j) => {
                    let (n, m) = (self.name(i), self.name(j));
                    let _ = if self.descriptors {
                        writeln!(out, "const $is_{n} = $is_{m}, $d_{n} = $d_{m};")
                    } else {
                        writeln!(out, "const $is_{n} = $is_{m};")
                    };
                }
                None => self.decl_function(i, out, max_depth),
            }
        }
    }

    /// `const $is_X = (v, d, t0..) => ..;` (the predicate) and `const $d_X = (t0..) => ..;`
    /// (the descriptor) for declaration `i`.
    pub fn decl_function(&mut self, i: usize, out: &mut String, max_depth: u32) {
        let d = &self.decls[i];
        let Some(body) = &d.body else {
            return;
        };
        let params: String = (0..d.params).map(|p| format!(", t{p}")).collect();
        let dparams: Vec<String> = (0..d.params).map(|p| format!("t{p}")).collect();
        let check = self.pred(body, "v");
        let desc = self.descriptors.then(|| self.desc(body));
        self.flush_regexes(out);
        let n = self.name(i);
        let _ = writeln!(
            out,
            "const $is_{n} = (v, d{params}) => d < {max_depth} && {check};"
        );
        if let Some(desc) = desc {
            let _ = writeln!(out, "const $d_{n} = ({}) => {desc};", dparams.join(", "));
        }
    }

    /// `const <is> = (v, d) => ..;` and `const <dn> = () => ..;` for a validated type that is
    /// not a plain reference.
    pub fn entry_functions(&mut self, g: &G, is: &str, dn: &str, out: &mut String) {
        let check = self.pred(g, "v");
        let desc = self.descriptors.then(|| self.desc(g));
        self.flush_regexes(out);
        let _ = writeln!(out, "const {is} = (v, d) => {check};");
        if let Some(desc) = desc {
            let _ = writeln!(out, "const {dn} = () => {desc};");
        }
    }
}
