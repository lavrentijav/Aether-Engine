//! # aether-redstone
//!
//! A **compiled, graph-based** redstone engine (spec §7/§8).
//!
//! Classic servers answer "what changed?" by re-querying the world block by
//! block ("what's to my left? below?") every time a signal moves. Aether instead
//! **compiles** a redstone network once into a **Directed Dependency Graph
//! (DDG)** laid out in contiguous [CSR] arrays, then ticks it by walking those
//! arrays — no spatial queries while the structure is unchanged.
//!
//! ```text
//!   build/break a component ──► [ compile graph → contiguous CSR arrays ]
//!                                            │
//!                                            ▼
//!   change a source level ──► [ solve(): bounded worklist relaxation ]
//!                                            │
//!                                            ▼
//!                              power(node) / is_lit(lamp)  — O(1) reads
//! ```
//!
//! ## Signal model (Phase 1)
//!
//! Every node carries a power level `0..=15`:
//!
//! * **Source** (lever, redstone block, torch): a fixed level you set.
//! * **Wire**: the strongest signal reaching it; a wire→wire hop loses one
//!   level, a source→wire hop does not — so a line from a level-15 source reads
//!   15, 14, 13, … and dies after 15 blocks.
//! * **Repeater**: a diode — any positive input re-boosts its output to 15,
//!   extending a line and (in Phase 2) adding a tick delay.
//! * **Lamp**: a sink; [`RedstoneGraph::is_lit`] is true when any signal reaches
//!   it.
//!
//! Edges are **directed** (`from` feeds `to`); model a two-way wire with
//! [`RedstoneGraphBuilder::link_both`].
//!
//! ## Registered Phase 1 deviations
//!
//! Quasi-Connectivity, exact repeater/comparator tick delays and strict
//! directional update order are **not** modelled here — they are the accepted
//! Phase 1 gaps closed in Phase 2 (see `docs/KNOWN_ISSUES.md`). The relaxation
//! computes the *steady-state* power, order-independent by construction.
//!
//! [CSR]: https://en.wikipedia.org/wiki/Sparse_matrix#Compressed_sparse_row_(CSR,_CRS_or_Yale_format)
//!
//! ```
//! use aether_redstone::{RedstoneGraphBuilder, NodeKind};
//!
//! let mut b = RedstoneGraphBuilder::new();
//! let src = b.add_source(15);
//! let w1 = b.add_wire();
//! let lamp = b.add_lamp();
//! b.link(src, w1);       // source feeds the wire
//! b.link(w1, lamp);      // wire feeds the lamp
//! let g = b.build();
//!
//! assert_eq!(g.power(w1), 15);
//! assert!(g.is_lit(lamp));
//! assert_eq!(g.kind(lamp), NodeKind::Lamp);
//! ```

use std::collections::VecDeque;

/// Maximum redstone signal strength.
pub const MAX_POWER: u8 = 15;

/// A node in the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A fixed power source (lever, redstone block, torch): holds a set level.
    Source,
    /// Redstone wire/dust: carries the strongest incoming signal, attenuating
    /// one level per wire hop.
    Wire,
    /// A repeater/diode: re-boosts any positive input to [`MAX_POWER`].
    Repeater,
    /// A powered device (lamp, piston, …): a sink that is "lit" when powered.
    Lamp,
}

/// A handle to a node in a [`RedstoneGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The raw index.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Builds a redstone network, then compiles it into a [`RedstoneGraph`].
#[derive(Debug, Clone, Default)]
pub struct RedstoneGraphBuilder {
    kinds: Vec<NodeKind>,
    levels: Vec<u8>,
    edges: Vec<(u32, u32)>,
}

impl RedstoneGraphBuilder {
    /// A new, empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, kind: NodeKind, level: u8) -> NodeId {
        let id = NodeId(self.kinds.len() as u32);
        self.kinds.push(kind);
        self.levels.push(level.min(MAX_POWER));
        id
    }

    /// Add a fixed power source holding `level` (`0..=15`).
    pub fn add_source(&mut self, level: u8) -> NodeId {
        self.push(NodeKind::Source, level)
    }

    /// Add a wire node.
    pub fn add_wire(&mut self) -> NodeId {
        self.push(NodeKind::Wire, 0)
    }

    /// Add a repeater (diode) node.
    pub fn add_repeater(&mut self) -> NodeId {
        self.push(NodeKind::Repeater, 0)
    }

    /// Add a lamp (sink) node.
    pub fn add_lamp(&mut self) -> NodeId {
        self.push(NodeKind::Lamp, 0)
    }

    /// Add a directed link: `from` feeds `to`.
    pub fn link(&mut self, from: NodeId, to: NodeId) {
        self.edges.push((from.0, to.0));
    }

    /// Add links in both directions — the usual way to wire adjacent two-way
    /// redstone dust.
    pub fn link_both(&mut self, a: NodeId, b: NodeId) {
        self.link(a, b);
        self.link(b, a);
    }

    /// Compile the network into a [`RedstoneGraph`] (contiguous CSR) and solve
    /// its initial steady state.
    pub fn build(self) -> RedstoneGraph {
        let n = self.kinds.len();
        // Build CSR successor lists: offsets[i]..offsets[i+1] index into targets.
        let mut counts = vec![0u32; n + 1];
        for &(from, _to) in &self.edges {
            counts[from as usize + 1] += 1;
        }
        for i in 0..n {
            counts[i + 1] += counts[i];
        }
        let offsets = counts;
        let mut targets = vec![0u32; self.edges.len()];
        let mut cursor = offsets.clone();
        for &(from, to) in &self.edges {
            let slot = &mut cursor[from as usize];
            targets[*slot as usize] = to;
            *slot += 1;
        }

        let mut g = RedstoneGraph {
            kinds: self.kinds,
            levels: self.levels,
            power: vec![0; n],
            offsets,
            targets,
        };
        g.solve();
        g
    }
}

