//! Signature help: the signatures of the call (or `new`) whose argument list holds the
//! cursor, with the active parameter.

use super::Ctx;
use super::hover::jsdoc_markdown;
use super::locate::{name_text, node_at};
use serde_json::{Value, json};
use ts_r_ast::{Ast, Node, NodeId};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{SigId, TypeId};

/// Signatures listed at most (overload sets in lib files can be long).
const MAX_SIGNATURES: usize = 20;

pub fn signature_help(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let text = c.file(cx.file).text.as_str();
    let Some((call, callee, open, kind)) = enclosing_call(ck, cx.file, offset) else {
        return Value::Null;
    };
    let active_param = top_level_commas(text, open + 1, offset);
    let t = ck.check_expr(Loc::new(cx.file, callee));
    let sigs = ck.signatures_of(t, kind);
    if sigs.is_empty() {
        return Value::Null;
    }
    let name = callee_name(ast, callee);
    let args = match *ast.node(call) {
        Node::Call { args, .. } | Node::New { args, .. } => ast.list(args).len(),
        _ => 0,
    };
    let mut out = Vec::new();
    let mut active_sig = None;
    for (i, &sig) in sigs.iter().take(MAX_SIGNATURES).enumerate() {
        let (label, params, rest, n) = signature_label(ck, sig, &name, kind);
        if active_sig.is_none() && (rest || n > active_param as usize) && n >= args.min(n) {
            active_sig = Some(i);
        }
        let mut s = json!({"label": label, "parameters": params});
        if let Some(doc) = signature_docs(ck, sig) {
            s["documentation"] = json!({"kind": "markdown", "value": doc});
        }
        out.push(s);
    }
    let active_sig = active_sig.unwrap_or(0);
    let n_params = out[active_sig]["parameters"].as_array().map_or(0, Vec::len);
    let rest_last = sigs
        .get(active_sig)
        .is_some_and(|&s| ck.store().sig(s).has_rest());
    let active_param = if rest_last && n_params > 0 {
        (active_param as usize).min(n_params - 1)
    } else {
        active_param as usize
    };
    json!({
        "signatures": out,
        "activeSignature": active_sig,
        "activeParameter": active_param,
    })
}

/// The innermost call or `new` whose parentheses contain `offset`:
/// `(call, callee, offset of '(', kind)`.
fn enclosing_call(
    ck: &Ck<'_, '_>,
    file: FileId,
    offset: u32,
) -> Option<(NodeId, NodeId, u32, SigKind)> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_bytes();
    let parents = c.file(file).parents();
    let mut cur = node_at(ast, offset);
    for _ in 0..10_000 {
        let n = cur?;
        let (callee, type_args, kind) = match *ast.node(n) {
            Node::Call {
                callee, type_args, ..
            } => (callee, type_args, SigKind::Call),
            Node::New {
                callee,
                type_args,
                has_args: true,
                ..
            } => (callee, type_args, SigKind::Construct),
            _ => {
                cur = parents.parent(n);
                continue;
            }
        };
        // The `(` after the callee (and its type arguments).
        let after = type_args.map_or(ast.span(callee).end, |t| ast.span(t).end);
        let span = ast.span(n);
        let mut p = after as usize;
        while p < text.len() && p < span.end as usize && text[p] != b'(' {
            p += 1;
        }
        if p < text.len() && text[p] == b'(' && (p as u32) < offset {
            let closed = span.end as usize > p && text.get(span.end as usize - 1) == Some(&b')');
            if !closed || offset < span.end {
                return Some((n, callee, p as u32, kind));
            }
        }
        cur = parents.parent(n);
    }
    None
}

