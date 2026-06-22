//! # AeroRegex — Aho-Corasick + Graph Matching Engine
//!
//! The top-level matching engine that combines an Aho-Corasick DFA for fast
//! multi-pattern literal scanning with node-graph constraint validation.
//!
//! ## How it works
//!
//! 1. **Build phase** (`AeroRegex::new`):
//!    - Parse each regex → normalize → compile to `RegexGraph`s
//!    - Extract all literal words from graphs
//!    - Build an Aho-Corasick DFA from those words
//!    - Map DFA pattern IDs back to graph/node pairs via `word_map`
//!
//! 2. **Match phase** (`feed_packet`):
//!    - Stream input bytes through the Aho-Corasick DFA
//!    - On each DFA match, look up which graphs/nodes are triggered
//!    - Validate positional constraints via `is_match`
//!
//! ## Key types
//!
//! - `State` — streaming match state (Aho state + position + current nodes)
//! - `MatchNode` — a node in a graph with its matched position
//! - `PosIndex` — position constraint (exact, at-least, or range)

use crate::regex_graph::{Node, NodeData};
use crate::{
    normalizer::normalize,
    regex_expr::RegexExpr,
    regex_graph::RegexGraph,
    utilities::{RegexId, RegexIdList},
};
use aho_corasick::{
    Anchored,
    automaton::{Automaton, StateID},
    dfa::DFA,
};
use bstr::BString;
use itertools::Itertools;
use std::collections::HashMap;
use std::iter::Enumerate;
use std::slice::Iter;

#[derive(Debug)]
enum PosIndex {
    Index(usize),
    AtLeast(usize),
    Range(usize, usize),
}

impl PosIndex {
    pub(crate) fn matched(&self, pos: usize) -> bool {
        match self {
            PosIndex::Index(index) => *index == pos,
            PosIndex::AtLeast(index) => pos >= *index,
            PosIndex::Range(min, max) => pos >= *min && pos <= *max,
        }
    }
}

#[derive(Debug)]
struct MatchNode {
    node_id: usize,
    pos: PosIndex,
}

#[derive(Debug)]
pub(crate) struct State {
    current_nodes: Vec<Option<MatchNode>>,
    aho_state: StateID,
    pos: usize,
}

#[derive(Clone)]
struct WordIndex {
    graph_id: usize,
    node_id: usize,
}

pub(crate) struct AeroRegex {
    aho: DFA,
    graphs: Vec<RegexGraph>,
    word_map: Vec<(Vec<WordIndex>, usize)>,
}

impl AeroRegex {
    pub(crate) fn new(regexes: &[(String, RegexId)]) -> Self {
        let mut graphs = vec![];
        let mut word_map: HashMap<BString, Vec<WordIndex>> = HashMap::new();

        for (regex, regex_index) in regexes {
            let regex = RegexExpr::parse(&regex);

            let generated_graphs = RegexGraph::new(&normalize(&regex), *regex_index);
            for graph in generated_graphs {
                let graph_id = graphs.len();
                for (word, node_id) in graph.words() {
                    word_map
                        .entry(word.clone())
                        .or_default()
                        .push(WordIndex { graph_id, node_id });
                }
                graphs.push(graph);
            }
        }

        let aho_patterns = word_map.keys().cloned().collect_vec();

        let word_map = word_map
            .iter()
            .map(|(word, graph_indexes)| ((*graph_indexes).clone(), word.len()))
            .collect();

        Self {
            aho: DFA::builder().build(&aho_patterns).unwrap(),
            graphs,
            word_map,
        }
    }

    pub(crate) fn feed_packet(&self, state: &mut State, bytes: &[u8]) -> RegexIdList {
        let mut results = RegexIdList::default();

        for &byte in bytes {
            state.aho_state = self.aho.next_state(Anchored::No, state.aho_state, byte);
            state.pos += 1;
            let pos = state.pos;

            if !self.aho.is_match(state.aho_state) {
                continue;
            }

            for pattern_id in 0..self.aho.match_len(state.aho_state) {
                let pattern = self.aho.match_pattern(state.aho_state, pattern_id);
                let (records, word_len) = &self.word_map[pattern.as_usize()];

                for &WordIndex { graph_id, node_id } in records {
                    let last_node = &state.current_nodes[graph_id];

                    let regex_graph = &self.graphs[graph_id];

                    // let current_node = regex_graph.nodes(node_id);
                    let a = self.is_match(last_node, graph_id, node_id, pos);

                    // let last_node_id = last_node.node_id
                    // prev_end + word_len == pos
                }
            }
        }

        results
    }

    fn is_match(
        &self,
        last_node: &Option<MatchNode>,
        graph_id: usize,
        current_node_id: usize,
        current_pos: usize,
    ) -> bool {
        match last_node {
            None => {
                let regex_graph = &self.graphs[graph_id];
                let node_index = 0;
                let node = &regex_graph.nodes[node_index];
                match node.data {
                    NodeData::Start => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::Index(0),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos)
                    }
                    NodeData::Literal { .. } => current_node_id == node_index,
                    NodeData::Temp { len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::Index(len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos)
                    }
                    NodeData::TempRang { min_len, max_len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::Range(min_len, max_len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos)
                    }
                    NodeData::TempInf { len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::AtLeast(len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos)
                    }
                    NodeData::Empty => unreachable!(),
                    NodeData::End => unreachable!(),
                    NodeData::Repetition { .. } => todo!(),
                    NodeData::OrLiteral { .. } => todo!(),
                    NodeData::OrGraph { .. } => todo!(),
                }
            }
            Some(MatchNode { node_id, pos }) => {
                let regex_graph = &self.graphs[graph_id];
                let node = &regex_graph.nodes[node_id + 1];

                if current_node_id <= *node_id {
                    return false;
                }

                match node.data {
                    NodeData::Start => unreachable!(),
                    NodeData::End => unreachable!(),
                    NodeData::Literal { .. } => {
                        if current_node_id == *node_id {
                            todo!()
                            // node.len +
                        }
                    }
                    NodeData::OrLiteral { .. } => todo!(),
                    NodeData::OrGraph { .. } => todo!(),
                    NodeData::Temp { .. } => todo!(),
                    NodeData::TempRang { .. } => todo!(),
                    NodeData::TempInf { .. } => todo!(),
                    NodeData::Empty => todo!(),
                    NodeData::Repetition { .. } => todo!(),
                }
                false
            }
        }
    }
}
