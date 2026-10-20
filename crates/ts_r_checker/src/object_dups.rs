//! Duplicate object literal members that the binder cannot see (TS1117/TS1118/TS1119).
//!
//! The binder reports duplicates between literal keys (`a`, `"a"`, `1`, `["a"]`) and
//! forgets earlier names at each spread. tsc 7.0.2 also compares computed keys by the name
//! their type denotes (a string, number or enum literal, or a unique symbol: `[s]` with
//! `const s = "s"`, `[-1]`, `[E.A]`, `[sym]`, `[f()]` returning `typeof sym`), and keeps
//! names across spreads (`{ a: 1, ...o, a: 2 }` is TS1117). The kinds follow the binder's
//! table: property after property is TS1117, two getters or two setters TS1118, a
//! property next to a method or accessor TS1119; methods may repeat. The error is at the
//! later key. Pairs the binder already reported are skipped here.

use rustc_hash::FxHashMap;
use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::diagnostics_generated::DiagnosticInfo;
use crate::nodes::key_text;

const P: u8 = 1;
const ME: u8 = 2;
const G: u8 = 4;
const S: u8 = 8;

/// Earlier members of one name: kinds of all of them, and of the literal ones since the
/// last spread (those the binder compares).
#[derive(Default, Clone, Copy)]
struct Seen {
    all: u8,
    binder: u8,
    generation: u32,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks object literal `n` for duplicates involving computed keys or spreads.
    pub(crate) fn check_object_literal_duplicates(&mut self, file: FileId, n: NodeId) {
        let ast = self.ast(file);
        let Node::ObjectLit { props } = *ast.node(n) else {
            return;
        };
        let props = ast.list(props);
        // Only literals whose members the binder already compared in full are skipped.
        let needed = props.iter().any(|&p| match *ast.node(p) {
            Node::Spread { .. } => true,
            Node::Property { key, .. } | Node::MethodDef { key, .. } => {
                matches!(ast.node(key), Node::ComputedKey { .. }) && key_text(ast, key).is_none()
            }
            _ => false,
        });
        if !needed {
            return;
        }
        let mut seen: FxHashMap<String, Seen> = FxHashMap::default();
        let mut generation = 0;
        for &p in props {
            let (key, kind) = match *ast.node(p) {
                Node::Property { key, .. } => (key, P),
                Node::ShorthandProp { name, .. } => (name, P),
                Node::MethodDef { kind, key, .. } => (
                    key,
                    match kind {
                        MethodKind::Get => G,
                        MethodKind::Set => S,
                        _ => ME,
                    },
                ),
                Node::Spread { .. } => {
                    generation += 1;
                    continue;
                }
                _ => continue,
            };
            let (name, literal) = match key_text(ast, key) {
                Some(t) => (t, true),
                None => match *ast.node(key) {
                    Node::ComputedKey { expr } => {
                        match self.late_bound_name(Loc::new(file, expr)) {
                            Some(t) => (t, false),
                            None => continue,
                        }
                    }
                    _ => continue,
                },
            };
            let prev = seen.get(&name).copied().unwrap_or_default();
            let err = conflict(kind, prev.all);
            let binder_err = if literal && prev.generation == generation {
                conflict(kind, prev.binder)
            } else {
                None
            };
            if let Some(info) = err
                && binder_err.is_none()
            {
                self.error(Loc::new(file, key), info, &[]);
            }
            let e = seen.entry(name).or_default();
            e.all |= kind;
            if literal {
                if e.generation != generation {
                    e.binder = 0;
                    e.generation = generation;
                }
                e.binder |= kind;
            }
        }
    }
}

/// The error for a member of `kind` after earlier members of kinds `prev`.
fn conflict(kind: u8, prev: u8) -> Option<DiagnosticInfo> {
    match kind {
        P if prev & P != 0 => {
            Some(dg::AN_OBJECT_LITERAL_CANNOT_HAVE_MULTIPLE_PROPERTIES_WITH_THE_SAME_NAME_1117)
        }
        P if prev & (ME | G | S) != 0 => {
            Some(dg::AN_OBJECT_LITERAL_CANNOT_HAVE_PROPERTY_AND_ACCESSOR_WITH_THE_SAME_NAME_1119)
        }
        ME if prev & (P | G | S) != 0 => {
            Some(dg::AN_OBJECT_LITERAL_CANNOT_HAVE_PROPERTY_AND_ACCESSOR_WITH_THE_SAME_NAME_1119)
        }
        G | S if prev & (P | ME) != 0 => {
            Some(dg::AN_OBJECT_LITERAL_CANNOT_HAVE_PROPERTY_AND_ACCESSOR_WITH_THE_SAME_NAME_1119)
        }
        G if prev & G != 0 => Some(
            dg::AN_OBJECT_LITERAL_CANNOT_HAVE_MULTIPLE_GET_SET_ACCESSORS_WITH_THE_SAME_NAME_1118,
        ),
        S if prev & S != 0 => Some(
            dg::AN_OBJECT_LITERAL_CANNOT_HAVE_MULTIPLE_GET_SET_ACCESSORS_WITH_THE_SAME_NAME_1118,
        ),
        _ => None,
    }
}
