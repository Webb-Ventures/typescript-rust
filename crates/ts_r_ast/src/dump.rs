//! Plain-text tree dump used by snapshot tests.
//!
//! Format: one node per line, `Kind start..end field=value ...`, children indented by two
//! spaces and prefixed with their field name (`name:` or `name[i]:` for lists). Default-valued
//! scalars (false, empty modifiers, first enum variant) are omitted.

use std::fmt::Write as _;

use crate::{Ast, FieldDump, NodeId};

pub(crate) fn dump(ast: &Ast, root: NodeId, src: Option<&str>) -> String {
    let mut out = String::new();
    // Explicit stack: (node, indent, label).
    let mut stack: Vec<(NodeId, usize, String)> = vec![(root, 0, String::new())];
    while let Some((id, indent, label)) = stack.pop() {
        let node = ast.node(id);
        let span = ast.span(id);
        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(&label);
        let _ = write!(out, "{} {}..{}", node.kind().name(), span.start, span.end);
        let fields = node.dump_fields(ast);
        let mut children: Vec<(NodeId, String)> = Vec::new();
        for (name, f) in fields {
            match f {
                FieldDump::Skip => {}
                FieldDump::Scalar(s) if s.is_empty() => {
                    let _ = write!(out, " {name}");
                }
                FieldDump::Scalar(s) => {
                    let _ = write!(out, " {name}={s}");
                }
                FieldDump::Child(c) => children.push((c, format!("{name}: "))),
                FieldDump::List(l) => {
                    for (i, &c) in ast.list(l).iter().enumerate() {
                        children.push((c, format!("{name}[{i}]: ")));
                    }
                }
            }
        }
        if children.is_empty()
            && let Some(src) = src
        {
            let text = span.text(src);
            if !text.is_empty() && text.len() <= 60 && !text.contains('\n') {
                let _ = write!(out, " `{text}`");
            }
        }
        out.push('\n');
        for (c, l) in children.into_iter().rev() {
            stack.push((c, indent + 2, l));
        }
    }
    out
}
