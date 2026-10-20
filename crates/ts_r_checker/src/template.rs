//! Template literal types (`` `a${T}b` ``), the intrinsic string mappings (`Uppercase<T>`,
//! `Lowercase<T>`, `Capitalize<T>`, `Uncapitalize<T>`), and matching and inference of
//! string literal and template literal types against template literal patterns.
//!
//! Behaviour follows observed tsc 7.0.2 results: literals, `null`, `undefined` and nested
//! templates are folded into the texts; unions distribute (cross product, capped);
//! `string`, `number`, `bigint` and `any` placeholders and generic types stay as spans; any
//! other span type makes the whole type `string`; `` `${string}` `` is `string`.

use ts_r_ast::Node;
use ts_r_types::{SymId, TypeFlags, TypeId, TypeKind, TypeTraits};

use crate::checker::{Ck, Loc};

/// The cross product of union spans above which a template literal type becomes `string`
/// (tsc reports TS2590 there; we stay silent).
const MAX_TEMPLATE_CROSS_PRODUCT: usize = 100_000;

/// The intrinsic type aliases of the lib.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intrinsic {
    Uppercase,
    Lowercase,
    Capitalize,
    Uncapitalize,
    NoInfer,
}

/// The intrinsic an `intrinsic` type alias name denotes.
pub fn intrinsic_kind(name: &str) -> Option<Intrinsic> {
    Some(match name {
        "Uppercase" => Intrinsic::Uppercase,
        "Lowercase" => Intrinsic::Lowercase,
        "Capitalize" => Intrinsic::Capitalize,
        "Uncapitalize" => Intrinsic::Uncapitalize,
        "NoInfer" => Intrinsic::NoInfer,
        _ => return None,
    })
}

/// Applies a string mapping to a string value (JavaScript's `toUpperCase` and friends).
pub fn apply_string_mapping(kind: Intrinsic, s: &str) -> String {
    match kind {
        Intrinsic::Uppercase => s.to_uppercase(),
        Intrinsic::Lowercase => s.to_lowercase(),
        Intrinsic::Capitalize => {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        }
        Intrinsic::Uncapitalize => {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_lowercase().chain(c).collect(),
                None => String::new(),
            }
        }
        Intrinsic::NoInfer => s.to_owned(),
    }
}

/// Whether a string is a valid numeric literal for `${number}` placeholders (tsc's
/// `isValidNumberString` with `roundTripOnly = false`).
pub fn is_valid_number_string(s: &str) -> bool {
    if s.is_empty() || s.trim() != s {
        return false;
    }
    js_to_number(s).is_some_and(|v| v.is_finite())
}

/// Whether a string is a valid bigint literal for `${bigint}` placeholders.
pub fn is_valid_bigint_string(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    if t.is_empty() {
        return false;
    }
    let (digits, radix) = if let Some(d) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (d, 16)
    } else if let Some(d) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (d, 8)
    } else if let Some(d) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (d, 2)
    } else {
        (t, 10)
    };
    if radix != 10 && s.starts_with('-') {
        return false;
    }
    !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix))
}

