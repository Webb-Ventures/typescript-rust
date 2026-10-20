//! Read-only queries for sound mode (`ts_r_sound`, design record 0007). Additive: nothing
//! here changes how the checker computes types or diagnostics; these only expose state the
//! regular check already computes (resolved call signatures) to code outside the crate.
//! Flow narrowing queries live in `flow/query_sound_flow.rs` (they need `flow.rs` internals).

use ts_r_types::SigId;

use crate::checker::{Ck, Loc};

impl Ck<'_, '_> {
    /// The signature resolved for a call, `new` or tagged template at `call` (the
    /// instantiated signature for generic calls). Checks the expression quietly when the
    /// regular walk has not resolved it yet. `None` when the callee has no signature
    /// (`any`, errors) or resolution failed.
    pub fn resolved_signature(&mut self, call: Loc) -> Option<SigId> {
        if let Some(&s) = self.resolved_calls.get(&call) {
            return s;
        }
        if self.is_resolving(crate::checker::Resolving::Node(call)) {
            return None;
        }
        self.quietly(|ck| ck.check_expr(call));
        self.resolved_calls.get(&call).copied().flatten()
    }
}
