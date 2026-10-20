//! How a file is written (semicolons, quotes, indentation, line breaks), so generated code
//! looks like its neighbours, and line helpers on byte offsets.

use ts_r_ast::{Ast, Node, NodeId, Visitor, Walk, walk};

/// Whether the file ends statements with semicolons, judged from the first five places that
/// take one, in source order (a node before its children) at any depth: statements (not
/// those ending in `}` but type aliases, not a `for` head) and members of interfaces, type
/// literals and classes. Members and type aliases go without only when a line break follows
/// them: `{ a: T }` and `a: T, b: U` count for neither. Yes when none goes without, or when
/// with/without is above 1/5. tsserver counts type members too: a file whose statements
/// have no semicolons but whose type literals write `{ a: T; b: U }` early on gets them
/// (observed on private-app; `organize-semicolons-*` fixtures).
pub fn uses_semicolons(ast: &Ast, text: &str) -> bool {
    struct Count<'t> {
        text: &'t str,
        with: u32,
        without: u32,
        for_heads: Vec<NodeId>,
    }
    impl Visitor for Count<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if self.with + self.without >= 5 {
                return Walk::Stop;
            }
            let node = ast.node(id);
            match *node {
                Node::For { init: Some(i), .. } => self.for_heads.push(i),
                Node::ForIn { left, .. } | Node::ForOf { left, .. } => self.for_heads.push(left),
                _ => {}
            }
            let member = match node {
                Node::ImportDecl { .. }
                | Node::ImportEquals { .. }
                | Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
                | Node::ExprStmt { .. }
                | Node::Return { .. }
                | Node::Throw { .. }
                | Node::Break { .. }
                | Node::Continue { .. }
                | Node::Debugger {} => false,
                Node::VarDecl { .. } if !self.for_heads.contains(&id) => false,
                Node::PropertySignature { .. }
                | Node::MethodSignature { .. }
                | Node::CallSignature { .. }
                | Node::ConstructSignature { .. }
                | Node::IndexSignature { .. }
                | Node::PropertyDef { .. } => true,
                // `type A = { a: T }` counts as going without, though it ends in `}`.
                Node::TypeAlias { .. } => true,
                _ => return Walk::Continue,
            };
            let span = ast.span(id);
            let src = span.text(self.text).trim_end();
            let after = self
                .text
                .get(span.end as usize..)
                .unwrap_or("")
                .trim_start_matches([' ', '\t']);
            if src.ends_with(';') || after.starts_with(';') {
                self.with += 1;
            } else if if member {
                after.is_empty() || after.starts_with(['\n', '\r'])
            } else {
                !src.ends_with('}')
            } {
                self.without += 1;
            }
            Walk::Continue
        }
    }
    let mut c = Count {
        text,
        with: 0,
        without: 0,
        for_heads: Vec::new(),
    };
    walk(ast, ast.root, &mut c);
    c.without == 0 || 5 * c.with > c.without
}

/// `;` or nothing, as the file writes statements.
pub fn semi(ast: &Ast, text: &str) -> &'static str {
    if uses_semicolons(ast, text) { ";" } else { "" }
}

/// The quote of the first import's module specifier (`"` without imports).
pub fn quote(ast: &Ast, text: &str) -> char {
    for s in top_level(ast) {
        if let Node::ImportDecl { source, .. } = *ast.node(s) {
            return if ast.span(source).text(text).starts_with('\'') {
                '\''
            } else {
                '"'
            };
        }
    }
    '"'
}

/// The file's line break.
pub fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// One level of indentation: the smallest indentation of a non-blank line (a tab when lines
/// are indented with tabs), four spaces when no line is indented.
pub fn indent_unit(text: &str) -> String {
    let mut best: Option<usize> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            return "\t".into();
        }
        let n = line.len() - line.trim_start_matches(' ').len();
        // A JSDoc continuation (` * x`) is not an indentation level.
        if n > 0 && !line.trim_start().starts_with('*') {
            best = Some(best.map_or(n, |b| b.min(n)));
        }
    }
    " ".repeat(best.unwrap_or(4))
}

/// Start of the line holding `pos`.
pub fn line_start(text: &str, pos: u32) -> u32 {
    text[..pos as usize].rfind('\n').map_or(0, |i| i as u32 + 1)
}

/// Just after the line break ending the line of `pos` (the text's end on the last line).
pub fn next_line(text: &str, pos: u32) -> u32 {
    text[pos as usize..]
        .find('\n')
        .map_or(text.len() as u32, |i| pos + i as u32 + 1)
}

/// Leading whitespace of the line holding `pos`.
pub fn line_indent(text: &str, pos: u32) -> &str {
    let s = line_start(text, pos) as usize;
    let rest = &text[s..];
    let n = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    &rest[..n]
}

/// Top-level statements.
pub fn top_level(ast: &Ast) -> Vec<NodeId> {
    match *ast.node(ast.root) {
        Node::Program { stmts } => ast.list(stmts).to_vec(),
        _ => Vec::new(),
    }
}

/// Whether only whitespace lies in `text[from..to]`.
pub fn blank(text: &str, from: u32, to: u32) -> bool {
    text.get(from as usize..to as usize)
        .is_some_and(|s| s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_unit_is_the_smallest_indentation() {
        assert_eq!(indent_unit("a {\n  b {\n    c\n  }\n}\n"), "  ");
        assert_eq!(indent_unit("/**\n * doc\n */\nx\n"), "    ");
        assert_eq!(indent_unit("a {\n\tb\n}\n"), "\t");
    }

    #[test]
    fn lines() {
        let t = "ab\n  cd\nef";
        assert_eq!(line_start(t, 5), 3);
        assert_eq!(next_line(t, 4), 8);
        assert_eq!(next_line(t, 9), 10);
        assert_eq!(line_indent(t, 6), "  ");
    }
}