/// JavaScript `Number(s)` for the forms a template literal placeholder accepts.
fn js_to_number(s: &str) -> Option<f64> {
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let v = if body == "Infinity" {
        f64::INFINITY
    } else if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        if neg || s.starts_with('+') {
            return None;
        }
        u64::from_str_radix(h, 16).ok()? as f64
    } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        if neg || s.starts_with('+') {
            return None;
        }
        u64::from_str_radix(o, 8).ok()? as f64
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        if neg || s.starts_with('+') {
            return None;
        }
        u64::from_str_radix(b, 2).ok()? as f64
    } else {
        if body.is_empty()
            || !body
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
            || !body.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        {
            return None;
        }
        body.parse::<f64>().ok()?
    };
    Some(if neg { -v } else { v })
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The type of a `TemplateLitType` node.
    pub fn template_type_from_node(&mut self, l: Loc) -> TypeId {
        let ast = self.ast(l.file);
        let Node::TemplateLitType { parts } = *ast.node(l.node) else {
            return TypeId::UNRESOLVED;
        };
        let mut texts: Vec<String> = Vec::new();
        let mut types: Vec<TypeId> = Vec::new();
        let mut expect_text = true;
        for &p in ast.list(parts) {
            match *ast.node(p) {
                Node::TemplateElement { cooked, .. } => {
                    if !expect_text {
                        // Two texts in a row: merge (defensive; the parser alternates).
                        if let Some(last) = texts.last_mut() {
                            last.push_str(ast.atom(cooked));
                        }
                        continue;
                    }
                    texts.push(ast.atom(cooked).to_owned());
                    expect_text = false;
                }
                _ => {
                    if expect_text {
                        texts.push(String::new());
                    }
                    types.push(self.type_from_node(Loc::new(l.file, p)));
                    expect_text = true;
                }
            }
        }
        if expect_text {
            texts.push(String::new());
        }
        if texts.len() != types.len() + 1 {
            return TypeId::UNRESOLVED;
        }
        if self.template_cross_product(&types) >= MAX_TEMPLATE_CROSS_PRODUCT {
            self.error(
                l,
                crate::diag::dg::EXPRESSION_PRODUCES_A_UNION_TYPE_THAT_IS_TOO_COMPLEX_TO_REPRESENT_2590,
                &[],
            );
            return TypeId::ERROR;
        }
        self.template_literal_type(&texts, &types)
    }

    /// The number of members a template over `types` would have (saturating).
    fn template_cross_product(&self, types: &[TypeId]) -> usize {
        let s = self.store();
        types
            .iter()
            .map(|&t| {
                if s.flags(t).contains(TypeFlags::UNION) {
                    s.members_of_union(t).len().max(1)
                } else {
                    1
                }
            })
            .try_fold(1usize, |acc, n| acc.checked_mul(n))
            .unwrap_or(usize::MAX)
    }

    /// tsc's `getTemplateLiteralType`: normalizes texts and span types.
    pub fn template_literal_type(&mut self, texts: &[String], types: &[TypeId]) -> TypeId {
        let s = self.store();
        if types.contains(&TypeId::UNRESOLVED) || types.iter().any(|&x| self.store().is_error(x)) {
            return TypeId::UNRESOLVED;
        }
        if types.iter().any(|&t| s.flags(t).contains(TypeFlags::NEVER)) {
            return TypeId::NEVER;
        }
        if let Some(ui) = types
            .iter()
            .position(|&t| s.flags(t).contains(TypeFlags::UNION))
        {
            // Too many members: tsc's error type (TS2590 is reported at a template node).
            if self.template_cross_product(types) >= MAX_TEMPLATE_CROSS_PRODUCT {
                return TypeId::UNRESOLVED;
            }
            let members = s.members_of_union(types[ui]).to_vec();
            let mut parts = Vec::with_capacity(members.len());
            let mut v = types.to_vec();
            for m in members {
                v[ui] = m;
                parts.push(self.template_literal_type(texts, &v));
            }
            return self.union(&parts);
        }
        if types.contains(&TypeId::WILDCARD) {
            return TypeId::WILDCARD;
        }
        let mut new_texts: Vec<String> = Vec::new();
        let mut new_types: Vec<TypeId> = Vec::new();
        let mut text = texts.first().cloned().unwrap_or_default();
        if !self.add_template_spans(texts, types, &mut text, &mut new_texts, &mut new_types, 0) {
            return TypeId::STRING;
        }
        if new_types.is_empty() {
            return s.string_literal(&text, false);
        }
        new_texts.push(text);
        if new_texts.iter().all(|t| t.is_empty())
            && new_types
                .iter()
                .all(|&t| s.flags(t).contains(TypeFlags::STRING))
        {
            return TypeId::STRING;
        }
        let text_ids: Vec<TypeId> = new_texts
            .iter()
            .map(|t| s.string_literal(t, false))
            .collect();
        s.template_literal(&text_ids, &new_types)
    }

    #[allow(clippy::too_many_arguments)]
    fn add_template_spans(
        &mut self,
        texts: &[String],
        types: &[TypeId],
        text: &mut String,
        new_texts: &mut Vec<String>,
        new_types: &mut Vec<TypeId>,
        depth: u32,
    ) -> bool {
        if depth > 50 {
            return false;
        }
        let s = self.store();
        for (i, &t) in types.iter().enumerate() {
            let f = s.flags(t);
            let next = texts.get(i + 1).map(String::as_str).unwrap_or("");
            if f.intersects(TypeFlags::LITERAL | TypeFlags::NULL | TypeFlags::UNDEFINED) {
                match self.template_string_for_type(t) {
                    Some(v) => text.push_str(&v),
                    None => return false,
                }
                text.push_str(next);
            } else if f.contains(TypeFlags::TEMPLATE_LITERAL) {
                let Some((tt, ty)) = s.template_parts(t) else {
                    return false;
                };
                let tt: Vec<String> = tt
                    .iter()
                    .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                    .collect();
                let ty = ty.to_vec();
                text.push_str(&tt[0]);
                if !self.add_template_spans(&tt, &ty, text, new_texts, new_types, depth + 1) {
                    return false;
                }
                text.push_str(next);
            } else if self.is_generic_index_type(t) || self.is_pattern_placeholder(t) {
                new_types.push(t);
                new_texts.push(std::mem::take(text));
                text.push_str(next);
            } else {
                return false;
            }
        }
        true
    }

    /// The text a literal type contributes to a template.
    pub fn template_string_for_type(&self, t: TypeId) -> Option<String> {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::NULL) {
            return Some("null".into());
        }
        if f.contains(TypeFlags::UNDEFINED) {
            return Some("undefined".into());
        }
        if f.contains(TypeFlags::BOOLEAN_LITERAL) {
            return Some(if s.regular(t) == TypeId::TRUE {
                "true".into()
            } else {
                "false".into()
            });
        }
        match s.kind(t) {
            TypeKind::BigIntLiteral(x) => Some(s.str(x).to_owned()),
            _ => self.literal_text(t),
        }
    }

    /// tsc's `isPatternLiteralPlaceholderType`: types that may stand in a template literal
    /// pattern (`string`, `number`, `bigint`, `any`, pattern templates and mappings).
    pub fn is_pattern_placeholder(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            let mut seen_placeholder = false;
            for m in ms {
                let mf = self.store().flags(m);
                if self.is_pattern_placeholder(m) {
                    seen_placeholder = true;
                } else if !mf.intersects(TypeFlags::OBJECT) {
                    return false;
                }
            }
            return seen_placeholder;
        }
        f.intersects(TypeFlags::ANY | TypeFlags::STRING | TypeFlags::NUMBER | TypeFlags::BIGINT)
            || self.is_pattern_literal_type(t)
    }

    /// A non-generic template literal or string mapping type (a pattern over strings).
    pub fn is_pattern_literal_type(&self, t: TypeId) -> bool {
        let s = self.store();
        s.flags(t)
            .intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
            && !s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
    }

    // ---------------------------------------------------------------------------------
    // String mappings
    // ---------------------------------------------------------------------------------

    /// tsc's `getStringMappingType`.
    pub fn string_mapping_type(&mut self, sym: SymId, t: TypeId) -> TypeId {
        let Some(kind) = intrinsic_kind(self.c.sym_name(sym)) else {
            return TypeId::UNRESOLVED;
        };
        self.string_mapping_with(sym, kind, t, 0)
    }

    fn string_mapping_with(
        &mut self,
        sym: SymId,
        kind: Intrinsic,
        t: TypeId,
        depth: u32,
    ) -> TypeId {
        let s = self.store();
        let f = s.flags(t);
        if depth > 50 || t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return TypeId::UNRESOLVED;
        }
        if f.intersects(TypeFlags::UNION | TypeFlags::NEVER) {
            return self.map_type(t, |ck, m| ck.string_mapping_with(sym, kind, m, depth + 1));
        }
        if let Some(v) = s.string_value(t)
            && f.contains(TypeFlags::STRING_LITERAL)
        {
            let mapped = apply_string_mapping(kind, v);
            return s.string_literal(&mapped, false);
        }
        if f.contains(TypeFlags::TEMPLATE_LITERAL) {
            let Some((tt, ty)) = s.template_parts(t) else {
                return t;
            };
            let mut texts: Vec<String> = tt
                .iter()
                .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                .collect();
            let mut types = ty.to_vec();
            match kind {
                Intrinsic::Uppercase | Intrinsic::Lowercase => {
                    for x in texts.iter_mut() {
                        *x = apply_string_mapping(kind, x);
                    }
                    for x in types.iter_mut() {
                        *x = self.string_mapping_with(sym, kind, *x, depth + 1);
                    }
                }
                _ => {
                    if !texts[0].is_empty() {
                        texts[0] = apply_string_mapping(kind, &texts[0]);
                    } else if let Some(first) = types.first_mut() {
                        *first = self.string_mapping_with(sym, kind, *first, depth + 1);
                    }
                }
            }
            return self.template_literal_type(&texts, &types);
        }
        if let TypeKind::StringMapping { sym: s2, .. } = s.kind(t)
            && s2 == sym
        {
            return t;
        }
        if f.intersects(TypeFlags::ANY | TypeFlags::STRING | TypeFlags::STRING_MAPPING)
            || self.is_generic_index_type(t)
        {
            return s.string_mapping(sym, t);
        }
        if self.is_pattern_placeholder(t) {
            let inner = self.template_literal_type(&[String::new(), String::new()], &[t]);
            return s.string_mapping(sym, inner);
        }
        t
    }

    /// tsc's `isMemberOfStringMapping`.
    fn is_member_of_string_mapping(&mut self, source: TypeId, target: TypeId) -> bool {
        let s = self.store();
        let tf = s.flags(target);
        if tf.contains(TypeFlags::ANY) {
            return true;
        }
        if tf.intersects(TypeFlags::STRING | TypeFlags::TEMPLATE_LITERAL) {
            return self.is_assignable(source, target);
        }
        if tf.contains(TypeFlags::STRING_MAPPING) {
            let mut stack: Vec<SymId> = Vec::new();
            let mut cur = target;
            for _ in 0..50 {
                match self.store().kind(cur) {
                    TypeKind::StringMapping { sym, ty } => {
                        stack.insert(0, sym);
                        cur = ty;
                    }
                    _ => break,
                }
            }
            let mut mapped = source;
            for sym in stack {
                mapped = self.string_mapping_type(sym, mapped);
            }
            return mapped == source && self.is_member_of_string_mapping(source, cur);
        }
        false
    }

    // ---------------------------------------------------------------------------------
    // Matching and inference
    // ---------------------------------------------------------------------------------

    /// tsc's `inferTypesFromTemplateLiteralType`: the types matched by each placeholder of
    /// `target` in `source` (a string literal or template literal type).
    pub fn infer_from_template(&mut self, source: TypeId, target: TypeId) -> Option<Vec<TypeId>> {
        let s = self.store();
        let (ttexts, ttypes) = s.template_parts(target)?;
        let ttexts: Vec<String> = ttexts
            .iter()
            .map(|&x| s.string_value(x).unwrap_or("").to_owned())
            .collect();
        let ttypes = ttypes.to_vec();
        if s.flags(source).contains(TypeFlags::STRING_LITERAL) {
            let v = s.string_value(source)?.to_owned();
            return self.infer_from_literal_parts(&[v], &[], &ttexts);
        }
        if s.flags(source).contains(TypeFlags::TEMPLATE_LITERAL) {
            let (stexts, stypes) = s.template_parts(source)?;
            let stexts: Vec<String> = stexts
                .iter()
                .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                .collect();
            let stypes = stypes.to_vec();
            if stexts == ttexts {
                let mut out = Vec::with_capacity(stypes.len());
                for (i, &st) in stypes.iter().enumerate() {
                    let tt = ttypes.get(i).copied().unwrap_or(TypeId::STRING);
                    // A type parameter span receives the source span as is.
                    if self.store().flags(tt).contains(TypeFlags::TYPE_PARAMETER) {
                        out.push(st);
                        continue;
                    }
                    let sc = self.base_constraint_or_self(st);
                    let tc = self.base_constraint_or_self(tt);
                    if self.is_assignable(sc, tc) {
                        out.push(st);
                    } else {
                        out.push(self.string_like_for(st));
                    }
                }
                return Some(out);
            }
            return self.infer_from_literal_parts(&stexts, &stypes, &ttexts);
        }
        None
    }

    fn base_constraint_or_self(&mut self, t: TypeId) -> TypeId {
        if self.store().flags(t).intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(t);
            if c != TypeId::UNKNOWN && c != TypeId::UNRESOLVED {
                return c;
            }
        }
        t
    }

    /// tsc's `getStringLikeTypeForType`.
    fn string_like_for(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        if s.flags(t)
            .intersects(TypeFlags::ANY | TypeFlags::STRING_LIKE)
        {
            return t;
        }
        self.template_literal_type(&[String::new(), String::new()], &[t])
    }

    fn infer_from_literal_parts(
        &mut self,
        stexts: &[String],
        stypes: &[TypeId],
        ttexts: &[String],
    ) -> Option<Vec<TypeId>> {
        let last_s = stexts.len() - 1;
        let s_start = &stexts[0];
        let s_end = &stexts[last_s];
        let last_t = ttexts.len() - 1;
        let t_start = &ttexts[0];
        let t_end = &ttexts[last_t];
        if (last_s == 0 && s_start.len() < t_start.len() + t_end.len())
            || !s_start.starts_with(t_start.as_str())
            || !s_end.ends_with(t_end.as_str())
        {
            return None;
        }
        let remaining_end = s_end[..s_end.len() - t_end.len()].to_owned();
        let text_at = |i: usize| -> &str {
            if i < last_s {
                &stexts[i]
            } else {
                &remaining_end
            }
        };
        let mut matches: Vec<TypeId> = Vec::new();
        let mut seg = 0usize;
        let mut pos = t_start.len();
        // `(segment, position)` matches are turned into types after the scan.
        let mut cuts: Vec<(usize, usize, usize, usize)> = Vec::new();
        for delim in ttexts.iter().take(last_t).skip(1) {
            let (ns, np) = if !delim.is_empty() {
                let mut sidx = seg;
                let mut p = pos;
                loop {
                    let txt = text_at(sidx);
                    if p <= txt.len()
                        && let Some(found) = txt[p..].find(delim.as_str())
                    {
                        p += found;
                        break;
                    }
                    sidx += 1;
                    if sidx == stexts.len() {
                        return None;
                    }
                    p = 0;
                }
                (sidx, p)
            } else if pos < text_at(seg).len() {
                let ch = text_at(seg)[pos..].chars().next().map_or(1, char::len_utf8);
                (seg, pos + ch)
            } else if seg < last_s {
                (seg + 1, 0)
            } else {
                return None;
            };
            cuts.push((seg, pos, ns, np));
            seg = ns;
            pos = np + delim.len();
        }
        cuts.push((seg, pos, last_s, text_at(last_s).len()));
        for (seg, pos, s, p) in cuts {
            let t = if s == seg {
                let txt = text_at(s);
                let piece = txt.get(pos..p.max(pos)).unwrap_or("");
                self.store().string_literal(piece, false)
            } else {
                let mut texts: Vec<String> = Vec::new();
                texts.push(stexts[seg].get(pos..).unwrap_or("").to_owned());
                for t in &stexts[seg + 1..s] {
                    texts.push(t.clone());
                }
                texts.push(text_at(s).get(..p).unwrap_or("").to_owned());
                let types = stypes[seg..s].to_vec();
                self.template_literal_type(&texts, &types)
            };
            matches.push(t);
        }
        Some(matches)
    }

    /// A template literal with its string mapping holes replaced by `string`
    /// (`` `AA${Uppercase<string>}` `` is constrained by `` `AA${string}` ``).
    pub fn template_mapping_constraint(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        let Some((tt, ty)) = s.template_parts(t) else {
            return t;
        };
        let texts: Vec<String> = tt
            .iter()
            .map(|&x| s.string_value(x).unwrap_or("").to_owned())
            .collect();
        let types: Vec<TypeId> = ty
            .iter()
            .map(|&x| {
                if s.flags(x).contains(TypeFlags::STRING_MAPPING) {
                    TypeId::STRING
                } else {
                    x
                }
            })
            .collect();
        if types[..] == ty[..] {
            return t;
        }
        self.template_literal_type(&texts, &types)
    }

    /// tsc's `isTypeMatchedByTemplateLiteralType`.
    pub fn is_matched_by_template(&mut self, source: TypeId, target: TypeId) -> bool {
        let Some(inferred) = self.infer_from_template(source, target) else {
            return false;
        };
        let Some((_, ttypes)) = self.store().template_parts(target) else {
            return false;
        };
        let ttypes = ttypes.to_vec();
        inferred
            .iter()
            .zip(ttypes.iter())
            .all(|(&r, &t)| self.is_valid_template_placeholder(r, t))
    }

    /// tsc's `isValidTypeForTemplateLiteralPlaceholder`.
    fn is_valid_template_placeholder(&mut self, source: TypeId, target: TypeId) -> bool {
        let s = self.store();
        let tf = s.flags(target);
        if tf.contains(TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(target).to_vec();
            return ms
                .into_iter()
                .all(|m| self.is_valid_template_placeholder(source, m));
        }
        if tf.contains(TypeFlags::STRING) || self.is_assignable(source, target) {
            return true;
        }
        let sf = s.flags(source);
        if sf.contains(TypeFlags::STRING_LITERAL) {
            let v = s.string_value(source).unwrap_or("").to_owned();
            return (tf.contains(TypeFlags::NUMBER) && is_valid_number_string(&v))
                || (tf.contains(TypeFlags::BIGINT) && is_valid_bigint_string(&v))
                || (tf.intersects(TypeFlags::BOOLEAN_LITERAL | TypeFlags::NULLABLE)
                    && self.template_string_for_type(target).as_deref() == Some(v.as_str()))
                || (tf.contains(TypeFlags::STRING_MAPPING)
                    && self.is_member_of_string_mapping(source, target))
                || (tf.contains(TypeFlags::TEMPLATE_LITERAL)
                    && self.is_matched_by_template(source, target));
        }
        if sf.contains(TypeFlags::TEMPLATE_LITERAL)
            && let Some((texts, types)) = s.template_parts(source)
            && texts.len() == 2
            && texts.iter().all(|&x| s.string_value(x) == Some(""))
        {
            let inner = types[0];
            return self.is_assignable(inner, target);
        }
        false
    }

    /// Inference from a string literal or template literal source to a template literal
    /// target: each placeholder receives the text it matches. A matched string inferred to a
    /// type parameter constrained to numbers, bigints, booleans and the like becomes the
    /// corresponding literal (`${infer N extends number}`).
    pub(crate) fn infer_to_template(
        &mut self,
        ctx: &mut crate::infer::InferenceContext,
        source: TypeId,
        target: TypeId,
        contra: bool,
        depth: u32,
    ) {
        let Some((ttexts, ttypes)) = self.store().template_parts(target) else {
            return;
        };
        let ttypes = ttypes.to_vec();
        // A source that cannot be matched against a target made of placeholders only
        // (`string` against `${infer F}${infer R}`) infers `never` for each of them.
        let only_placeholders = {
            let s = self.store();
            ttexts
                .iter()
                .all(|&x| s.string_value(x).is_none_or(str::is_empty))
        };
        let matches = match self.infer_from_template(source, target) {
            Some(m) => m,
            None if only_placeholders => vec![TypeId::NEVER; ttypes.len()],
            None => return,
        };
        for (i, &tt) in ttypes.iter().enumerate() {
            let src = matches.get(i).copied().unwrap_or(TypeId::NEVER);
            let s = self.store();
            if s.flags(src).contains(TypeFlags::STRING_LITERAL)
                && s.flags(tt).contains(TypeFlags::TYPE_PARAMETER)
                && let Some(pi) = ctx.index_of(tt)
                && let Some(c) = self.type_param_constraint(ctx.params[pi])
            {
                let c = self.base_constraint_or_self(c);
                if !self.store().flags(c).contains(TypeFlags::ANY)
                    && let Some(m) = self.template_placeholder_match(src, c)
                {
                    self.infer_from(ctx, m, tt, contra, depth + 1);
                    continue;
                }
            }
            self.infer_from(ctx, src, tt, contra, depth + 1);
        }
    }

    /// The type a matched string stands for under a placeholder constraint, by precedence:
    /// string-like constraints keep the string, then numbers, bigints, booleans, `undefined`
    /// and `null` (`None` when no constraint member accepts it).
    fn template_placeholder_match(&mut self, src: TypeId, constraint: TypeId) -> Option<TypeId> {
        let s = self.store();
        let text = s.string_value(src)?.to_owned();
        let members = self.union_members(constraint);
        let rank = |ck: &mut Self, m: TypeId| -> Option<(u8, TypeId)> {
            let st = ck.store();
            let f = st.flags(m);
            if f.contains(TypeFlags::STRING) {
                return Some((0, src));
            }
            if f.contains(TypeFlags::TEMPLATE_LITERAL) {
                return ck.is_matched_by_template(src, m).then_some((1, src));
            }
            if f.contains(TypeFlags::STRING_MAPPING) {
                return ck.is_member_of_string_mapping(src, m).then_some((2, src));
            }
            if f.contains(TypeFlags::STRING_LITERAL) {
                return (st.string_value(m) == Some(text.as_str())).then_some((3, m));
            }
            // A numeric string infers its literal only when the literal spells it back
            // (`"01"`, `"1.0"` and `"1e2"` infer `number`).
            if f.contains(TypeFlags::NUMBER) {
                let v = js_to_number(&text).unwrap_or(f64::NAN);
                let lit = st.number_literal(v, false);
                let exact = ck.template_string_for_type(lit).as_deref() == Some(text.as_str());
                return Some((4, if exact { lit } else { m }));
            }
            if f.contains(TypeFlags::NUMBER_LITERAL) {
                let v = js_to_number(&text).unwrap_or(f64::NAN);
                return (st.number_value(m) == Some(v)).then_some((5, m));
            }
            if f.contains(TypeFlags::BIGINT) {
                return is_valid_bigint_string(&text).then(|| (6, st.bigint_literal(&text, false)));
            }
            if f.contains(TypeFlags::BOOLEAN_LITERAL) {
                return (ck.template_string_for_type(m).as_deref() == Some(text.as_str()))
                    .then_some((8, m));
            }
            if f.intersects(TypeFlags::UNDEFINED | TypeFlags::NULL) {
                return (ck.template_string_for_type(m).as_deref() == Some(text.as_str()))
                    .then_some((9, m));
            }
            None
        };
        let mut best: Option<(u8, TypeId)> = None;
        let has_boolean = members.contains(&TypeId::TRUE) && members.contains(&TypeId::FALSE);
        for m in members {
            if let Some(r) = rank(self, m)
                && best.is_none_or(|b| r.0 < b.0)
            {
                best = Some(r);
            }
        }
        if has_boolean && best.is_none_or(|b| b.0 > 7) {
            match text.as_str() {
                "true" => return Some(TypeId::TRUE),
                "false" => return Some(TypeId::FALSE),
                _ => {}
            }
        }
        best.map(|b| b.1)
    }

    /// Relation of a source to a template literal or string mapping target that is not
    /// generic: `Some(result)` when the rule applies.
    pub fn template_target_related(&mut self, source: TypeId, target: TypeId) -> Option<bool> {
        let s = self.store();
        let tf = s.flags(target);
        if tf.contains(TypeFlags::TEMPLATE_LITERAL) {
            if s.traits(target)
                .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
                || s.traits(source)
                    .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            {
                return None;
            }
            return Some(self.is_matched_by_template(source, target));
        }
        if tf.contains(TypeFlags::STRING_MAPPING) {
            if s.traits(target)
                .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
                || s.traits(source)
                    .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            {
                return None;
            }
            if s.flags(source).contains(TypeFlags::STRING_MAPPING) {
                return match (s.kind(source), s.kind(target)) {
                    (
                        TypeKind::StringMapping { sym: a, ty: x },
                        TypeKind::StringMapping { sym: b, ty: y },
                    ) => Some(a == b && self.is_assignable(x, y)),
                    _ => Some(false),
                };
            }
            return Some(self.is_member_of_string_mapping(source, target));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_mappings() {
        assert_eq!(apply_string_mapping(Intrinsic::Uppercase, "abc"), "ABC");
        assert_eq!(apply_string_mapping(Intrinsic::Capitalize, "abc"), "Abc");
        assert_eq!(apply_string_mapping(Intrinsic::Uncapitalize, "ABC"), "aBC");
        assert_eq!(apply_string_mapping(Intrinsic::Capitalize, ""), "");
    }

    #[test]
    fn number_strings() {
        assert!(is_valid_number_string("1"));
        assert!(is_valid_number_string("-1.5"));
        assert!(is_valid_number_string("1e3"));
        assert!(is_valid_number_string("0x10"));
        assert!(!is_valid_number_string(""));
        assert!(!is_valid_number_string(" 1"));
        assert!(!is_valid_number_string("abc"));
        assert!(!is_valid_number_string("Infinity"));
        assert!(is_valid_bigint_string("123"));
        assert!(!is_valid_bigint_string("1.5"));
    }
}