/// A compiled redstone dependency graph in contiguous CSR memory.
#[derive(Debug, Clone)]
pub struct RedstoneGraph {
    kinds: Vec<NodeKind>,
    levels: Vec<u8>,
    power: Vec<u8>,
    // CSR successor adjacency: node `i`'s successors are
    // `targets[offsets[i]..offsets[i+1]]`.
    offsets: Vec<u32>,
    targets: Vec<u32>,
}

impl RedstoneGraph {
    /// Number of nodes.
    #[inline]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether the graph has no nodes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The kind of `id`.
    #[inline]
    pub fn kind(&self, id: NodeId) -> NodeKind {
        self.kinds[id.index()]
    }

    /// The current power level of `id` (`0..=15`).
    #[inline]
    pub fn power(&self, id: NodeId) -> u8 {
        self.power[id.index()]
    }

    /// Whether `id` carries any power.
    #[inline]
    pub fn is_powered(&self, id: NodeId) -> bool {
        self.power[id.index()] > 0
    }

    /// Whether a lamp is lit (powered). Non-lamp nodes are never "lit".
    #[inline]
    pub fn is_lit(&self, id: NodeId) -> bool {
        self.kinds[id.index()] == NodeKind::Lamp && self.power[id.index()] > 0
    }

    /// Set a source node's level and re-solve the graph. Returns `false` if the
    /// node is not a [`NodeKind::Source`].
    pub fn set_source(&mut self, id: NodeId, level: u8) -> bool {
        if self.kinds[id.index()] != NodeKind::Source {
            return false;
        }
        self.levels[id.index()] = level.min(MAX_POWER);
        self.solve();
        true
    }

    /// Signal a `sender` of the given power delivers to a `receiver`.
    ///
    /// The one-level wire attenuation applies only **wire → wire**: a wire feeds
    /// its *full* level to a repeater or lamp (they react to "is there a
    /// signal?"), and a source/repeater always delivers its full output.
    #[inline]
    fn delivered(sender: NodeKind, power: u8, receiver: NodeKind) -> u8 {
        match sender {
            NodeKind::Source | NodeKind::Repeater => power,
            NodeKind::Lamp => 0,
            NodeKind::Wire => {
                if receiver == NodeKind::Wire {
                    power.saturating_sub(1)
                } else {
                    power
                }
            }
        }
    }

    /// Power a receiver of `kind` takes from an incoming signal `cand`.
    #[inline]
    fn receive(kind: NodeKind, cand: u8) -> u8 {
        match kind {
            // A repeater is a diode: any positive input re-boosts to full.
            NodeKind::Repeater => {
                if cand > 0 {
                    MAX_POWER
                } else {
                    0
                }
            }
            // Wires and lamps take the signal as-is; sources are fixed inputs.
            NodeKind::Wire | NodeKind::Lamp => cand,
            NodeKind::Source => 0,
        }
    }

