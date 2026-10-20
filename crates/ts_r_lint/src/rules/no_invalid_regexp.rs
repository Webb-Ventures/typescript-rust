//! `no-invalid-regexp`: disallow invalid regular expression strings in `RegExp` constructors.

use super::prelude::*;
use crate::regex::{self, Flags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-invalid-regexp",
        "Disallow invalid regular expression strings in `RegExp` constructors",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoInvalidRegexp>,
};

#[derive(Debug, Default)]
struct NoInvalidRegexp {
    /// Extra flags allowed by `allowConstructorFlags`.
    allowed: String,
}

impl FromOptions for NoInvalidRegexp {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoInvalidRegexp {
            allowed: str_list(obj_opt(opts, 0), "allowConstructorFlags").concat(),
        })
    }
}

pub(crate) const STANDARD_FLAGS: &str = "dgimsuvy";

fn string_lit(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

impl NoInvalidRegexp {
    fn flag_error(&self, flags: &str) -> Option<String> {
        let invalid: String = flags
            .chars()
            .filter(|c| !STANDARD_FLAGS.contains(*c) && !self.allowed.contains(*c))
            .collect();
        if !invalid.is_empty() {
            return Some(format!(
                "Invalid flags supplied to RegExp constructor '{invalid}'."
            ));
        }
        for (i, c) in flags.char_indices() {
            if flags[..i].contains(c) {
                return Some(format!(
                    "Duplicate flags ('{c}') supplied to RegExp constructor."
                ));
            }
        }
        if flags.contains('u') && flags.contains('v') {
            return Some("Regex 'u' and 'v' flags cannot be used together.".to_owned());
        }
        None
    }
}

impl Rule for NoInvalidRegexp {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(args) = regexp_call_args(cx, node, false, false) else {
            return;
        };
        let ast = cx.ast();
        let flags = match args.get(1) {
            None => Some(""),
            Some(&a) => string_lit(ast, a),
        };
        if let Some(f) = flags
            && let Some(msg) = self.flag_error(f)
        {
            cx.report_node(node, msg).id("regexMessage");
            return;
        }
        let Some(pattern) = args.first().and_then(|&a| string_lit(ast, a)) else {
            return;
        };
        let error = match flags {
            Some(f) => {
                let std: String = f.chars().filter(|c| STANDARD_FLAGS.contains(*c)).collect();
                regex::parse(pattern, Flags::new(&std))
                    .err()
                    .map(|e| regex::error_message(pattern, &std, &e))
            }
            None => {
                // Unknown flags: valid if some flag set accepts the pattern.
                let first = regex::parse(pattern, Flags::default()).err();
                match first {
                    Some(e)
                        if regex::parse(pattern, Flags::new("u")).is_err()
                            && regex::parse(pattern, Flags::new("v")).is_err() =>
                    {
                        Some(regex::error_message(pattern, "", &e))
                    }
                    _ => None,
                }
            }
        };
        if let Some(msg) = error {
            cx.report_node(node, format!("{msg}.")).id("regexMessage");
        }
    }
}
