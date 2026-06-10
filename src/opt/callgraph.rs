//! Call graph construction and analysis.
//!
//! Builds a call graph from a module's functions after call resolution.
//! Detects recursive functions via DFS cycle detection. Provides
//! reverse post-order iteration for bottom-up inlining (callees first).

use crate::ir::inst::*;
use crate::ir::walk::find_natural_loops;
use std::collections::HashSet;

/// A node in the call graph — one per function in the module.
#[derive(Debug)]
pub struct CallNode {
    /// Index into Module::functions.
    pub func_idx: u32,
    /// Indices of functions this function calls (via Internal refs).
    pub callees: Vec<u32>,
    /// Indices of functions that call this one.
    pub callers: Vec<u32>,
    /// Total instruction count (cost model).
    pub inst_count: usize,
    /// True if this function is (directly or transitively) recursive.
    pub is_recursive: bool,
}

/// The call graph for an entire module.
#[derive(Debug)]
pub struct CallGraph {
    pub nodes: Vec<CallNode>,
}

impl CallGraph {
    /// Build the call graph from a module.
    pub fn build(module: &Module) -> Self {
        let n = module.functions.len();
        let mut nodes: Vec<CallNode> = (0..n)
            .map(|i| CallNode {
                func_idx: i as u32,
                callees: Vec::new(),
                callers: Vec::new(),
                inst_count: 0,
                is_recursive: false,
            })
            .collect();

        // Scan each function for Call(Internal) instructions.
        for (i, func) in module.functions.iter().enumerate() {
            let mut callees_set: HashSet<u32> = HashSet::new();
            let mut count = 0usize;
            for block in &func.blocks {
                count += block.insts.len();
                for inst in &block.insts {
                    if let InstKind::Call(FuncRef::Internal(idx), _) = &inst.kind {
                        callees_set.insert(*idx);
                    }
                }
            }
            let mut callees: Vec<u32> = callees_set.into_iter().collect();
            callees.sort();
            nodes[i].callees = callees.clone();

            // Apply loop cost multiplier: functions with loops have higher
            // dynamic cost than their static instruction count suggests.
            // Multiply by 4 for each loop found (conservative estimate of
            // average trip count impact on code expansion after inlining).
            let loops = find_natural_loops(func);
            let loop_multiplier = if loops.is_empty() { 1 } else { 4 * loops.len() };
            nodes[i].inst_count = count * loop_multiplier;

            // Register callers.
            for &callee in &callees {
                if (callee as usize) < n {
                    nodes[callee as usize].callers.push(i as u32);
                }
            }
        }

        // Detect recursion via DFS from each node.
        for i in 0..n {
            if detect_cycle(&nodes, i as u32) {
                nodes[i].is_recursive = true;
            }
        }

        CallGraph { nodes }
    }

    /// Return function indices in bottom-up order: callees before
    /// callers. This is the correct order for inlining.
    pub fn bottom_up_order(&self) -> Vec<u32> {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        let mut order = Vec::new();

        // Start DFS from root nodes (functions with no callers) to
        // ensure post-order puts leaves (callees) first.
        let roots: Vec<u32> = (0..n as u32)
            .filter(|&i| self.nodes[i as usize].callers.is_empty())
            .collect();
        for root in &roots {
            rpo_dfs(&self.nodes, *root, &mut visited, &mut order);
        }
        // Also visit any unvisited nodes (cycles, disconnected).
        for i in 0..n {
            if !visited[i] {
                rpo_dfs(&self.nodes, i as u32, &mut visited, &mut order);
            }
        }

        // Post-order puts callees AFTER callers. We want callees first,
        // so DON'T reverse — just return the post-order directly.
        order
    }

    /// Get the cost of inlining a function (instruction count).
    pub fn inline_cost(&self, func_idx: u32) -> usize {
        self.nodes[func_idx as usize].inst_count
    }

    /// Is the function recursive?
    pub fn is_recursive(&self, func_idx: u32) -> bool {
        self.nodes[func_idx as usize].is_recursive
    }
}

fn rpo_dfs(nodes: &[CallNode], idx: u32, visited: &mut [bool], order: &mut Vec<u32>) {
    if visited[idx as usize] {
        return;
    }
    visited[idx as usize] = true;
    for &callee in &nodes[idx as usize].callees {
        if (callee as usize) < nodes.len() {
            rpo_dfs(nodes, callee, visited, order);
        }
    }
    order.push(idx);
}

/// Detect if function `start` is part of a cycle in the call graph.
fn detect_cycle(nodes: &[CallNode], start: u32) -> bool {
    let mut visiting = HashSet::new();
    dfs_cycle(nodes, start, &mut visiting)
}

fn dfs_cycle(nodes: &[CallNode], idx: u32, visiting: &mut HashSet<u32>) -> bool {
    if !visiting.insert(idx) {
        return true; // back-edge → cycle
    }
    for &callee in &nodes[idx as usize].callees {
        if (callee as usize) < nodes.len() && dfs_cycle(nodes, callee, visiting) {
            return true;
        }
    }
    visiting.remove(&idx);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    #[test]
    fn detects_self_recursion() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("factorial".into(), vec![], IrType::Int(IntWidth::I32));
        // Self-call: factorial calls factorial.
        let call_id = f.next_value_id();
        f.register_type(call_id, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: call_id,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::Call(FuncRef::Internal(0), vec![]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(Some(call_id)));
        m.add_function(f);

        let cg = CallGraph::build(&m);
        assert!(
            cg.is_recursive(0),
            "self-calling function should be recursive"
        );
    }

    #[test]
    fn non_recursive_detected() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        // Function 0: "callee" — no calls.
        let mut callee = Function::new("callee".into(), vec![], IrType::Int(IntWidth::I32));
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(None));
        m.add_function(callee);

        // Function 1: "caller" — calls callee (Internal(0)).
        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        let call_id = caller.next_value_id();
        caller.register_type(call_id, IrType::Int(IntWidth::I32));
        caller.block_mut(caller.entry).insts.push(Inst {
            id: call_id,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::Call(FuncRef::Internal(0), vec![]),
        });
        caller.block_mut(caller.entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        let cg = CallGraph::build(&m);
        assert!(!cg.is_recursive(0), "callee should not be recursive");
        assert!(!cg.is_recursive(1), "caller should not be recursive");
        assert_eq!(cg.nodes[1].callees, vec![0]);
        assert_eq!(cg.nodes[0].callers, vec![1]);
    }

    #[test]
    fn bottom_up_order_callees_first() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        // callee (idx 0) — no calls
        let mut callee = Function::new("callee".into(), vec![], IrType::Void);
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(None));
        m.add_function(callee);
        // caller (idx 1) — calls callee
        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        let cid = caller.next_value_id();
        caller.register_type(cid, IrType::Void);
        caller.block_mut(caller.entry).insts.push(Inst {
            id: cid,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Call(FuncRef::Internal(0), vec![]),
        });
        caller.block_mut(caller.entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        let cg = CallGraph::build(&m);
        let rpo = cg.bottom_up_order();
        // Callee should come before caller in RPO.
        let callee_pos = rpo.iter().position(|&i| i == 0).unwrap();
        let caller_pos = rpo.iter().position(|&i| i == 1).unwrap();
        assert!(
            callee_pos < caller_pos,
            "callee should come before caller in RPO"
        );
    }
}