/// Commas at nesting depth 0 between `from` and `to` (skipping strings and comments).
fn top_level_commas(text: &str, from: u32, to: u32) -> u32 {
    let b = text.as_bytes();
    let (mut i, end) = (from as usize, (to as usize).min(b.len()));
    let mut depth = 0i32;
    let mut commas = 0;
    while i < end {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => commas += 1,
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < end && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < end && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < end && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    commas
}

fn callee_name(ast: &Ast, callee: NodeId) -> String {
    match *ast.node(callee) {
        Node::Ident { .. } => name_text(ast, callee).unwrap_or("").to_string(),
        Node::Member { prop, .. } => name_text(ast, prop).unwrap_or("").to_string(),
        Node::Paren { expr } => callee_name(ast, expr),
        _ => String::new(),
    }
}

/// `name<T>(a: A, b?: B): R`, the parameters' label offsets (UTF-16), whether the last
/// parameter is a rest parameter, and the parameter count.
fn signature_label(
    ck: &mut Ck<'_, '_>,
    sig: SigId,
    name: &str,
    kind: SigKind,
) -> (String, Vec<Value>, bool, usize) {
    let mut label = String::new();
    if kind == SigKind::Construct {
        label.push_str("new ");
    }
    label.push_str(name);
    let d = ck.store().sig(sig);
    let tps: Vec<TypeId> = ck.store().list_items(d.type_params).to_vec();
    let params = d.params.to_vec();
    let min = d.min_args as usize;
    let rest = d.has_rest();
    if !tps.is_empty() {
        let parts: Vec<String> = tps
            .iter()
            .map(|&tp| {
                let n = ck.type_to_string(tp);
                match ck.type_param_constraint(tp) {
                    Some(cn) => format!("{n} extends {}", ck.type_to_string(cn)),
                    None => n,
                }
            })
            .collect();
        label.push('<');
        label.push_str(&parts.join(", "));
        label.push('>');
    }
    label.push('(');
    let mut out = Vec::new();
    let n = params.len();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let pname = ck.store().str(p.name).to_string();
        let ty = ck.type_to_string(p.ty);
        let is_rest = rest && i == n - 1;
        let text = if is_rest {
            format!("...{pname}: {ty}")
        } else if i >= min && p.optional {
            format!("{pname}?: {ty}")
        } else {
            format!("{pname}: {ty}")
        };
        let start = utf16_len(&label);
        label.push_str(&text);
        out.push(json!({"label": [start, utf16_len(&label)]}));
    }
    label.push_str("): ");
    let r = ck.return_type_of(sig);
    let rs = ck.type_to_string(r);
    label.push_str(&rs);
    (label, out, rest, n)
}

fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// JSDoc of the signature's declaration.
fn signature_docs(ck: &Ck<'_, '_>, sig: SigId) -> Option<String> {
    let c = ck.c;
    let target = ck.store().sig(sig).target.map(|t| t.0).unwrap_or(sig);
    let (f, n) = ck.store().sig(target).decl?;
    if f as usize >= c.files.len() {
        return None;
    }
    let node = ck.node_id(n)?;
    let d = Loc::new(FileId(f), node);
    let ast = c.ast(d.file);
    let text = c.file(d.file).text.as_str();
    // The comment before the declaration (or its variable statement for `const f = () =>`).
    let mut at = d;
    for _ in 0..4 {
        match c.parent(at).map(|p| (p, c.node(p))) {
            Some((
                p,
                Node::VarDeclarator { .. } | Node::VarDecl { .. } | Node::ExportDefaultExpr { .. },
            )) => at = p,
            _ => break,
        }
    }
    let start = ast.span(at.node).start;
    let comment = ast.comments_between(0, start).last()?;
    let between = text.get(comment.span.end as usize..start as usize)?;
    if !between.trim().is_empty() {
        return None;
    }
    let raw = comment.span.text(text);
    raw.starts_with("/**").then(|| jsdoc_markdown(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_at_depth_zero() {
        let t = "f(a, [b, c], \"x,y\", g(1, 2), ";
        assert_eq!(top_level_commas(t, 2, t.len() as u32), 4);
        assert_eq!(top_level_commas("f(/* , */ a", 2, 11), 0);
    }
}