    /// Recompute every node's steady-state power from the sources.
    ///
    /// A bounded, monotone (max-only) worklist relaxation: each node is re-queued
    /// only when its power strictly increases, and power is capped at
    /// [`MAX_POWER`], so it always terminates — order-independent by design.
    pub fn solve(&mut self) {
        let n = self.len();
        for p in &mut self.power {
            *p = 0;
        }
        let mut queue: VecDeque<u32> = VecDeque::with_capacity(n);
        for i in 0..n {
            if self.kinds[i] == NodeKind::Source && self.levels[i] > 0 {
                self.power[i] = self.levels[i];
                queue.push_back(i as u32);
            }
        }

        while let Some(u) = queue.pop_front() {
            let ui = u as usize;
            let uk = self.kinds[ui];
            let up = self.power[ui];
            let (start, end) = (self.offsets[ui] as usize, self.offsets[ui + 1] as usize);
            for &v in &self.targets[start..end] {
                let vi = v as usize;
                let vk = self.kinds[vi];
                if vk == NodeKind::Source {
                    continue; // sources are fixed inputs
                }
                let cand = Self::delivered(uk, up, vk);
                if cand == 0 {
                    continue;
                }
                let newp = Self::receive(vk, cand);
                if newp > self.power[vi] {
                    self.power[vi] = newp;
                    queue.push_back(v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_wire_chain_attenuates() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(15);
        let wires: Vec<_> = (0..4).map(|_| b.add_wire()).collect();
        b.link(src, wires[0]);
        for w in wires.windows(2) {
            b.link(w[0], w[1]);
        }
        let g = b.build();
        // Adjacent to a level-15 source the wire is 15, then -1 per hop.
        assert_eq!(g.power(wires[0]), 15);
        assert_eq!(g.power(wires[1]), 14);
        assert_eq!(g.power(wires[2]), 13);
        assert_eq!(g.power(wires[3]), 12);
    }

    #[test]
    fn wire_signal_dies_after_15_blocks() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(15);
        let wires: Vec<_> = (0..17).map(|_| b.add_wire()).collect();
        b.link(src, wires[0]);
        for w in wires.windows(2) {
            b.link(w[0], w[1]);
        }
        let g = b.build();
        assert_eq!(g.power(wires[14]), 1); // 15,14,...,1
        assert_eq!(g.power(wires[15]), 0); // died out
        assert_eq!(g.power(wires[16]), 0);
    }

    #[test]
    fn lamp_lights_within_range_and_not_beyond() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(15);
        let near = b.add_wire();
        let near_lamp = b.add_lamp();
        b.link(src, near);
        b.link(near, near_lamp);
        let g = b.build();
        assert!(g.is_lit(near_lamp));
        assert!(!g.is_lit(near)); // a wire is not a lamp
    }

    #[test]
    fn repeater_reboosts_and_extends_the_line() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(15);
        // 15 wires bring the signal down to 1...
        let mut prev = src;
        let mut last_wire = src;
        for _ in 0..15 {
            let w = b.add_wire();
            b.link(prev, w);
            prev = w;
            last_wire = w;
        }
        // ...a repeater re-boosts to 15...
        let rep = b.add_repeater();
        b.link(last_wire, rep);
        // ...and a fresh wire line continues at full strength.
        let after = b.add_wire();
        b.link(rep, after);
        let lamp = b.add_lamp();
        b.link(after, lamp);
        let g = b.build();
        assert_eq!(g.power(last_wire), 1);
        assert_eq!(g.power(rep), 15);
        assert_eq!(g.power(after), 15);
        assert!(g.is_lit(lamp));
    }

    #[test]
    fn unpowered_source_leaves_network_dark() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(0);
        let w = b.add_wire();
        let lamp = b.add_lamp();
        b.link(src, w);
        b.link(w, lamp);
        let g = b.build();
        assert_eq!(g.power(w), 0);
        assert!(!g.is_lit(lamp));
    }

    #[test]
    fn toggling_a_source_resolves() {
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(0);
        let w = b.add_wire();
        let lamp = b.add_lamp();
        b.link(src, w);
        b.link(w, lamp);
        let mut g = b.build();
        assert!(!g.is_lit(lamp));
        // Flip the lever on.
        assert!(g.set_source(src, 15));
        assert!(g.is_lit(lamp));
        assert_eq!(g.power(w), 15);
        // And back off.
        g.set_source(src, 0);
        assert!(!g.is_lit(lamp));
        // Non-source nodes reject set_source.
        assert!(!g.set_source(w, 15));
    }

    #[test]
    fn strongest_of_two_sources_wins_at_a_junction() {
        let mut b = RedstoneGraphBuilder::new();
        let weak = b.add_source(5);
        let strong = b.add_source(15);
        let junction = b.add_wire();
        b.link(weak, junction);
        b.link(strong, junction);
        let g = b.build();
        assert_eq!(g.power(junction), 15);
    }

    #[test]
    fn wire_loop_is_stable() {
        // A ring of wires fed by one source must converge, not spin forever.
        let mut b = RedstoneGraphBuilder::new();
        let src = b.add_source(15);
        let ring: Vec<_> = (0..6).map(|_| b.add_wire()).collect();
        b.link(src, ring[0]);
        for i in 0..ring.len() {
            b.link_both(ring[i], ring[(i + 1) % ring.len()]);
        }
        let g = b.build();
        // The node opposite the feed still gets a sensible, bounded value.
        assert_eq!(g.power(ring[0]), 15);
        assert!(g.power(ring[3]) > 0 && g.power(ring[3]) <= 15);
    }

    #[test]
    fn bidirectional_wire_carries_both_ways() {
        let mut b = RedstoneGraphBuilder::new();
        let mid = b.add_wire();
        let left = b.add_source(15);
        let rlamp = b.add_lamp();
        let a = b.add_wire();
        b.link(left, mid);
        b.link_both(mid, a);
        b.link(a, rlamp);
        let g = b.build();
        assert!(g.is_lit(rlamp));
    }

    #[test]
    fn empty_graph_is_harmless() {
        let g = RedstoneGraphBuilder::new().build();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
    }
}
