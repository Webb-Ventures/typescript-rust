//! Open-document multiplexing: many clients' documents -> one backend's view (K-12).
//!
//! Every client keeps its own copy of each open document. A backend ("slot") has a
//! [`Mirror`]: for each document it has open, the text it was given and the client that
//! text belongs to (the *owner*). Invariant after every event: a mirror doc's text equals
//! its owner's text.
//!
//! When several clients of one backend have a file open with different text, the daemon
//! picks the text to show the backend with [`Mirror::decide`]:
//!
//! * all equal -> nothing to do;
//! * otherwise compare each text with the file on disk; texts that differ are *dirty*
//!   (unsaved edits). One distinct dirty text -> the backend gets it (the clean windows
//!   just show the saved file, so the backend sees the edits). Two or more different dirty
//!   texts -> [`Diverged`]: two windows edit the same unsaved file differently, and the
//!   daemon falls back to one backend per client for that workspace.
//!
//! Output is a list of [`SyncOp`]s the daemon turns into tsserver or LSP messages.

use crate::text::{Pos, TextDoc};
use std::collections::BTreeMap;

/// Client identifier (unique per daemon).
pub type ClientId = u64;

/// One client's copy of an open document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientDoc {
    /// Current text.
    pub text: TextDoc,
    /// LSP language id (`typescript`, ...) or derived from tsserver's `scriptKindName`.
    pub language_id: String,
    /// The URI this client uses (LSP clients), so answers echo it exactly.
    pub uri: Option<String>,
    /// tsserver `scriptKindName`, if the client gave one.
    pub script_kind: Option<String>,
    /// tsserver `projectRootPath`, if the client gave one.
    pub project_root: Option<String>,
}

impl ClientDoc {
    /// A new document.
    pub fn new(text: impl Into<String>, language_id: impl Into<String>) -> ClientDoc {
        ClientDoc {
            text: TextDoc::new(text),
            language_id: language_id.into(),
            uri: None,
            script_kind: None,
            project_root: None,
        }
    }
}

/// A document as the backend has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendDoc {
    /// Text the backend has.
    pub text: TextDoc,
    /// Client whose text this is.
    pub owner: ClientId,
    /// LSP version last sent to the backend.
    pub version: i64,
    /// URI used with the backend (LSP backends).
    pub uri: String,
    /// Language id used at open.
    pub language_id: String,
}

/// A change to send to the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOp {
    /// Open a document.
    Open {
        /// Document key (path, or URI for non-file schemes).
        key: String,
        /// URI for LSP backends.
        uri: String,
        /// Language id.
        language_id: String,
        /// Full text.
        text: String,
        /// LSP version.
        version: i64,
        /// tsserver `scriptKindName`.
        script_kind: Option<String>,
        /// tsserver `projectRootPath`.
        project_root: Option<String>,
    },
    /// Apply edits in order (each range refers to the text after the previous edit).
    Edits {
        /// Document key.
        key: String,
        /// URI for LSP backends.
        uri: String,
        /// LSP version after the edits.
        version: i64,
        /// `(range, new text)`, applied in order.
        edits: Vec<((Pos, Pos), String)>,
    },
    /// Close a document.
    Close {
        /// Document key.
        key: String,
        /// URI for LSP backends.
        uri: String,
    },
}

/// Two clients edit the same unsaved file differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diverged {
    /// The file.
    pub key: String,
    /// The clients with distinct unsaved texts.
    pub clients: Vec<ClientId>,
}

/// Read-only view of the clients served by one backend.
pub trait Members {
    /// The member clients that have `key` open, with their docs, in client-id order.
    fn docs_for(&self, key: &str) -> Vec<(ClientId, &ClientDoc)>;
    /// Every key any member has open.
    fn all_keys(&self) -> Vec<String>;
}

impl Members for BTreeMap<ClientId, BTreeMap<String, ClientDoc>> {
    fn docs_for(&self, key: &str) -> Vec<(ClientId, &ClientDoc)> {
        self.iter()
            .filter_map(|(c, docs)| docs.get(key).map(|d| (*c, d)))
            .collect()
    }
    fn all_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.values().flat_map(|d| d.keys().cloned()).collect();
        keys.sort();
        keys.dedup();
        keys
    }
}

/// What a backend has open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mirror {
    /// Open documents by key.
    pub docs: BTreeMap<String, BackendDoc>,
}

fn uri_for(key: &str, doc: &ClientDoc) -> String {
    doc.uri.clone().unwrap_or_else(|| {
        if key.contains(':') && !key.starts_with('/') && !looks_like_drive(key) {
            key.to_string()
        } else {
            crate::uri::path_to_uri(key)
        }
    })
}

