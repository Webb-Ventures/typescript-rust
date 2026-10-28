//! A tiny expression language for the code the bundler synthesizes (`require_x()`,
//! `(init_x(), __tsrToCjs(x_exports))`, `Promise.resolve().then(...)`). It renders either as
//! AST nodes (inside module fragments, so renaming and source maps work) or as text (for
//! chunk-level code).

use ts_r_ast::{Ast, NodeId};
use ts_r_minify::build;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Expr {
    Ident(String),
    Member(Box<Expr>, String),
    Call(Box<Expr>, Vec<Expr>),
    Seq(Vec<Expr>),
    Str(String),
    Num(u32),
    /// `{ key: value, ... }`
    Object(Vec<(String, Expr)>),
    /// `() => expr`
    Arrow(Box<Expr>),
    /// `import("path")`
    Import(String),
    /// `void 0`
    Undefined,
    /// `await e`
    Await(Box<Expr>),
}

impl Expr {
    pub fn ident(s: &str) -> Expr {
        Expr::Ident(s.to_owned())
    }

    pub fn call(name: &str, args: Vec<Expr>) -> Expr {
        Expr::Call(Box::new(Expr::ident(name)), args)
    }

    pub fn member(obj: Expr, prop: &str) -> Expr {
        Expr::Member(Box::new(obj), prop.to_owned())
    }

    /// `Promise.resolve().then(() => e)`
    pub fn promise_then(e: Expr) -> Expr {
        let resolve = Expr::Call(
            Box::new(Expr::member(Expr::ident("Promise"), "resolve")),
            Vec::new(),
        );
        Expr::Call(
            Box::new(Expr::member(resolve, "then")),
            vec![Expr::Arrow(Box::new(e))],
        )
    }

    /// `Promise.resolve().then(() => init()).then(() => e)`: waits for an `async` wrapper
    /// (and still works when `init()` returns nothing, as in a cycle).
    pub fn promise_then_after(init: Expr, e: Expr) -> Expr {
        Expr::Call(
            Box::new(Expr::member(Expr::promise_then(init), "then")),
            vec![Expr::Arrow(Box::new(e))],
        )
    }

    /// Builds the expression as nodes. `at` is the source-map position.
    pub fn build(&self, ast: &mut Ast, at: u32) -> NodeId {
        match self {
            Expr::Ident(n) => build::ident(ast, n, at),
            Expr::Member(o, p) => {
                let o = o.build(ast, at);
                build::prop_access(ast, o, p, at)
            }
            Expr::Call(c, args) => {
                let c = c.build(ast, at);
                let a: Vec<NodeId> = args.iter().map(|x| x.build(ast, at)).collect();
                build::call(ast, c, &a, at)
            }
            Expr::Seq(items) => {
                let a: Vec<NodeId> = items.iter().map(|x| x.build(ast, at)).collect();
                let s = build::seq(ast, &a, at);
                build::paren(ast, s, at)
            }
            Expr::Str(s) => build::string(ast, s, at),
            Expr::Num(n) => build::number(ast, *n as f64, at),
            Expr::Object(props) => {
                let vals: Vec<(String, NodeId)> = props
                    .iter()
                    .map(|(k, v)| (k.clone(), v.build(ast, at)))
                    .collect();
                let refs: Vec<(&str, NodeId)> =
                    vals.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                build::object(ast, &refs, at)
            }
            Expr::Arrow(body) => {
                let b = body.build(ast, at);
                let b = if matches!(**body, Expr::Object(_)) {
                    build::paren(ast, b, at)
                } else {
                    b
                };
                build::arrow_expr(ast, &[], b, at)
            }
            Expr::Import(p) => {
                let arg = build::string(ast, p, at);
                ast.add(
                    ts_r_ast::Node::ImportCall {
                        phase: ts_r_ast::ImportPhase::Evaluation,
                        arg,
                        options: None,
                    },
                    ts_r_ast::Span::new(at, at),
                )
            }
            Expr::Undefined => build::undefined(ast, at),
            Expr::Await(e) => {
                let arg = e.build(ast, at);
                ast.add(ts_r_ast::Node::Await { arg }, ts_r_ast::Span::new(at, at))
            }
        }
    }

