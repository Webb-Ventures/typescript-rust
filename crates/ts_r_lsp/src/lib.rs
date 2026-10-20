//! ts-r-lsp: one shared, memory-budgeted language server daemon per workspace.
//!
//! L1 answers type questions with the project's own TypeScript (TS 7's native language
//! server, or tsserver for TS 5/6) behind the [`backend::Backend`] seam; our checker plugs
//! in there in L3. See `AGENTS.md` in this crate for the architecture, invariants and gaps.

pub mod backend;
pub mod client;
pub mod daemon;
pub mod docs;
pub mod fake;
pub mod front;
pub mod ipc;
pub mod jsonrpc;
pub mod lint;
pub mod process;
pub mod settings;
pub mod text;
pub mod translate;
pub mod tsr;
pub mod uri;
pub mod wire;
