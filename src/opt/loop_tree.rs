//! Loop nesting tree.
//!
//! Builds a parent/child hierarchy from the flat `Vec<NaturalLoop>`
//! returned by `find_natural_loops`. Each node knows its depth,
//! parent, and children, so passes like interchange can find
//! perfectly-nested pairs and unswitching can target innermost loops.

use std::collections::{HashMap, HashSet};
use crate::ir::inst::{BlockId, Function};
use crate::ir::walk::find_natural_loops;

/// Unique identifier for a loop in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoopId(pub u32);

/// A node in the loop nesting tree.
#[derive(Debug, Clone)]
pub struct LoopTreeNode {
    pub id: LoopId,
    pub header: BlockId,
    pub body: HashSet<BlockId>,
    pub latches: Vec<BlockId>,
    pub parent: Option<LoopId>,
    pub children: Vec<LoopId>,
    /// Nesting depth: 1 for outermost loops, 2 for their children, etc.
    pub depth: u32,
}

/// The complete loop nesting forest for a function.
#[derive(Debug)]
pub struct LoopTree {
    pub nodes: Vec<LoopTreeNode>,
    /// Maps each block to the innermost loop that contains it.
    pub block_to_loop: HashMap<BlockId, LoopId>,
}

impl LoopTree {
    /// Return the IDs of all innermost (leaf) loops — loops with no children.
    pub fn innermost_loops(&self) -> Vec<LoopId> {
        self.nodes.iter()
            .filter(|n| n.children.is_empty())
            .map(|n| n.id)
            .collect()
    }

    /// Get a node by ID.
    pub fn node(&self, id: LoopId) -> &LoopTreeNode {
        &self.nodes[id.0 as usize]
    }

    /// Nesting depth of a block (0 if not in any loop).
    pub fn loop_depth(&self, block: BlockId) -> u32 {
        self.block_to_loop.get(&block)
            .map(|lid| self.node(*lid).depth)
            .unwrap_or(0)
    }

    /// Return the parent loop of a given loop, if any.
    pub fn parent(&self, id: LoopId) -> Option<LoopId> {
        self.node(id).parent
    }

    /// Return (outer, inner) pairs of perfectly nested loops.
    /// "Perfectly nested" means outer's body blocks (excluding inner's
    /// body) contain no instructions other than control flow to/from
    /// the inner loop.
    pub fn perfectly_nested_pairs(&self, func: &Function) -> Vec<(LoopId, LoopId)> {
        let mut pairs = Vec::new();
        for node in &self.nodes {
            if node.children.len() != 1 { continue; }
            let child_id = node.children[0];
            let child = self.node(child_id);

            // The outer loop's "own" blocks (not in the inner loop)
            // must contain no non-trivial instructions. We allow:
            // - the outer header (relay block, 0 insts ok)
            // - the outer cmp block (comparison + condBr)
            // - the outer latch (increment + branch)
            // - the outer "body" block that just branches to the inner preheader
            // Everything else must be part of the inner loop.
            let outer_only: Vec<BlockId> = node.body.iter()
                .filter(|b| !child.body.contains(b))
                .copied()
                .collect();

            // Conservative check: outer-only blocks should have very few
            // instructions total (header relay + cmp + latch + body-entry).
            let total_outer_insts: usize = outer_only.iter()
                .map(|&b| func.block(b).insts.len())
                .sum();

            // A typical Fortran DO nest has ~4-6 instructions in the
            // outer shell (const bound, icmp, iadd). Allow up to 10
            // to account for LICM-hoisted invariants.
            if total_outer_insts <= 10 {
                pairs.push((node.id, child_id));
            }
        }
        pairs
    }
}