    /// Renders as JavaScript text. `compact` drops optional spaces.
    pub fn render(&self, out: &mut String, compact: bool) {
        match self {
            Expr::Ident(n) => out.push_str(n),
            Expr::Member(o, p) => {
                o.render_operand(out, compact);
                if ts_r_codegen::literals::is_identifier_name(p) {
                    out.push('.');
                    out.push_str(p);
                } else {
                    out.push('[');
                    quote(p, out);
                    out.push(']');
                }
            }
            Expr::Call(c, args) => {
                c.render_operand(out, compact);
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(if compact { "," } else { ", " });
                    }
                    a.render_arg(out, compact);
                }
                out.push(')');
            }
            Expr::Seq(items) => {
                out.push('(');
                for (i, a) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(if compact { "," } else { ", " });
                    }
                    a.render_arg(out, compact);
                }
                out.push(')');
            }
            Expr::Str(s) => quote(s, out),
            Expr::Num(n) => out.push_str(&n.to_string()),
            Expr::Object(props) => {
                if props.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str(if compact { "{" } else { "{ " });
                for (i, (k, v)) in props.iter().enumerate() {
                    if i > 0 {
                        out.push_str(if compact { "," } else { ", " });
                    }
                    if k == "__proto__" {
                        // A plain `__proto__` key would set the prototype.
                        out.push_str("[\"__proto__\"]");
                    } else if ts_r_codegen::literals::is_identifier_name(k) {
                        out.push_str(k);
                    } else {
                        quote(k, out);
                    }
                    out.push_str(if compact { ":" } else { ": " });
                    v.render_arg(out, compact);
                }
                out.push_str(if compact { "}" } else { " }" });
            }
            Expr::Arrow(body) => {
                out.push_str(if compact { "()=>" } else { "() => " });
                if matches!(**body, Expr::Object(_)) {
                    out.push('(');
                    body.render(out, compact);
                    out.push(')');
                } else {
                    body.render_arg(out, compact);
                }
            }
            Expr::Import(p) => {
                out.push_str("import(");
                quote(p, out);
                out.push(')');
            }
            Expr::Undefined => out.push_str("void 0"),
            Expr::Await(e) => {
                out.push_str("await ");
                e.render_operand(out, compact);
            }
        }
    }

    pub fn to_text(&self, compact: bool) -> String {
        let mut s = String::new();
        self.render(&mut s, compact);
        s
    }

    /// As a callee or member object (needs parens unless primary).
    fn render_operand(&self, out: &mut String, compact: bool) {
        match self {
            Expr::Arrow(_) | Expr::Object(_) | Expr::Undefined | Expr::Await(_) => {
                out.push('(');
                self.render(out, compact);
                out.push(')');
            }
            _ => self.render(out, compact),
        }
    }

    /// As an argument / property value (a sequence must be parenthesized; `Seq` already is).
    fn render_arg(&self, out: &mut String, compact: bool) {
        self.render(out, compact);
    }
}

fn quote(s: &str, out: &mut String) {
    ts_r_codegen::literals::quote_string(s, '"', out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text() {
        let e = Expr::Seq(vec![
            Expr::call("init_a", vec![]),
            Expr::call("__tsrToCjs", vec![Expr::ident("a_exports")]),
        ]);
        assert_eq!(e.to_text(false), "(init_a(), __tsrToCjs(a_exports))");
        let p = Expr::promise_then(Expr::Object(vec![]));
        assert_eq!(p.to_text(true), "Promise.resolve().then(()=>({}))");
        let m = Expr::member(Expr::ident("import_x"), "a-b");
        assert_eq!(m.to_text(true), "import_x[\"a-b\"]");
    }
}