fn looks_like_drive(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

impl Mirror {
    /// Forgets everything (backend restarted).
    pub fn clear(&mut self) {
        self.docs.clear();
    }

    /// Fast path: `client` applied `edits` to `key` and it owns the backend's copy, so the
    /// edits are forwarded as they are. Returns `None` when the slow path is needed.
    /// `new_text` is the client's text after the edits.
    pub fn forward_edits(
        &mut self,
        client: ClientId,
        key: &str,
        edits: &[(Option<(Pos, Pos)>, String)],
        new_text: &TextDoc,
    ) -> Option<SyncOp> {
        let doc = self.docs.get_mut(key)?;
        if doc.owner != client {
            return None;
        }
        let before_end = doc.text.end_pos();
        let mut explicit = Vec::with_capacity(edits.len());
        for (range, text) in edits {
            let r = range.unwrap_or((Pos::default(), doc.text.end_pos()));
            doc.text.apply_change(Some(r), text);
            explicit.push((r, text.clone()));
        }
        doc.version += 1;
        if doc.text.text() != new_text.text() {
            // Our edit semantics disagreed with the client's; resync with a full replace.
            doc.text = new_text.clone();
            return Some(SyncOp::Edits {
                key: key.to_string(),
                uri: doc.uri.clone(),
                version: doc.version,
                edits: vec![((Pos::default(), before_end), new_text.text().to_string())],
            });
        }
        Some(SyncOp::Edits {
            key: key.to_string(),
            uri: doc.uri.clone(),
            version: doc.version,
            edits: explicit,
        })
    }

    /// Brings the backend's copy of `key` in line with the members' docs.
    /// `prefer` is the client whose event triggered this (wins ties).
    pub fn decide(
        &mut self,
        key: &str,
        members: &dyn Members,
        prefer: Option<ClientId>,
        disk: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<SyncOp>, Diverged> {
        let docs = members.docs_for(key);
        if docs.is_empty() {
            return Ok(match self.docs.remove(key) {
                Some(d) => vec![SyncOp::Close {
                    key: key.to_string(),
                    uri: d.uri,
                }],
                None => vec![],
            });
        }
        let first_text = docs[0].1.text.text();
        let owner: ClientId = if docs.iter().all(|(_, d)| d.text.text() == first_text) {
            match self.docs.get(key) {
                Some(b) if docs.iter().any(|(c, _)| *c == b.owner) => b.owner,
                _ => prefer
                    .filter(|p| docs.iter().any(|(c, _)| c == p))
                    .unwrap_or(docs[0].0),
            }
        } else {
            let disk_text = disk(key);
            let dirty: Vec<(ClientId, &ClientDoc)> = docs
                .iter()
                .filter(|(_, d)| disk_text.as_deref() != Some(d.text.text()))
                .copied()
                .collect();
            let mut distinct: Vec<&str> = dirty.iter().map(|(_, d)| d.text.text()).collect();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() > 1 {
                return Err(Diverged {
                    key: key.to_string(),
                    clients: dirty.iter().map(|(c, _)| *c).collect(),
                });
            }
            let pool = if dirty.is_empty() { &docs } else { &dirty };
            let current = self.docs.get(key).map(|b| b.owner);
            [current, prefer]
                .into_iter()
                .flatten()
                .find(|c| pool.iter().any(|(p, _)| p == c))
                .unwrap_or(pool[0].0)
        };
        let (_, odoc) = docs
            .iter()
            .find(|(c, _)| *c == owner)
            .expect("owner has the doc");
        Ok(match self.docs.get_mut(key) {
            None => {
                let uri = uri_for(key, odoc);
                self.docs.insert(
                    key.to_string(),
                    BackendDoc {
                        text: odoc.text.clone(),
                        owner,
                        version: 1,
                        uri: uri.clone(),
                        language_id: odoc.language_id.clone(),
                    },
                );
                vec![SyncOp::Open {
                    key: key.to_string(),
                    uri,
                    language_id: odoc.language_id.clone(),
                    text: odoc.text.text().to_string(),
                    version: 1,
                    script_kind: odoc.script_kind.clone(),
                    project_root: odoc.project_root.clone(),
                }]
            }
            Some(b) => {
                b.owner = owner;
                if b.text.text() == odoc.text.text() {
                    vec![]
                } else {
                    let old_end = b.text.end_pos();
                    b.text = odoc.text.clone();
                    b.version += 1;
                    vec![SyncOp::Edits {
                        key: key.to_string(),
                        uri: b.uri.clone(),
                        version: b.version,
                        edits: vec![((Pos::default(), old_end), odoc.text.text().to_string())],
                    }]
                }
            }
        })
    }

    /// Reconciles every document (after a restart, a split or a member leaving).
    pub fn reconcile(
        &mut self,
        members: &dyn Members,
        disk: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<SyncOp>, Diverged> {
        let mut ops = Vec::new();
        let mut keys = members.all_keys();
        keys.extend(self.docs.keys().cloned());
        keys.sort();
        keys.dedup();
        for key in keys {
            ops.extend(self.decide(&key, members, None, disk)?);
        }
        Ok(ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type World = BTreeMap<ClientId, BTreeMap<String, ClientDoc>>;

    fn open(w: &mut World, c: ClientId, key: &str, text: &str) {
        w.entry(c)
            .or_default()
            .insert(key.into(), ClientDoc::new(text, "typescript"));
    }

    fn set(w: &mut World, c: ClientId, key: &str, text: &str) {
        w.get_mut(&c).unwrap().get_mut(key).unwrap().text = TextDoc::new(text);
    }

    fn disk_says(text: &'static str) -> impl Fn(&str) -> Option<String> {
        move |_| Some(text.to_string())
    }

    #[test]
    fn second_window_with_same_text_shares_the_backend_copy() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "x");
        let ops = m.decide("/a.ts", &w, Some(1), &disk_says("x")).unwrap();
        assert!(matches!(&ops[..], [SyncOp::Open { text, .. }] if text == "x"));
        open(&mut w, 2, "/a.ts", "x");
        assert!(
            m.decide("/a.ts", &w, Some(2), &disk_says("x"))
                .unwrap()
                .is_empty()
        );
        // Owner closes: the other window keeps the doc open, nothing to send.
        w.get_mut(&1).unwrap().remove("/a.ts");
        assert!(
            m.decide("/a.ts", &w, Some(1), &disk_says("x"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(m.docs["/a.ts"].owner, 2);
        w.get_mut(&2).unwrap().remove("/a.ts");
        assert!(matches!(
            &m.decide("/a.ts", &w, Some(2), &disk_says("x")).unwrap()[..],
            [SyncOp::Close { .. }]
        ));
        assert!(m.docs.is_empty());
    }

    #[test]
    fn one_dirty_window_wins_over_clean_ones() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "saved");
        open(&mut w, 2, "/a.ts", "saved");
        m.reconcile(&w, &disk_says("saved")).unwrap();
        // Window 2 types: the backend follows window 2.
        set(&mut w, 2, "/a.ts", "saved+edit");
        let ops = m.decide("/a.ts", &w, Some(2), &disk_says("saved")).unwrap();
        assert!(matches!(&ops[..], [SyncOp::Edits { edits, .. }] if edits[0].1 == "saved+edit"));
        assert_eq!(m.docs["/a.ts"].owner, 2);
        // Now the owner's edits go straight through.
        set(&mut w, 2, "/a.ts", "saved+edit!");
        let op = m
            .forward_edits(
                2,
                "/a.ts",
                &[(Some((Pos::new(0, 10), Pos::new(0, 10))), "!".into())],
                &TextDoc::new("saved+edit!"),
            )
            .unwrap();
        assert!(matches!(op, SyncOp::Edits { version: 3, .. }));
        // Window 1 is not the owner: slow path.
        assert!(
            m.forward_edits(1, "/a.ts", &[], &TextDoc::new("saved"))
                .is_none()
        );
    }

    #[test]
    fn two_different_unsaved_edits_diverge() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "saved");
        open(&mut w, 2, "/a.ts", "saved");
        m.reconcile(&w, &disk_says("saved")).unwrap();
        set(&mut w, 1, "/a.ts", "saved-1");
        m.decide("/a.ts", &w, Some(1), &disk_says("saved")).unwrap();
        set(&mut w, 2, "/a.ts", "saved-2");
        let err = m
            .decide("/a.ts", &w, Some(2), &disk_says("saved"))
            .unwrap_err();
        assert_eq!(
            err,
            Diverged {
                key: "/a.ts".into(),
                clients: vec![1, 2]
            }
        );
        // Same unsaved text in both windows is not a divergence.
        set(&mut w, 2, "/a.ts", "saved-1");
        assert!(m.decide("/a.ts", &w, Some(2), &disk_says("saved")).is_ok());
    }

    #[test]
    fn unreadable_disk_counts_as_dirty() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "one");
        open(&mut w, 2, "/a.ts", "two");
        assert!(m.reconcile(&w, &|_| None).is_err());
    }

    #[test]
    fn reconcile_after_restart_and_member_leaving() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "a");
        open(&mut w, 2, "/b.ts", "b");
        let ops = m.reconcile(&w, &disk_says("")).unwrap();
        assert_eq!(
            ops.iter()
                .filter(|o| matches!(o, SyncOp::Open { .. }))
                .count(),
            2
        );
        w.remove(&2);
        let ops = m.reconcile(&w, &disk_says("")).unwrap();
        assert!(matches!(&ops[..], [SyncOp::Close { key, .. }] if key == "/b.ts"));
        m.clear();
        let ops = m.reconcile(&w, &disk_says("")).unwrap();
        assert!(
            matches!(&ops[..], [SyncOp::Open { key, uri, .. }] if key == "/a.ts" && uri == "file:///a.ts")
        );
    }

    #[test]
    fn forward_edits_resyncs_on_semantic_mismatch() {
        let mut w = World::new();
        let mut m = Mirror::default();
        open(&mut w, 1, "/a.ts", "abc");
        m.reconcile(&w, &disk_says("abc")).unwrap();
        let op = m
            .forward_edits(
                1,
                "/a.ts",
                &[(Some((Pos::new(0, 0), Pos::new(0, 1))), "X".into())],
                &TextDoc::new("something else"),
            )
            .unwrap();
        assert!(
            matches!(op, SyncOp::Edits { edits, .. } if edits[0].0 == (Pos::new(0, 0), Pos::new(0, 3)))
        );
        assert_eq!(m.docs["/a.ts"].text.text(), "something else");
    }
}
