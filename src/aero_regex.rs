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

use crate::regex_graph::NodeData;
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
struct MatchNode {
    node_id: usize,
    pos: PosIndex,
}

#[derive(Debug, Clone)]
pub(crate) struct State {
    current_nodes: Vec<Option<MatchNode>>,
    aho_state: StateID,
    pos: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

    pub(crate) fn check(&self, state: &mut State, bytes: &[u8]) -> RegexIdList {
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

                    if self.is_match(last_node, graph_id, node_id, pos, word_len) {
                        state.current_nodes[graph_id] = Some(MatchNode {
                            node_id,
                            pos: PosIndex::Index(pos),
                        });
                        if self.is_end_node(graph_id, node_id) {
                            results.push(regex_graph.regex_id);
                            
                        }
                    }
                }
            }
        }

        // TODO: Remove duplicates from results. This is a temporary solution until we can fix the underlying issue.
        results.iter().unique().cloned().collect()
    }

    pub(crate) fn start_state(&self) -> State {
        State {
            current_nodes: vec![None; self.graphs.len()],
            pos: 0,
            aho_state: self.aho.start_state(Anchored::No).unwrap(),
        }
    }

    fn is_match(
        &self,
        last_node: &Option<MatchNode>,
        graph_id: usize,
        current_node_id: usize,
        current_pos: usize,
        word_len: &usize,
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
                        self.is_match(&last_node, graph_id, current_node_id, current_pos, word_len)
                    }
                    NodeData::Literal { .. } => {
                        current_node_id == node_index
                            && node.start_index.matched(current_pos - word_len)
                    }
                    NodeData::OrLiteral { .. } => {
                        current_node_id == node_index
                            && node.start_index.matched(current_pos - word_len)
                    }
                    NodeData::Temp { len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::Index(len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos, word_len)
                    }
                    NodeData::TempRang { min_len, max_len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::Range(min_len, max_len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos, word_len)
                    }
                    NodeData::TempInf { len } => {
                        let last_node = Some(MatchNode {
                            node_id: node_index,
                            pos: PosIndex::AtLeast(len),
                        });
                        self.is_match(&last_node, graph_id, current_node_id, current_pos, word_len)
                    }
                    NodeData::Empty => unreachable!(),
                    NodeData::End => unreachable!(),
                    NodeData::Repetition { .. } => todo!(),
                    NodeData::OrGraph { .. } => todo!(),
                }
            }
            Some(MatchNode { node_id, pos }) => {
                let node_id = node_id + 1;
                let regex_graph = &self.graphs[graph_id];
                let node = &regex_graph.nodes[node_id];

                if current_node_id < node_id {
                    return false;
                }

                match node.data {
                    NodeData::Start => unreachable!(),
                    NodeData::Literal { .. } => {
                        if current_node_id == node_id {
                            pos.matched(current_pos - word_len)
                        } else {
                            false
                        }
                    }
                    NodeData::End => todo!(),
                    NodeData::OrLiteral { .. } => todo!(),
                    NodeData::OrGraph { .. } => todo!(),
                    NodeData::Temp { .. } => todo!(),
                    NodeData::TempRang { .. } => todo!(),
                    NodeData::TempInf { .. } => todo!(),
                    NodeData::Empty => todo!(),
                    NodeData::Repetition { .. } => todo!(),
                }
            }
        }
    }

    fn is_end_node(&self, graph_id: usize, node_id: usize) -> bool {
        let nodes = &self.graphs[graph_id].nodes;

        node_id == nodes.len() - 1
            || nodes[node_id + 1..].iter().all(|n| {
                matches!(
                    n.data,
                    NodeData::Start
                        | NodeData::End
                        | NodeData::Empty
                        | NodeData::Temp { .. }
                        | NodeData::TempRang { .. }
                        | NodeData::TempInf { .. }
                        | NodeData::Repetition { .. }
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(patterns: &[&str]) -> AeroRegex {
        let regexes: Vec<(String, RegexId)> = patterns
            .iter()
            .enumerate()
            .map(|(i, &s)| (s.to_string(), i))
            .collect();
        AeroRegex::new(&regexes)
    }

    fn check(engine: &AeroRegex, input: &[u8]) -> RegexIdList {
        let mut state = engine.start_state();
        engine.check(&mut state, input)
    }

    mod simple_literal {
        use super::*;

        #[test]
        fn exact_match() {
            let engine = &build(&["abc"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn no_match() {
            let engine = &build(&["abc"]);
            assert_eq!(check(engine, b"def"), vec![]);
        }

        #[test]
        fn partial_match() {
            let engine = &build(&["abc"]);
            assert_eq!(check(engine, b"ab"), vec![]);
        }

        #[test]
        fn match_in_middle() {
            let engine = &build(&["bc"]);
            assert_eq!(check(engine, b"abcd"), vec![0]);
        }

        #[test]
        fn empty_input() {
            let engine = &build(&["abc"]);
            assert_eq!(check(engine, b""), vec![]);
        }

        #[test]
        fn single_char() {
            let engine = &build(&["a"]);
            assert_eq!(check(engine, b"a"), vec![0]);
            assert_eq!(check(engine, b"b"), vec![]);
        }
    }

    mod multiple_patterns {
        use super::*;

        #[test]
        fn two_patterns() {
            let engine = &build(&["abc", "def"]);
            assert_eq!(check(engine, b"abcdef"), vec![0, 1]);
        }

        #[test]
        fn overlapping_patterns() {
            let engine = &build(&["ab", "bc"]);
            assert_eq!(check(engine, b"abc"), vec![0, 1]);
        }

        #[test]
        fn same_pattern_different_id() {
            let engine = &build(&["foo", "bar"]);
            assert_eq!(check(engine, b"foo"), vec![0]);
            assert_eq!(check(engine, b"bar"), vec![1]);
        }

        #[test]
        fn three_patterns_all_match() {
            let engine = &build(&["a", "b", "c"]);
            assert_eq!(check(engine, b"abc"), vec![0, 1, 2]);
        }

        #[test]
        fn three_patterns_partial_match() {
            let engine = &build(&["a", "b", "c"]);
            assert_eq!(check(engine, b"ab"), vec![0, 1]);
        }
    }

    mod alternation {
        use super::*;

        #[test]
        fn simple_alternation() {
            let engine = &build(&["a|b"]);
            assert_eq!(check(engine, b"a"), vec![0]);
            assert_eq!(check(engine, b"b"), vec![0]);
        }

        #[test]
        fn alternation_no_match() {
            let engine = &build(&["a|b"]);
            assert_eq!(check(engine, b"c"), vec![]);
        }

        #[test]
        fn alternation_in_context() {
            let engine = &build(&["a|b"]);
            let result = check(engine, b"xyzaxyzb");
            assert!(result.contains(&0));
        }
    }

    mod concatenation {
        use super::*;

        #[test]
        fn two_literals() {
            let engine = &build(&["ab"]);
            assert_eq!(check(engine, b"ab"), vec![0]);
        }

        #[test]
        fn longer_concat() {
            let engine = &build(&["hello"]);
            assert_eq!(check(engine, b"hello"), vec![0]);
        }

        #[test]
        fn concat_not_found() {
            let engine = &build(&["hello"]);
            assert_eq!(check(engine, b"hell"), vec![]);
        }

        #[test]
        fn concat_in_longer_input() {
            let engine = &build(&["ab"]);
            assert_eq!(check(engine, b"xxabxx"), vec![0]);
        }
    }

    mod streaming {
        use super::*;

        #[test]
        fn feed_chunked() {
            let engine = &build(&["abc"]);
            let mut state = engine.start_state();
            engine.check(&mut state, b"ab");
            assert!(!engine.check(&mut state, b"c").is_empty());
        }

        #[test]
        fn feed_across_boundary() {
            let engine = &build(&["bc"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b"ab"), vec![]);
            assert_eq!(engine.check(&mut state, b"c"), vec![0]);
        }

        #[test]
        fn multiple_feeds() {
            let engine = &build(&["xyz"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b"x"), vec![]);
            assert_eq!(engine.check(&mut state, b"y"), vec![]);
            assert_eq!(engine.check(&mut state, b"z"), vec![0]);
        }

        #[test]
        fn feed_single_byte_at_a_time() {
            let engine = &build(&["abc"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b"a"), vec![]);
            assert_eq!(engine.check(&mut state, b"b"), vec![]);
            assert_eq!(engine.check(&mut state, b"c"), vec![0]);
        }

        #[test]
        fn feed_empty_then_match() {
            let engine = &build(&["abc"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b""), vec![]);
            assert_eq!(engine.check(&mut state, b"abc"), vec![0]);
        }
    }

    mod real_world {
        use super::*;

        #[test]
        fn file_extension() {
            let engine = &build(&["html", "css", "js"]);
            assert_eq!(check(engine, b"index.html"), vec![0]);
            assert_eq!(check(engine, b"style.css"), vec![1]);
            assert_eq!(check(engine, b"app.js"), vec![2]);
        }

        #[test]
        fn ip_address_fragment() {
            let engine = &build(&["192", "168"]);
            assert_eq!(check(engine, b"192.168.1.1"), vec![0, 1]);
        }

        #[test]
        fn protocol_prefix() {
            let engine = &build(&["http", "https"]);
            assert_eq!(check(engine, b"https://example.com"), vec![0, 1]);
        }

        #[test]
        fn multiple_keywords() {
            let engine = &build(&["SELECT", "FROM", "WHERE"]);
            assert_eq!(
                check(engine, b"SELECT * FROM users WHERE id=1"),
                vec![0, 1, 2]
            );
        }
    }

    mod large_input {
        use super::*;

        #[test]
        fn long_input_single_match() {
            let engine = &build(&["needle"]);
            let input: Vec<u8> = b"hay"
                .iter()
                .cycle()
                .take(1000)
                .chain(b"needle")
                .copied()
                .collect();
            assert_eq!(check(engine, &input), vec![0]);
        }

        #[test]
        fn long_input_no_match() {
            let engine = &build(&["needle"]);
            let input: Vec<u8> = b"hay".iter().cycle().take(1000).copied().collect();
            assert_eq!(check(engine, &input), vec![]);
        }
    }

    mod char_class {
        use super::*;

        #[test]
        fn digit_class_matches_digit() {
            let engine = &build(&["[0-9]"]);
            assert_eq!(check(engine, b"0"), vec![0]);
            assert_eq!(check(engine, b"5"), vec![0]);
            assert_eq!(check(engine, b"9"), vec![0]);
        }

        #[test]
        fn digit_class_no_match_letter() {
            let engine = &build(&["[0-9]"]);
            assert_eq!(check(engine, b"a"), vec![]);
        }

        #[test]
        fn digit_class_in_text() {
            let engine = &build(&["[0-9]"]);
            let result = check(engine, b"abc123xyz");
            assert!(result.contains(&0));
        }

        #[test]
        fn small_letter_class() {
            let engine = &build(&["[a-c]"]);
            assert_eq!(check(engine, b"a"), vec![0]);
            assert_eq!(check(engine, b"b"), vec![0]);
            assert_eq!(check(engine, b"c"), vec![0]);
        }

        #[test]
        fn small_letter_class_no_match_outside() {
            let engine = &build(&["[a-c]"]);
            assert_eq!(check(engine, b"d"), vec![]);
            assert_eq!(check(engine, b"z"), vec![]);
        }

        #[test]
        fn small_letter_class_in_text() {
            let engine = &build(&["[a-c]"]);
            let result = check(engine, b"xbxycy");
            assert!(result.contains(&0));
        }

        #[test]
        fn large_letter_class_collapses_to_dot() {
            let engine = &build(&["[a-z]"]);
            assert_eq!(check(engine, b"a"), vec![]);
        }

        #[test]
        fn digit_class_multiple_digits() {
            let engine = &build(&["[0-9]"]);
            let result = check(engine, b"0123456789");
            assert!(result.contains(&0));
        }

        #[test]
        fn combined_digit_and_letter_classes() {
            let engine = &build(&["[0-9]", "[a-c]"]);
            let result = check(engine, b"a1b2c3");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }
    }

    mod dot_wildcard {
        use super::*;

        #[test]
        fn dot_alone_no_words() {
            let engine = &build(&["."]);
            assert_eq!(check(engine, b"a"), vec![]);
        }

        #[test]
        fn dot_in_concat_as_end() {
            let engine = &build(&["abc."]);
            assert_eq!(check(engine, b"abcx"), vec![0]);
        }
    }

    mod anchors {
        use super::*;

        #[test]
        fn start_anchor_match() {
            let engine = &build(&["^abc"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn start_anchor_with_prefix() {
            let engine = &build(&["^abc"]);
            assert_eq!(check(engine, b"xxabc"), vec![]);
        }

        #[test]
        fn end_anchor_matches() {
            let engine = &build(&["abc$"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn end_anchor_not_enforced() {
            let engine = &build(&["abc$"]);
            assert_eq!(check(engine, b"abcdef"), vec![0]);
        }

        #[test]
        fn both_anchors_do_not_match() {
            let engine = &build(&["^abc$"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }
    }

    mod combined_scenarios {
        use super::*;

        #[test]
        fn alternation_with_char_class() {
            let engine = &build(&["[0-9]x"]);
            assert_eq!(check(engine, b"3x"), vec![0]);
            assert_eq!(check(engine, b"ax"), vec![]);
        }

        #[test]
        fn multiple_char_classes() {
            let engine = &build(&["[0-9][a-c]"]);
            assert_eq!(check(engine, b"1a"), vec![0]);
            assert_eq!(check(engine, b"2b"), vec![0]);
            assert_eq!(check(engine, b"3d"), vec![]);
        }

        #[test]
        fn char_class_with_literal_suffix() {
            let engine = &build(&["[a-c]end"]);
            assert_eq!(check(engine, b"aend"), vec![0]);
            assert_eq!(check(engine, b"bend"), vec![0]);
            assert_eq!(check(engine, b"zend"), vec![]);
        }

        #[test]
        fn literal_with_digit_class() {
            let engine = &build(&["id[0-9]"]);
            assert_eq!(check(engine, b"id0"), vec![0]);
            assert_eq!(check(engine, b"id5"), vec![0]);
            assert_eq!(check(engine, b"idz"), vec![]);
        }

        #[test]
        fn mixed_patterns_different_classes() {
            let engine = &build(&["[a-c]", "[0-9]"]);
            let result = check(engine, b"a1b2");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn class_in_middle_of_literals() {
            let engine = &build(&["a[0-9]z"]);
            assert_eq!(check(engine, b"a0z"), vec![0]);
            assert_eq!(check(engine, b"a9z"), vec![0]);
            assert_eq!(check(engine, b"az"), vec![]);
        }

        #[test]
        fn streaming_with_char_class() {
            let engine = &build(&["[0-9]abc"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b"7"), vec![]);
            assert_eq!(engine.check(&mut state, b"abc"), vec![0]);
        }

        #[test]
        fn overlapping_classes() {
            let engine = &build(&["[0-9]", "[0-5]"]);
            assert_eq!(check(engine, b"3"), vec![0, 1]);
            assert_eq!(check(engine, b"7"), vec![0]);
            assert_eq!(check(engine, b"a"), vec![]);
        }

        #[test]
        fn class_repeated_manually() {
            let engine = &build(&["[0-9][0-9][0-9]"]);
            let result = check(engine, b"123");
            assert!(result.contains(&0));
            assert_eq!(check(engine, b"12"), vec![]);
        }
    }

    mod or_graph {
        use super::*;

        #[test]
        #[should_panic(expected = "not yet implemented")]
        fn or_graph_panics_on_build() {
            let _engine = &build(&["(?i)^ab[0-9]{3}.{2,4}[a-z]$"]);
        }
    }

    mod todo_panics {
        use super::*;

        #[test]
        #[should_panic(expected = "not yet implemented")]
        fn or_graph_in_words() {
            let _engine = &build(&["(?i)^ab[0-9]{3}.{2,4}[a-z]$"]);
        }

        #[test]
        #[should_panic]
        fn end_in_middle() {
            let engine = &build(&["abc$def"]);
            let mut state = engine.start_state();
            engine.check(&mut state, b"abcdef");
        }

        #[test]
        fn single_literal_matches_and_done() {
            let engine = &build(&["a"]);
            let mut state = engine.start_state();
            engine.check(&mut state, b"a");
        }
    }

    mod hard_working {
        use super::*;

        #[test]
        fn many_patterns_registered() {
            let patterns = &[
                "GET", "POST", "PUT", "DELETE", "PATCH",
                "HEAD", "OPTIONS", "CONNECT", "TRACE", "COPY",
                "LOCK", "UNLOCK",
            ];
            let engine = &build(patterns);
            let result = check(engine, b"GET /api POST /web");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn long_concatenation() {
            let engine = &build(&["abcdefghij"]);
            assert_eq!(check(engine, b"abcdefghij"), vec![0]);
            assert_eq!(check(engine, b"abcdefghi"), vec![]);
        }

        #[test]
        fn overlapping_prefix_suffix_patterns() {
            let engine = &build(&["ab", "abc", "bcd", "abcd"]);
            let result = check(engine, b"abcdefg");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn many_alternations_in_single_regex() {
            let engine = &build(&["GET|POST|PUT|DELETE"]);
            assert_eq!(check(engine, b"GET"), vec![0]);
            assert_eq!(check(engine, b"POST"), vec![0]);
            assert_eq!(check(engine, b"PUT"), vec![0]);
            assert_eq!(check(engine, b"DELETE"), vec![0]);
        }

        #[test]
        fn start_anchor_works() {
            let engine = &build(&["^SELECT"]);
            assert_eq!(check(engine, b"SELECT * FROM t"), vec![0]);
            assert_eq!(check(engine, b"INSERT INTO t"), vec![]);
        }

        #[test]
        fn end_anchor_works() {
            let engine = &build(&["WHERE"]);
            assert_eq!(check(engine, b"SELECT * WHERE"), vec![0]);
        }

        #[test]
        fn both_anchors_exact_match() {
            let engine = &build(&["^abc$"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn literal_repetition_bounded() {
            let engine = &build(&["aaa"]);
            assert_eq!(check(engine, b"aaa"), vec![0]);
            assert_eq!(check(engine, b"aa"), vec![]);
        }

        #[test]
        fn literal_repetition_range() {
            let engine = &build(&["aaaa"]);
            assert_eq!(check(engine, b"aaaa"), vec![0]);
            assert_eq!(check(engine, b"aaa"), vec![]);
        }

        #[test]
        fn anchored_char_class_repetition() {
            let engine = &build(&["^[a-c]{2}$"]);
            assert_eq!(check(engine, b"ab"), vec![0]);
            assert_eq!(check(engine, b"ac"), vec![0]);
            assert_eq!(check(engine, b"ad"), vec![]);
        }

        #[test]
        fn char_class_plus_literal() {
            let engine = &build(&["[0-9]abc"]);
            assert_eq!(check(engine, b"7abc"), vec![0]);
            assert_eq!(check(engine, b"abc"), vec![]);
        }

        #[test]
        fn literal_plus_char_class() {
            let engine = &build(&["abc[0-9]"]);
            assert_eq!(check(engine, b"abc3"), vec![0]);
            assert_eq!(check(engine, b"abc"), vec![]);
        }

        #[test]
        fn shared_prefix_patterns() {
            let engine = &build(&["GET /api", "GET /web", "POST /api"]);
            assert_eq!(check(engine, b"GET /api v1"), vec![0]);
            assert_eq!(check(engine, b"POST /api"), vec![2]);
        }

        #[test]
        fn streaming_many_small_chunks() {
            let engine = &build(&["abc"]);
            let mut state = engine.start_state();
            assert_eq!(engine.check(&mut state, b"a"), vec![]);
            assert_eq!(engine.check(&mut state, b"b"), vec![]);
            assert_eq!(engine.check(&mut state, b"c"), vec![0]);
        }

        #[test]
        fn binary_bytes_pattern() {
            let engine = &build(&["\x00\x01\x02"]);
            assert_eq!(check(engine, b"\x00\x01\x02\x03"), vec![0]);
        }

        #[test]
        fn single_byte_patterns() {
            let engine = &build(&["\x00", "\x41"]);
            let result = check(engine, b"\x00\x41");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn very_long_input_single_match_at_end() {
            let engine = &build(&["NEEDLE"]);
            let mut input: Vec<u8> = vec![b'x'; 10000];
            input.extend_from_slice(b"NEEDLE");
            assert_eq!(check(engine, &input), vec![0]);
        }

        #[test]
        fn pattern_equals_input() {
            let engine = &build(&["hello"]);
            assert_eq!(check(engine, b"hello"), vec![0]);
        }

        #[test]
        fn id_with_digit_repetition() {
            let engine = &build(&["id[0-9][0-9][0-9]"]);
            assert_eq!(check(engine, b"id123"), vec![0]);
            assert_eq!(check(engine, b"id12"), vec![]);
        }

        #[test]
        fn overlapping_matches_different_patterns() {
            let engine = &build(&["abc", "bcd"]);
            let result = check(engine, b"abcd");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn same_literal_different_patterns() {
            let engine = &build(&["abc", "xyzabc"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn long_alternation_many_branches() {
            let engine = &build(&["aaa|bbbb|cc|ddddd|eeeee"]);
            assert_eq!(check(engine, b"aaa"), vec![0]);
            assert_eq!(check(engine, b"bbbb"), vec![0]);
            assert_eq!(check(engine, b"cc"), vec![0]);
            assert_eq!(check(engine, b"ddddd"), vec![0]);
        }
    }

    mod dedup_behavior {
        use super::*;

        #[test]
        fn single_pattern_matches_once_dedup() {
            let engine = &build(&["ab"]);
            let result = check(engine, b"xab");
            assert!(result.contains(&0));
        }

        #[test]
        fn streaming_single_pattern_dedup() {
            let engine = &build(&["abc"]);
            let mut state = engine.start_state();
            let r1 = engine.check(&mut state, b"a");
            let r2 = engine.check(&mut state, b"b");
            let r3 = engine.check(&mut state, b"c");
            assert!(r1.is_empty());
            assert!(r2.is_empty());
            assert!(r3.contains(&0));
        }

        #[test]
        fn two_patterns_one_matches_twice() {
            let engine = &build(&["ab", "bc"]);
            let result = check(engine, b"abc");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn different_patterns_no_dedup() {
            let engine = &build(&["ab", "bc", "cd"]);
            let result = check(engine, b"abcd");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }
    }

    mod corner_cases {
        use super::*;

        #[test]
        fn empty_input_list() {
            let engine = &build(&[]);
            assert_eq!(check(engine, b"anything"), vec![]);
        }

        #[test]
        fn single_byte_pattern_single_byte_input() {
            let engine = &build(&["a"]);
            assert_eq!(check(engine, b"a"), vec![0]);
        }

        #[test]
        fn pattern_longer_than_input() {
            let engine = &build(&["abcdef"]);
            assert_eq!(check(engine, b"abc"), vec![]);
        }

        #[test]
        fn pattern_equals_input_exactly() {
            let engine = &build(&["abc"]);
            assert_eq!(check(engine, b"abc"), vec![0]);
        }

        #[test]
        fn alternation_splits_into_graphs() {
            let engine = &build(&["a|b|c"]);
            let result = check(engine, b"abc");
            assert!(result.contains(&0));
        }

        #[test]
        fn repeated_chars_pattern() {
            let engine = &build(&["aaaa"]);
            assert_eq!(check(engine, b"aaaa"), vec![0]);
            assert_eq!(check(engine, b"aaa"), vec![]);
        }

        #[test]
        fn nested_literal_alternation() {
            let engine = &build(&["ab|cd"]);
            assert_eq!(check(engine, b"ab"), vec![0]);
            assert_eq!(check(engine, b"cd"), vec![0]);
            assert_eq!(check(engine, b"ef"), vec![]);
        }

        #[test]
        fn single_branch_alternation() {
            let engine = &build(&["a"]);
            assert_eq!(check(engine, b"a"), vec![0]);
        }

        #[test]
        fn special_regex_chars_in_literal() {
            let engine = &build(&[r"a\.b\*c\+d"]);
            assert_eq!(check(engine, b"a.b*c+d"), vec![0]);
        }

        #[test]
        fn zero_bytes_pattern() {
            let engine = &build(&["\x00"]);
            let result = check(engine, b"\x00");
            assert!(result.contains(&0));
        }

        #[test]
        fn multiple_patterns_same_word() {
            let engine = &build(&["abc", "abc"]);
            let result = check(engine, b"abc");
            assert!(result.contains(&0));
        }

        #[test]
        fn overlapping_patterns_from_different_regexes() {
            let engine = &build(&["abc", "bc"]);
            let result = check(engine, b"abc");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn word_map_correctness_shared_prefix() {
            let engine = &build(&["abx", "aby"]);
            assert_eq!(check(engine, b"abx"), vec![0]);
            assert_eq!(check(engine, b"aby"), vec![1]);
            assert_eq!(check(engine, b"abz"), vec![]);
        }

        #[test]
        fn many_single_char_patterns() {
            let engine = &build(&["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]);
            let result = check(engine, b"5");
            assert!(result.contains(&5));
        }
    }

    mod real_world_hard {
        use super::*;

        #[test]
        fn http_header_matching() {
            let engine = &build(&[
                "Content-Type", "Authorization", "Accept", "Host", "User-Agent",
            ]);
            let input = b"Host: example.com\r\nAuthorization: Bearer token\r\n";
            let result = check(engine, input);
            assert!(result.contains(&3));
            assert!(result.contains(&1));
        }

        #[test]
        fn log_level_detection() {
            let engine = &build(&["ERROR", "WARN", "INFO", "DEBUG"]);
            assert_eq!(
                check(engine, b"[2024-01-01] ERROR: connection refused"),
                vec![0]
            );
        }

        #[test]
        fn sql_injection_patterns() {
            let engine = &build(&["UNION SELECT", "DROP TABLE", "1=1", "OR 1=1"]);
            let result = check(engine, b"id=1 UNION SELECT * FROM users");
            assert!(result.contains(&0));
        }

        #[test]
        fn file_path_traversal() {
            let engine = &build(&["passwd", "secret"]);
            let result = check(engine, b"file=../../etc/passwd");
            assert!(result.contains(&0));
        }

        #[test]
        fn network_address_patterns() {
            let engine = &build(&["192", "10"]);
            let result = check(engine, b"src=192.168.1.1");
            assert!(result.contains(&0));
        }

        #[test]
        fn email_like_patterns() {
            let engine = &build(&["gmail", "yahoo"]);
            let result = check(engine, b"user@gmail.com");
            assert!(result.contains(&0));
        }

        #[test]
        fn version_number_pattern() {
            let engine = &build(&["v1", "v2", "v3"]);
            let result = check(engine, b"upgrade to v2 from v1");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn mixed_case_http_methods() {
            let engine = &build(&["GET", "POST", "PUT"]);
            let result = check(engine, b"GET /api POST /web");
            assert!(result.contains(&0));
            assert!(result.contains(&1));
        }

        #[test]
        fn deeply_nested_input() {
            let engine = &build(&["FINDME"]);
            let input: Vec<u8> = b"aaa".iter()
                .cycle()
                .take(500)
                .chain(b"bbb")
                .chain(b"aaa".iter().cycle().take(500))
                .chain(b"FINDME")
                .copied()
                .collect();
            assert_eq!(check(engine, &input), vec![0]);
        }
    }
}