/// Build the loop nesting tree for a function.
///
/// Algorithm: discover natural loops, sort by body size descending,
/// then for each loop find the smallest enclosing loop (its parent).
pub fn build_loop_tree(func: &Function) -> LoopTree {
    let natural = find_natural_loops(func);

    if natural.is_empty() {
        return LoopTree {
            nodes: Vec::new(),
            block_to_loop: HashMap::new(),
        };
    }

    // Create nodes sorted by body size descending (largest = outermost first).
    let mut indexed: Vec<(usize, &crate::ir::walk::NaturalLoop)> =
        natural.iter().enumerate().collect();
    indexed.sort_by(|a, b| b.1.body.len().cmp(&a.1.body.len()));

    // Build nodes with stable IDs (original discovery order).
    let mut nodes: Vec<LoopTreeNode> = natural.iter().enumerate().map(|(i, nl)| {
        LoopTreeNode {
            id: LoopId(i as u32),
            header: nl.header,
            body: nl.body.clone(),
            latches: nl.latches.clone(),
            parent: None,
            children: Vec::new(),
            depth: 0,
        }
    }).collect();

    // For each loop, find its parent = the smallest loop that strictly
    // contains it. We iterate in body-size order so we can check
    // containment efficiently.
    let n = nodes.len();
    for i in 0..n {
        let mut best_parent: Option<LoopId> = None;
        let mut best_size = usize::MAX;
        for j in 0..n {
            if i == j { continue; }
            // j is a candidate parent if j's body strictly contains i's body.
            if nodes[j].body.len() > nodes[i].body.len()
                && nodes[i].body.is_subset(&nodes[j].body)
                && nodes[j].body.len() < best_size
            {
                best_parent = Some(nodes[j].id);
                best_size = nodes[j].body.len();
            }
        }
        nodes[i].parent = best_parent;
    }

    // Wire children from parent links.
    let parents: Vec<Option<LoopId>> = nodes.iter().map(|n| n.parent).collect();
    for (i, parent) in parents.into_iter().enumerate() {
        if let Some(pid) = parent {
            nodes[pid.0 as usize].children.push(LoopId(i as u32));
        }
    }

    // Sort children by header for determinism.
    // (Two-step to satisfy the borrow checker: collect sort keys, then sort.)
    for i in 0..n {
        let headers: Vec<(LoopId, u32)> = nodes[i].children.iter()
            .map(|c| (*c, nodes[c.0 as usize].header.0))
            .collect();
        nodes[i].children.sort_by_key(|c| {
            headers.iter().find(|(id, _)| id == c).map(|(_, h)| *h).unwrap_or(0)
        });
    }

    // Compute depths.
    fn set_depth(nodes: &mut [LoopTreeNode], id: LoopId, depth: u32) {
        nodes[id.0 as usize].depth = depth;
        let children: Vec<LoopId> = nodes[id.0 as usize].children.clone();
        for child in children {
            set_depth(nodes, child, depth + 1);
        }
    }
    for i in 0..n {
        if nodes[i].parent.is_none() {
            set_depth(&mut nodes, LoopId(i as u32), 1);
        }
    }

    // Build block → innermost loop mapping.
    // Process innermost (deepest) loops last so they overwrite parents.
    let mut block_to_loop: HashMap<BlockId, LoopId> = HashMap::new();
    let mut by_depth: Vec<(u32, LoopId)> = nodes.iter()
        .map(|n| (n.depth, n.id))
        .collect();
    by_depth.sort_by_key(|(d, _)| *d);
    for (_, lid) in by_depth {
        for &block in &nodes[lid.0 as usize].body {
            block_to_loop.insert(block, lid);
        }
    }

    LoopTree { nodes, block_to_loop }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::ir::inst::*;
    use crate::lexer::{Span, Position};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    /// Build a function with 3-level nested DO loops:
    ///   entry → outer_header → outer_cmp → inner_header → inner_cmp →
    ///           innermost_header → innermost_cmp → body → innermost_latch →
    ///           inner_latch → outer_latch → outer_header
    ///
    /// Each loop has: header (block param) → cmp (icmp + condBr) → ... → latch (iadd + br header)
    fn build_triple_nested() -> Function {
        let mut f = Function::new("triple".into(), vec![], IrType::Void);

        // Create all blocks upfront.
        let outer_hdr   = f.create_block("outer_hdr");
        let outer_cmp   = f.create_block("outer_cmp");
        let inner_hdr   = f.create_block("inner_hdr");
        let inner_cmp   = f.create_block("inner_cmp");
        let deep_hdr    = f.create_block("deep_hdr");
        let deep_cmp    = f.create_block("deep_cmp");
        let body        = f.create_block("body");
        let deep_latch  = f.create_block("deep_latch");
        let inner_latch = f.create_block("inner_latch");
        let outer_latch = f.create_block("outer_latch");
        let exit        = f.create_block("exit");

        let entry = f.entry;

        // Entry: const 1, br outer_hdr(1)
        let one = f.next_value_id();
        f.register_type(one, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: one, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(outer_hdr, vec![one]));

        // Helper: add a simple loop level (header with param → cmp → condBr)
        fn add_loop_level(
            f: &mut Function, hdr: BlockId, cmp: BlockId,
            body_target: BlockId, exit_target: BlockId,
            latch: BlockId, init_src: ValueId,
        ) -> (ValueId, ValueId) {
            // Header: block param
            let iv = f.next_value_id();
            f.register_type(iv, IrType::Int(IntWidth::I32));
            f.block_mut(hdr).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
            f.block_mut(hdr).terminator = Some(Terminator::Branch(cmp, vec![]));

            // Cmp: icmp le iv, 10; condBr body, exit
            let bound = f.next_value_id();
            f.register_type(bound, IrType::Int(IntWidth::I32));
            f.block_mut(cmp).insts.push(Inst {
                id: bound, ty: IrType::Int(IntWidth::I32), span: span(),
                kind: InstKind::ConstInt(10, IntWidth::I32),
            });
            let cmp_val = f.next_value_id();
            f.register_type(cmp_val, IrType::Bool);
            f.block_mut(cmp).insts.push(Inst {
                id: cmp_val, ty: IrType::Bool, span: span(),
                kind: InstKind::ICmp(CmpOp::Le, iv, bound),
            });
            f.block_mut(cmp).terminator = Some(Terminator::CondBranch {
                cond: cmp_val,
                true_dest: body_target, true_args: vec![],
                false_dest: exit_target, false_args: vec![],
            });

            // Latch: iadd iv, 1; br hdr(next)
            let one_l = f.next_value_id();
            f.register_type(one_l, IrType::Int(IntWidth::I32));
            f.block_mut(latch).insts.push(Inst {
                id: one_l, ty: IrType::Int(IntWidth::I32), span: span(),
                kind: InstKind::ConstInt(1, IntWidth::I32),
            });
            let next = f.next_value_id();
            f.register_type(next, IrType::Int(IntWidth::I32));
            f.block_mut(latch).insts.push(Inst {
                id: next, ty: IrType::Int(IntWidth::I32), span: span(),
                kind: InstKind::IAdd(iv, one_l),
            });
            f.block_mut(latch).terminator = Some(Terminator::Branch(hdr, vec![next]));

            let _ = init_src; // init_src is passed as the branch arg from the outer scope
            (iv, next)
        }

        // Outer loop: entry→outer_hdr(1)→outer_cmp→inner_hdr/exit
        let (_, _) = add_loop_level(&mut f, outer_hdr, outer_cmp, inner_hdr, exit, outer_latch, one);
        // Wire outer_cmp's true branch to pass `one` to inner_hdr
        // (inner loop starts at 1 each outer iteration)
        if let Some(Terminator::CondBranch { true_args, .. }) = &mut f.block_mut(outer_cmp).terminator {
            true_args.push(one);
        }

        // Inner loop
        let (_, _) = add_loop_level(&mut f, inner_hdr, inner_cmp, deep_hdr, inner_latch, inner_latch, one);
        // Inner latch needs to go to outer_latch after the inner loop exits...
        // Actually inner_latch IS the inner latch (iadd + br inner_hdr). The exit
        // of the inner loop goes to outer_latch. Let me fix: inner_cmp's false goes to outer_latch.
        // But add_loop_level set false_dest = inner_latch for the exit. Let me restructure.

        // This is getting complex. Let me simplify: just set terminators directly.
        // Clear what add_loop_level did and redo properly.

        // Actually let me just build it manually for clarity.
        // I'll clear all blocks and rebuild.
        for b in &mut f.blocks {
            b.insts.clear();
            b.params.clear();
            b.terminator = None;
        }

        // Re-emit entry
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(outer_hdr, vec![c1]));

        // Macro for loop headers
        macro_rules! loop_header {
            ($f:expr, $hdr:expr, $cmp:expr) => {{
                let iv = $f.next_value_id();
                $f.register_type(iv, IrType::Int(IntWidth::I32));
                $f.block_mut($hdr).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
                $f.block_mut($hdr).terminator = Some(Terminator::Branch($cmp, vec![]));
                iv
            }};
        }
        macro_rules! loop_cmp {
            ($f:expr, $cmp:expr, $iv:expr, $bound:expr, $t:expr, $t_args:expr, $fal:expr) => {{
                let cv = $f.next_value_id();
                $f.register_type(cv, IrType::Bool);
                $f.block_mut($cmp).insts.push(Inst {
                    id: cv, ty: IrType::Bool, span: span(),
                    kind: InstKind::ICmp(CmpOp::Le, $iv, $bound),
                });
                $f.block_mut($cmp).terminator = Some(Terminator::CondBranch {
                    cond: cv, true_dest: $t, true_args: $t_args,
                    false_dest: $fal, false_args: vec![],
                });
            }};
        }
        macro_rules! loop_latch {
            ($f:expr, $latch:expr, $iv:expr, $one:expr, $hdr:expr) => {{
                let nxt = $f.next_value_id();
                $f.register_type(nxt, IrType::Int(IntWidth::I32));
                $f.block_mut($latch).insts.push(Inst {
                    id: nxt, ty: IrType::Int(IntWidth::I32), span: span(),
                    kind: InstKind::IAdd($iv, $one),
                });
                $f.block_mut($latch).terminator = Some(Terminator::Branch($hdr, vec![nxt]));
            }};
        }

        // Outer: hdr(%oi) → cmp → {inner_hdr(1), exit}
        let oi = loop_header!(f, outer_hdr, outer_cmp);
        loop_cmp!(f, outer_cmp, oi, c10, inner_hdr, vec![c1], exit);

        // Inner: hdr(%ii) → cmp → {deep_hdr(1), outer_latch}
        let ii = loop_header!(f, inner_hdr, inner_cmp);
        loop_cmp!(f, inner_cmp, ii, c10, deep_hdr, vec![c1], outer_latch);

        // Deep: hdr(%di) → cmp → {body, inner_latch}
        let di = loop_header!(f, deep_hdr, deep_cmp);
        loop_cmp!(f, deep_cmp, di, c10, body, vec![], inner_latch);

        // Body: just branch to deep_latch
        f.block_mut(body).terminator = Some(Terminator::Branch(deep_latch, vec![]));

        // Latches
        loop_latch!(f, deep_latch, di, c1, deep_hdr);
        loop_latch!(f, inner_latch, ii, c1, inner_hdr);
        loop_latch!(f, outer_latch, oi, c1, outer_hdr);

        // Exit
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        f
    }

    #[test]
    fn triple_nested_tree_structure() {
        let f = build_triple_nested();
        let tree = build_loop_tree(&f);

        assert_eq!(tree.nodes.len(), 3, "should have 3 loops");

        // Find by depth
        let roots: Vec<_> = tree.nodes.iter().filter(|n| n.parent.is_none()).collect();
        assert_eq!(roots.len(), 1, "one outermost loop");
        assert_eq!(roots[0].depth, 1);
        assert_eq!(roots[0].children.len(), 1, "outer has one child");

        let mid = tree.node(roots[0].children[0]);
        assert_eq!(mid.depth, 2);
        assert_eq!(mid.children.len(), 1, "middle has one child");

        let inner = tree.node(mid.children[0]);
        assert_eq!(inner.depth, 3);
        assert!(inner.children.is_empty(), "innermost has no children");
    }

    #[test]
    fn innermost_returns_leaf_only() {
        let f = build_triple_nested();
        let tree = build_loop_tree(&f);

        let leaves = tree.innermost_loops();
        assert_eq!(leaves.len(), 1, "only the deepest loop is innermost");
        assert_eq!(tree.node(leaves[0]).depth, 3);
    }

    #[test]
    fn block_to_loop_maps_to_innermost() {
        let f = build_triple_nested();
        let tree = build_loop_tree(&f);

        // The body block should map to the innermost loop.
        let body_block = f.blocks.iter().find(|b| b.name.starts_with("body")).unwrap();
        let mapped = tree.block_to_loop.get(&body_block.id);
        assert!(mapped.is_some(), "body block should be in a loop");
        let mapped_node = tree.node(*mapped.unwrap());
        assert_eq!(mapped_node.depth, 3, "body should map to innermost loop");
    }

    #[test]
    fn empty_function_has_no_loops() {
        let mut f = Function::new("empty".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        let tree = build_loop_tree(&f);
        assert!(tree.nodes.is_empty());
        assert!(tree.innermost_loops().is_empty());
    }
}
