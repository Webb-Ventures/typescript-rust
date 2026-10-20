//! Control-flow graph used by narrowing and reachability.
//!
//! Every flow node points backwards to its antecedent(s). The checker walks from the flow node
//! recorded at a reference towards the function start, interpreting assignments and conditions.

use ts_r_ast::NodeId;

/// Index into [`FlowGraph::nodes`]. `FlowId::UNREACHABLE` (0) is the shared unreachable node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct FlowId(pub u32);

impl FlowId {
    pub const UNREACHABLE: FlowId = FlowId(0);

    #[inline]
    pub fn is_unreachable(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum FlowKind {
    Unreachable,
    /// Start of the file or of a function-like body. `node` = the function (none for the
    /// file); `a` = flow where a function expression/arrow was created (0 for declarations).
    Start,
    /// `node` = assignment target, declarator name, `for-in/of` head or `++`/`--` operand.
    Assignment,
    /// `node` = the condition expression; the branch where it was truthy.
    TrueCondition,
    /// `node` = the condition expression; the branch where it was falsy.
    FalseCondition,
    /// Join point. Antecedents: `lists[a..a+b]`.
    BranchLabel,
    /// Loop head (has back edges). Antecedents: `lists[a..a+b]`.
    LoopLabel,
    /// Entry to switch clauses `clause_start..clause_end` (packed in `b` as two `u16`);
    /// `clause_start == clause_end` means "no clause matched" (implicit default).
    /// `node` = the switch statement, `a` = antecedent.
    SwitchClause,
    /// A call statement that may be an assertion or `never`-returning call. `node` = the call.
    Call,
    /// `x.push(...)`, `x.unshift(...)` or `x[i] = v` on an evolving array. `node` = the call or
    /// assignment.
    ArrayMutation,
    /// Leaves a `finally` block towards one target. `a` = the flow at the end of the finally
    /// block; `lists[b]` = the finally entry label, `lists[b + 1]` = a count `n`, then `n`
    /// antecedents: while walking through `a`, the entry label only has those antecedents
    /// (see [`FlowGraph::reduce_target`]).
    ReduceLabel,
}

/// Flow node flags.
pub mod flow_flags {
    /// The condition comes from `??` (nullish, not truthiness).
    pub const NULLISH: u8 = 1 << 0;
    /// A label created for `finally` entry.
    pub const FINALLY: u8 = 1 << 1;
    /// Condition node created for an optional chain link.
    pub const OPTIONAL_CHAIN: u8 = 1 << 2;
}

/// 16 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowNode {
    pub kind: FlowKind,
    pub flags: u8,
    pub node: Option<NodeId>,
    pub a: u32,
    pub b: u32,
}

/// Flow information for one function-like body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FunctionFlow {
    pub function: NodeId,
    pub start: FlowId,
    /// Flow at the end of the body (unreachable when every path returns or throws).
    pub end: FlowId,
    /// Join of every `return` (unreachable when there is none).
    pub returns: FlowId,
}

/// The finished graph of one file.
#[derive(Clone, Debug, Default)]
pub struct FlowGraph {
    pub nodes: Vec<FlowNode>,
    /// Antecedent lists of labels and reduce labels.
    pub lists: Vec<FlowId>,
    /// Flow at references and statements, sorted by node id.
    pub node_flow: Vec<(NodeId, FlowId)>,
    /// Sorted by function node id.
    pub functions: Vec<FunctionFlow>,
    /// Each non-empty `case`/`default` clause but a switch's last whose end is reachable in
    /// the binder's graph, with the flow at its end (`noFallthroughCasesInSwitch`), in
    /// source order.
    pub fallthroughs: Vec<(NodeId, FlowId)>,
    /// Under `allowUnreachableCode: false` only: the flow before each statement the binder
    /// finds reachable that would be reported as unreachable code, sorted by node id (the
    /// checker reports those its call narrowing makes unreachable).
    pub stmt_flow: Vec<(NodeId, FlowId)>,
}

impl FlowGraph {
    pub fn node(&self, id: FlowId) -> &FlowNode {
        &self.nodes[id.index()]
    }

    /// Antecedents of a label (empty for other kinds).
    pub fn antecedents(&self, id: FlowId) -> &[FlowId] {
        let n = &self.nodes[id.index()];
        match n.kind {
            FlowKind::BranchLabel | FlowKind::LoopLabel => self
                .lists
                .get(n.a as usize..(n.a + n.b) as usize)
                .unwrap_or(&[]),
            _ => &[],
        }
    }

    /// For a `ReduceLabel`: the finally label it restricts and the antecedents it allows.
    pub fn reduce_target(&self, id: FlowId) -> Option<(FlowId, &[FlowId])> {
        let n = &self.nodes[id.index()];
        if n.kind != FlowKind::ReduceLabel {
            return None;
        }
        let start = n.b as usize;
        let target = self.lists.get(start).copied()?;
        let count = self.lists.get(start + 1).map_or(0, |c| c.0) as usize;
        let s = start + 2;
        Some((target, self.lists.get(s..s + count).unwrap_or(&[])))
    }

    /// The flow node recorded at `node`, if any.
    pub fn flow_at(&self, node: NodeId) -> Option<FlowId> {
        self.node_flow
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|i| self.node_flow[i].1)
    }

    /// The flow before statement `node` (`stmt_flow`), if recorded.
    pub fn stmt_flow_at(&self, node: NodeId) -> Option<FlowId> {
        self.stmt_flow
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|i| self.stmt_flow[i].1)
    }

    pub fn function_flow(&self, function: NodeId) -> Option<&FunctionFlow> {
        self.functions
            .binary_search_by_key(&function, |f| f.function)
            .ok()
            .map(|i| &self.functions[i])
    }
}
