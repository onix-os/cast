// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use petgraph::{
    Direction,
    algo::astar,
    prelude::DiGraph,
    visit::{Dfs, Topo, Walker},
};

use self::subgraph::subgraph;

mod subgraph;

/// NodeIndex as employed in moss-rs usage
pub type NodeIndex = petgraph::prelude::NodeIndex<u32>;

/// The concrete node path which would become cyclic after adding an edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle<N> {
    pub path: Vec<N>,
}

/// Simplistic encapsulation of petgraph APIs to provide
/// suitable mechanisms to empower transaction code
#[derive(Debug, Clone)]
pub struct Dag<N>(DiGraph<N, (), u32>);

impl<N> Default for Dag<N> {
    fn default() -> Self {
        Self(DiGraph::default())
    }
}

impl<N> AsRef<DiGraph<N, (), u32>> for Dag<N> {
    fn as_ref(&self) -> &DiGraph<N, (), u32> {
        &self.0
    }
}

impl<N> Dag<N>
where
    N: Clone + PartialEq,
{
    /// Construct a new Dag
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds node N to the graph and returns the index.
    ///
    /// If N already exists, it'll return the index of that node.
    pub fn add_node_or_get_index(&mut self, node: &N) -> NodeIndex {
        if let Some(index) = self.get_index(node) {
            index
        } else {
            self.0.add_node(node.clone())
        }
    }

    /// Returns true if the node exists
    pub fn node_exists(&self, node: &N) -> bool {
        self.get_index(node).is_some()
    }

    /// Remove node
    pub fn remove_node(&mut self, node: &N) -> Option<N> {
        if let Some(index) = self.get_index(node) {
            self.0.remove_node(index)
        } else {
            None
        }
    }

    /// Add an edge from a to b
    pub fn add_edge(&mut self, a: NodeIndex, b: NodeIndex) -> bool {
        self.try_add_edge(a, b).unwrap_or(false)
    }

    /// Add an edge from `a` to `b`, retaining a concrete cycle path when the
    /// edge would make the graph cyclic.
    ///
    /// `Ok(false)` means the edge already exists. This keeps duplicate edges
    /// distinct from cycles for callers which need actionable diagnostics.
    pub fn try_add_edge(&mut self, a: NodeIndex, b: NodeIndex) -> Result<bool, Cycle<N>> {
        if self.0.contains_edge(a, b) {
            return Ok(false);
        }

        // Adding a -> b creates a cycle exactly when b can already reach a.
        if let Some((_, path)) = astar(&self.0, b, |node| node == a, |_| 1_usize, |_| 0) {
            let mut cycle = Vec::with_capacity(path.len() + 1);
            cycle.push(self.0[a].clone());
            cycle.extend(path.into_iter().map(|node| self.0[node].clone()));
            return Err(Cycle { path: cycle });
        }

        self.0.add_edge(a, b, ());
        Ok(true)
    }

    pub fn iter_nodes(&self) -> impl Iterator<Item = &'_ N> {
        self.0.node_weights()
    }

    /// Return the direct outgoing dependencies of `node`.
    pub fn successors<'a>(&'a self, node: &N) -> impl Iterator<Item = &'a N> + 'a {
        self.get_index(node)
            .into_iter()
            .flat_map(|index| self.0.neighbors_directed(index, Direction::Outgoing))
            .map(|index| &self.0[index])
    }

    /// Perform a depth-first search, given the start index
    pub fn dfs(&self, start: NodeIndex) -> impl Iterator<Item = &'_ N> {
        let dfs = Dfs::new(&self.0, start);

        dfs.iter(&self.0).map(|i| &self.0[i])
    }

    /// Perform a topological sort
    pub fn topo(&self) -> impl Iterator<Item = &'_ N> {
        let topo = Topo::new(&self.0);

        topo.iter(&self.0).map(|i| &self.0[i])
    }

    /// Returns batches of nodes that can be executed in parallel.
    pub fn batched_topo(&self) -> Vec<Vec<N>>
    where
        N: Ord,
    {
        let mut g = self.0.clone();
        let mut batches = Vec::new();

        while g.node_count() > 0 {
            let mut sources: Vec<_> = g.externals(Direction::Incoming).collect();
            if sources.is_empty() && g.node_count() > 0 {
                // Cycle detected.
                break;
            }

            let batch_nodes: Vec<_> = sources.iter().map(|&i| g[i].clone()).collect();
            batches.push(batch_nodes);

            // Reverse index before removing nodes to avoid graph invalidation (dupes in batches)
            sources.sort_by_key(|&idx| std::cmp::Reverse(idx.index()));

            for ix in sources {
                g.remove_node(ix);
            }
        }
        batches
    }

    /// Transpose the graph, returning the clone
    pub fn transpose(&self) -> Self {
        let mut transposed = self.0.clone();
        transposed.reverse();
        Self(transposed)
    }

    /// Split the graph at the given start node(s) - returning a new graph
    pub fn subgraph(&self, starting_nodes: &[N]) -> Self {
        Self(subgraph(&self.0, starting_nodes))
    }

    /// Return the index for node of type N
    pub fn get_index(&self, node: &N) -> Option<NodeIndex> {
        self.0.node_indices().find(|i| self.0[*i] == *node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batched_linear_dag() {
        let mut graph: Dag<i32> = Dag::new();

        // A -> B -> C -> D
        let a = graph.add_node_or_get_index(&1);
        let b = graph.add_node_or_get_index(&2);
        let c = graph.add_node_or_get_index(&3);
        let d = graph.add_node_or_get_index(&4);

        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, d);

        let batches = graph.batched_topo();

        // Each node is in its own batch (sequential)
        assert_eq!(batches.len(), 4);
        for batch in &batches {
            assert_eq!(batch.len(), 1);
        }
    }

    #[test]
    fn test_topo_batched_simple_dag() {
        let mut graph: Dag<usize> = Dag::new();

        // Create a simple DAG:
        //   A -> C -> E
        //   B -> D -> E
        let a = graph.add_node_or_get_index(&1);
        let b = graph.add_node_or_get_index(&2);
        let c = graph.add_node_or_get_index(&3);
        let d = graph.add_node_or_get_index(&4);
        let e = graph.add_node_or_get_index(&5);

        graph.add_edge(a, c);
        graph.add_edge(b, d);
        graph.add_edge(c, e);
        graph.add_edge(d, e);

        let batches = graph.batched_topo();

        assert_eq!(batches.len(), 3);

        // TODO: How tf do i get node value from A to E?

        // Batch 0: A and B (no dependencies)
        assert_eq!(batches[0].len(), 2);
        assert!(batches[0].contains(&1));
        assert!(batches[0].contains(&2));

        // Batch 1: C and D
        assert_eq!(batches[1].len(), 2);
        assert!(batches[1].contains(&3));
        assert!(batches[1].contains(&4));

        // Batch 2: E
        assert_eq!(batches[2].len(), 1);
        assert!(batches[2].contains(&5));
    }

    #[test]
    fn test_topo_batched_fully_parallel() {
        let mut graph: Dag<char> = Dag::new();

        // Four independent nodes
        let _a = graph.add_node_or_get_index(&'A');
        let _b = graph.add_node_or_get_index(&'B');
        let _c = graph.add_node_or_get_index(&'C');
        let _d = graph.add_node_or_get_index(&'D');

        let batches = graph.batched_topo();

        // All nodes in one batch (fully parallel)
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 4);
    }

    #[test]
    fn test_topo_batched_empty_graph() {
        let graph: Dag<i32> = Dag::new();
        let batches = graph.batched_topo();
        assert_eq!(batches.len(), 0);
    }

    #[test]
    fn successors_returns_only_direct_outgoing_nodes() {
        let mut graph = Dag::new();
        let root = graph.add_node_or_get_index(&"root");
        let direct = graph.add_node_or_get_index(&"direct");
        let transitive = graph.add_node_or_get_index(&"transitive");
        graph.add_edge(root, direct);
        graph.add_edge(direct, transitive);

        assert_eq!(graph.successors(&"root").copied().collect::<Vec<_>>(), ["direct"]);
    }

    #[test]
    fn try_add_edge_distinguishes_duplicates_from_actionable_cycles() {
        let mut graph = Dag::new();
        let a = graph.add_node_or_get_index(&"a");
        let b = graph.add_node_or_get_index(&"b");
        let c = graph.add_node_or_get_index(&"c");

        assert_eq!(graph.try_add_edge(a, b), Ok(true));
        assert_eq!(graph.try_add_edge(a, b), Ok(false));
        assert_eq!(graph.try_add_edge(b, c), Ok(true));
        assert_eq!(
            graph.try_add_edge(c, a),
            Err(Cycle {
                path: vec!["c", "a", "b", "c"]
            })
        );
        assert!(!graph.add_edge(c, a));
    }
}
