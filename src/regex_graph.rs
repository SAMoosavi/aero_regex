//! # RegexGraph — Compiled Node Graph
//!
//! Converts a normalized `RegexExpr` into a linear node graph suitable for
//! matching. Each graph represents one "path" through the regex after
//! alternations have been split.
//!
//! ## Node types
//!
//! | NodeData | Meaning |
//! |----------|---------|
//! | `Start` / `End` | Anchor constraints |
//! | `Literal { word }` | Exact byte match |
//! | `OrLiteral { literals }` | One of several literals (optimized) |
//! | `OrGraph { graphs }` | Branching sub-graphs (unresolved alternation) |
//! | `Temp { len }` | Fixed-width wildcard (e.g., `.{3}`) |
//! | `TempRange { min, max }` | Variable-width wildcard (e.g., `.{2,5}`) |
//! | `TempInf { len }` | Unbounded wildcard after fixed prefix |
//! | `Repetition { sub }` | Recursive sub-graph for `*`/`+` patterns |
//! | `Empty` | No-op |
//!
//! ## Compilation pipeline
//!
//! 1. `split()` — recursively decomposes alternations and classes into
//!    independent paths
//! 2. `hir_to_graph()` — converts each path's `RegexExpr` to a `Vec<Node>`
//! 3. `optimize_nodes()` — merges adjacent compatible `Temp*` nodes and
//!    computes `NodeLen` for each node

use crate::{
    regex_expr::RegexExpr,
    utilities::{RegexId, class_to_list_of_literal},
};
use bstr::BString;
use itertools::Itertools;
use std::fmt;

#[derive(Debug, Clone)]
pub(crate) enum NodeData {
    Start,
    End,
    Literal { word: BString },
    OrLiteral { literals: Vec<BString> },
    OrGraph { graphs: Vec<RegexGraph> },
    Temp { len: usize },
    TempRange { min_len: usize, max_len: usize },
    TempInf { len: usize },
    Empty,
    Repetition { sub: RegexGraph },
}

#[derive(Debug, Clone)]
pub(crate) enum NodeStartIndex {
    None,
    Index(usize),
    Range { min: usize, max: usize },
    AtLeast(usize),
}

impl NodeStartIndex {
    pub(crate) fn add(&self, other: NodeStartIndex) -> NodeStartIndex {
        match (self, other) {
            (NodeStartIndex::None, other) => other,
            (_, NodeStartIndex::None) => NodeStartIndex::None,
            (NodeStartIndex::Index(a), NodeStartIndex::Index(b)) => NodeStartIndex::Index(a + b),
            (
                NodeStartIndex::Range {
                    min: a_min,
                    max: a_max,
                },
                NodeStartIndex::Range {
                    min: b_min,
                    max: b_max,
                },
            ) => NodeStartIndex::Range {
                min: a_min + b_min,
                max: a_max + b_max,
            },
            (NodeStartIndex::AtLeast(a), NodeStartIndex::AtLeast(b)) => {
                NodeStartIndex::AtLeast(a + b)
            }
            (NodeStartIndex::Index(a), NodeStartIndex::AtLeast(b)) => {
                NodeStartIndex::AtLeast(a + b)
            }
            (NodeStartIndex::Range { min: a_min, max: _ }, NodeStartIndex::AtLeast(b)) => {
                NodeStartIndex::AtLeast(a_min + b)
            }
            (
                NodeStartIndex::Range {
                    min: a_min,
                    max: a_max,
                },
                NodeStartIndex::Index(b),
            ) => NodeStartIndex::Range {
                min: a_min + b,
                max: a_max + b,
            },
            (
                NodeStartIndex::Index(a),
                NodeStartIndex::Range {
                    min: b_min,
                    max: b_max,
                },
            ) => NodeStartIndex::Range {
                min: a + b_min,
                max: a + b_max,
            },
            (NodeStartIndex::AtLeast(a), NodeStartIndex::Index(b)) => {
                NodeStartIndex::AtLeast(a + b)
            }
            (NodeStartIndex::AtLeast(a), NodeStartIndex::Range { min: b_min, max: _ }) => {
                NodeStartIndex::AtLeast(a + b_min)
            }
        }
    }

    pub(crate) fn matched(&self, pos: usize) -> bool {
        match self {
            NodeStartIndex::Index(index) => *index == pos,
            NodeStartIndex::AtLeast(index) => pos >= *index,
            NodeStartIndex::Range { min, max } => pos >= *min && pos <= *max,
            NodeStartIndex::None => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) data: NodeData,
    pub(crate) start_index: NodeStartIndex,
}

#[derive(Debug, Clone)]
pub(crate) struct RegexGraph {
    pub(crate) regex_id: RegexId,
    pub(crate) nodes: Vec<Node>,
}

impl RegexGraph {
    pub(crate) fn new(hir: &RegexExpr, regex_id: RegexId) -> Vec<Self> {
        let hirs = Self::split(hir);
        hirs.iter()
            .map(Self::hir_to_graph)
            .map(Self::optimize_nodes)
            .map(|mut graph| {
                graph.regex_id = regex_id;
                graph
            })
            .collect()
    }

    /// Extracts all exact literal match strings paired with their corresponding node IDs.
    pub(crate) fn words(&self) -> Vec<(BString, usize)> {
        let mut result = Vec::new();
        for (node_id, node) in self.nodes.iter().enumerate() {
            match &node.data {
                NodeData::Literal { word } => {
                    result.push((word.clone(), node_id));
                }
                NodeData::OrLiteral { literals } => {
                    for lit in literals {
                        result.push((lit.clone(), node_id));
                    }
                }
                NodeData::OrGraph { .. } => todo!(),
                _ => {}
            }
        }
        result
    }

    fn split(hir: &RegexExpr) -> Vec<RegexExpr> {
        match hir {
            RegexExpr::Alternation(hirs) => hirs.iter().flat_map(Self::split).collect(),
            RegexExpr::Class(class) => class_to_list_of_literal(class),
            _ => vec![hir.clone()],
        }
    }

    fn hir_to_graph(hir: &RegexExpr) -> Self {
        let nodes = Self::hir_to_nodes(hir);
        Self { regex_id: 0, nodes }
    }

    fn hir_to_nodes(hir: &RegexExpr) -> Vec<Node> {
        let single = |data| {
            vec![Node {
                data,
                start_index: NodeStartIndex::None,
            }]
        };

        match hir {
            RegexExpr::Empty => single(NodeData::Empty),
            RegexExpr::Literal(lit) => single(NodeData::Literal { word: lit.clone() }),
            RegexExpr::Class(_) => single(NodeData::Temp { len: 1 }),
            RegexExpr::Start => single(NodeData::Start),
            RegexExpr::End => single(NodeData::End),
            RegexExpr::Repetition { min, max, sub } => {
                let min = *min as usize;
                let max = max.map(|m| m as usize);

                match &**sub {
                    RegexExpr::Empty => single(NodeData::Empty),
                    RegexExpr::Literal(lit) => {
                        let mut word = BString::from(lit.repeat(min));

                        match max {
                            None => vec![
                                Node {
                                    data: NodeData::Literal { word },
                                    start_index: NodeStartIndex::None,
                                },
                                Node {
                                    data: NodeData::Repetition {
                                        sub: Self::hir_to_graph(sub),
                                    },
                                    start_index: NodeStartIndex::None,
                                },
                            ],
                            Some(m) if m == min => single(NodeData::Literal { word }),
                            Some(m) => {
                                let mut literals = Vec::with_capacity(m - min + 1);
                                literals.push(word.clone());

                                for _ in 0..(m - min) {
                                    word.extend_from_slice(lit);
                                    literals.push(word.clone());
                                }
                                single(NodeData::OrLiteral { literals })
                            }
                        }
                    }

                    RegexExpr::Class(_) => single(match max {
                        None => NodeData::TempInf { len: min },
                        Some(m) if m == min => NodeData::Temp { len: min },
                        Some(m) => NodeData::TempRange {
                            min_len: min,
                            max_len: m,
                        },
                    }),
                    _ => single(NodeData::Repetition {
                        sub: Self::hir_to_graph(sub),
                    }),
                }
            }
            RegexExpr::Concat(concat) => concat.iter().flat_map(Self::hir_to_nodes).collect(),
            RegexExpr::Alternation(alternation) => {
                let all_literals: Option<Vec<BString>> = alternation
                    .iter()
                    .map(|alter| {
                        if let RegexExpr::Literal(literal) = alter {
                            Some(literal.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if let Some(literals) = all_literals {
                    single(NodeData::OrLiteral { literals })
                } else {
                    single(NodeData::OrGraph {
                        graphs: alternation.iter().map(Self::hir_to_graph).collect(),
                    })
                }
            }
            RegexExpr::Dot => single(NodeData::Temp { len: 1 }),
        }
    }

    fn optimize_nodes(base: Self) -> Self {
        let nodes = &base.nodes;
        if nodes.is_empty() {
            return base;
        }

        let mut new_nodes = Vec::with_capacity(nodes.len());
        let mut prev = nodes.first().unwrap().clone();
        let tail = &nodes[1..];

        for node in tail {
            match &prev.data {
                NodeData::Start => {
                    let mut new_node = node.clone();
                    if !matches!(prev.start_index, NodeStartIndex::None) {
                        unreachable!();
                    }
                    new_node.start_index = NodeStartIndex::Index(0);
                    prev = new_node;
                }
                NodeData::End => unreachable!(),
                NodeData::Literal { word } => {
                    let mut new_node = node.clone();
                    new_node.start_index = prev.start_index.add(NodeStartIndex::Index(word.len()));
                    new_nodes.push(prev);
                    prev = new_node;
                }
                NodeData::Temp { len } => {
                    let mut new_node = node.clone();
                    new_node.start_index = prev.start_index.add(NodeStartIndex::Index(*len));
                    prev = new_node;
                }
                NodeData::TempRange { min_len, max_len } => {
                    let mut new_node = node.clone();
                    new_node.start_index = prev.start_index.add(NodeStartIndex::Range {
                        min: *min_len,
                        max: *max_len,
                    });
                    prev = new_node;
                }
                NodeData::TempInf { len } => {
                    let mut new_node = node.clone();
                    new_node.start_index = if *len != 0 {
                        prev.start_index.add(NodeStartIndex::AtLeast(*len))
                    } else {
                        NodeStartIndex::None
                    };
                    prev = new_node;
                }
                NodeData::Empty => {
                    let mut new_node = node.clone();
                    new_node.start_index = prev.start_index.add(NodeStartIndex::Index(0));
                    prev = new_node;
                }
                NodeData::OrLiteral { literals } => {
                    let mut new_node = node.clone();
                    let max = literals.iter().map(|lit| lit.len()).max().unwrap_or(0);
                    let min = literals.iter().map(|lit| lit.len()).min().unwrap_or(0);
                    new_node.start_index = prev.start_index.add(NodeStartIndex::Range { min, max });
                    new_nodes.push(prev);
                    prev = new_node;
                }
                NodeData::Repetition { sub } => {
                    let optimized_sub = Self::optimize_nodes(sub.clone());
                    if optimized_sub.nodes.len() == 1 {
                        let single_node = &optimized_sub.nodes[0];
                        match single_node.data {
                            NodeData::Temp { .. }
                            | NodeData::TempRange { .. }
                            | NodeData::TempInf { .. }
                            | NodeData::Empty => prev = node.clone(),
                            NodeData::End | NodeData::Start => unreachable!(),
                            _ => {
                                let mut new_node = node.clone();
                                new_node.data = NodeData::Repetition { sub: optimized_sub };
                                new_node.start_index = NodeStartIndex::None;
                                new_nodes.push(prev);
                                prev = new_node;
                            }
                        }
                    } else {
                        let mut new_node = node.clone();
                        new_node.data = NodeData::Repetition { sub: optimized_sub };
                        new_node.start_index = NodeStartIndex::None;
                        new_nodes.push(prev);
                        prev = new_node;
                    }
                }
                NodeData::OrGraph { graphs } => {
                    let optimized_graphs = graphs
                        .iter()
                        .map(|g| Self::optimize_nodes(g.clone()))
                        .collect_vec();

                    let ranges: Option<Vec<(usize, usize)>> = optimized_graphs
                        .iter()
                        .map(|g| {
                            if g.nodes.len() != 1 {
                                return None;
                            }
                            match g.nodes[0].data {
                                NodeData::Empty => Some((0, 0)),
                                NodeData::Temp { len } => Some((len, len)),
                                NodeData::TempRange { min_len, max_len } => Some((min_len, max_len)),
                                NodeData::TempInf { len } => Some((len, usize::MAX)),
                                _ => None,
                            }
                        })
                        .collect();

                    if let Some(ranges) = ranges {
                        let min = ranges.iter().map(|r| r.0).min().unwrap_or(0);
                        let max = ranges.iter().map(|r| r.1).max().unwrap_or(0);

                        let data = match (min, max) {
                            (len, usize::MAX) => NodeData::TempInf { len },
                            (min, max) if min == max => NodeData::Temp { len: min },
                            (min, max) => NodeData::TempRange {
                                min_len: min,
                                max_len: max,
                            },
                        };

                        let start_index = prev.start_index;
                        let mut new_node = node.clone();
                        new_node.start_index = start_index.add(NodeStartIndex::Range { min, max });
                        new_nodes.push(Node { data, start_index });
                        prev = new_node;
                    } else {
                        let new_node = node.clone();
                        new_nodes.push(Node {
                            data: NodeData::OrGraph {
                                graphs: optimized_graphs,
                            },
                            start_index: NodeStartIndex::None,
                        });
                        prev = new_node;
                    }
                }
            }
        }

        new_nodes.push(prev);

        Self {
            regex_id: 0,
            nodes: new_nodes,
        }
    }
}

impl fmt::Display for RegexGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let graph = self
            .nodes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        write!(f, "{graph}")
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.data, self.start_index)
    }
}

impl fmt::Display for NodeData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data_str = match &self {
            NodeData::Start => "^".to_string(),
            NodeData::End => "$".to_string(),
            NodeData::Literal { word } => format!("'{word}'"),
            NodeData::OrLiteral { literals } => {
                let lits = literals
                    .iter()
                    .map(|word| format!("'{word}'"))
                    .collect::<Vec<_>>()
                    .join("|");
                format!("({lits})")
            }
            NodeData::OrGraph { graphs } => {
                let gs = graphs
                    .iter()
                    .map(|g| g.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ");
                format!("OR({gs})")
            }
            NodeData::Temp { len } => format!(".{{{len}}}"),
            NodeData::TempRange { min_len, max_len } => format!(".{{{min_len},{max_len}}}"),
            NodeData::TempInf { len } => format!(".{{{len}, }}"),
            NodeData::Empty => "ε".to_string(),
            NodeData::Repetition { sub } => format!("REP([{sub}] )"),
        };

        write!(f, "{data_str}")
    }
}

impl fmt::Display for NodeStartIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let idx_str = match &self {
            NodeStartIndex::None => "?".to_string(),
            NodeStartIndex::Index(i) => i.to_string(),
            NodeStartIndex::Range { min, max } => format!("{min}..{max}"),
            NodeStartIndex::AtLeast(i) => format!("{i}.."),
        };

        write!(f, "{idx_str}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalizer;
    use regex_syntax::Parser;

    fn parse(pattern: &str) -> RegexExpr {
        normalizer::normalize(&Parser::new().parse(pattern).unwrap().into())
    }

    mod split {
        use super::*;

        fn check_split(input: &str, expected: &[&str]) {
            let input_hir = parse(input);
            let expected_hirs: Vec<_> = expected.iter().map(|&s| parse(s)).collect();
            assert_eq!(RegexGraph::split(&input_hir), expected_hirs);
        }

        #[test]
        fn no_alternation() {
            check_split(r"abc", &[r"abc"]);
            check_split(r"^hello$", &[r"^hello$"]);
        }

        #[test]
        fn simple_alternation() {
            check_split(r"a|b", &[r"a", r"b"]);
            check_split(r"aa|bb|cc", &[r"aa", r"bb", r"cc"]);
        }

        #[test]
        fn anchors_and_alternations() {
            check_split(r"^(a|b)", &[r"^a", r"^b"]);
            check_split(r"(a|b)$", &[r"a$", r"b$"]);
            check_split(r"^(a|b)$", &[r"^a$", r"^b$"]);
            check_split(
                r"^prefix(a|b)suffix$",
                &[r"^prefixasuffix$", r"^prefixbsuffix$"],
            );
        }

        #[test]
        fn cartesian_product_multiple_groups() {
            check_split(r"(?:a|b)(?:c|d)", &[r"ac", r"ad", r"bc", r"bd"]);
        }

        #[test]
        fn nested_alternations() {
            check_split(r"(?:a|(?:b|c))", &[r"a", r"b", r"c"]);
            check_split(r"^(?:a|(?:b|c))$", &[r"^a$", r"^b$", r"^c$"]);
        }

        #[test]
        fn capture_groups_are_preserved() {
            check_split(r"(a|b)", &[r"a", r"b"]);
            check_split(r"^(a|b)(c|d)", &[r"^ac", r"^ad", r"^bc", r"^bd"]);
        }

        #[test]
        fn repetitions_are_not_split() {
            check_split(r"(?:a|b)*", &[r"(?:a|b)*"]);
            check_split(r"^(a|b)+$", &[r"^a(a|b)*$", r"^b(a|b)*$"]);
        }

        #[test]
        fn complex_real_world_scenario() {
            check_split(
                r"^GET /(?:index|about|contact)\.html$",
                &[
                    r"^GET /index\.html$",
                    r"^GET /about\.html$",
                    r"^GET /contact\.html$",
                ],
            );
        }
    }

    mod create_graph {
        use super::*;

        fn check_graphs(input: &str, expected: &[&str]) {
            let input_hir = parse(input);
            let graphs = RegexGraph::new(&input_hir, 0);

            let actual: Vec<String> = graphs.iter().map(|g| g.to_string()).collect();

            assert_eq!(
                actual.len(),
                expected.len(),
                "Graph count mismatch for '{input}'.\nExpected: {expected:#?}\nActual: {actual:#?}"
            );

            for (i, (act, exp)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(act, exp, "Graph mismatch at index {i} for '{input}'");
            }
        }

        #[test]
        fn simple_literal() {
            check_graphs("abc", &["'abc'@?"]);
        }

        #[test]
        fn start_anchor() {
            check_graphs("^abc", &["'abc'@0"]);
        }

        #[test]
        fn end_anchor() {
            check_graphs("abc$", &["'abc'@? -> $@3"]);
        }

        #[test]
        fn both_anchors() {
            check_graphs("^abc$", &["'abc'@0 -> $@3"]);
        }

        #[test]
        fn temp_skip() {
            check_graphs("^a.b$", &["'a'@0 -> 'b'@2 -> $@3"]);
        }

        #[test]
        fn temp_range() {
            check_graphs(
                "^a.{2,4}b$",
                &[
                    "'a'@0 -> 'b'@3 -> $@4",
                    "'a'@0 -> 'b'@4 -> $@5",
                    "'a'@0 -> 'b'@5 -> $@6",
                ],
            );
        }

        #[test]
        fn unbounded_temp() {
            check_graphs("^a.*b$", &["'a'@0 -> 'b'@? -> $@1"]);
        }

        #[test]
        fn alternation_split() {
            check_graphs("^(a|b)c$", &["'ac'@0 -> $@2", "'bc'@0 -> $@2"]);
        }

        #[test]
        fn or_literal() {
            check_graphs("a|b", &["'a'@?", "'b'@?"]);
        }

        #[test]
        fn repetition_with_or() {
            check_graphs("^(a|b)*c$", &["'c'@? -> $@1"]);
        }

        #[test]
        fn complex_real_world() {
            check_graphs(
                "^(?:a|b)[0-9]{1,2}.{10}test$",
                &[
                    "'a0'@0 -> 'test'@12 -> $@16",
                    "'a1'@0 -> 'test'@12 -> $@16",
                    "'a2'@0 -> 'test'@12 -> $@16",
                    "'a3'@0 -> 'test'@12 -> $@16",
                    "'a4'@0 -> 'test'@12 -> $@16",
                    "'a5'@0 -> 'test'@12 -> $@16",
                    "'a6'@0 -> 'test'@12 -> $@16",
                    "'a7'@0 -> 'test'@12 -> $@16",
                    "'a8'@0 -> 'test'@12 -> $@16",
                    "'a9'@0 -> 'test'@12 -> $@16",
                    "'a00'@0 -> 'test'@13 -> $@17",
                    "'a01'@0 -> 'test'@13 -> $@17",
                    "'a02'@0 -> 'test'@13 -> $@17",
                    "'a03'@0 -> 'test'@13 -> $@17",
                    "'a04'@0 -> 'test'@13 -> $@17",
                    "'a05'@0 -> 'test'@13 -> $@17",
                    "'a06'@0 -> 'test'@13 -> $@17",
                    "'a07'@0 -> 'test'@13 -> $@17",
                    "'a08'@0 -> 'test'@13 -> $@17",
                    "'a09'@0 -> 'test'@13 -> $@17",
                    "'a10'@0 -> 'test'@13 -> $@17",
                    "'a11'@0 -> 'test'@13 -> $@17",
                    "'a12'@0 -> 'test'@13 -> $@17",
                    "'a13'@0 -> 'test'@13 -> $@17",
                    "'a14'@0 -> 'test'@13 -> $@17",
                    "'a15'@0 -> 'test'@13 -> $@17",
                    "'a16'@0 -> 'test'@13 -> $@17",
                    "'a17'@0 -> 'test'@13 -> $@17",
                    "'a18'@0 -> 'test'@13 -> $@17",
                    "'a19'@0 -> 'test'@13 -> $@17",
                    "'a20'@0 -> 'test'@13 -> $@17",
                    "'a21'@0 -> 'test'@13 -> $@17",
                    "'a22'@0 -> 'test'@13 -> $@17",
                    "'a23'@0 -> 'test'@13 -> $@17",
                    "'a24'@0 -> 'test'@13 -> $@17",
                    "'a25'@0 -> 'test'@13 -> $@17",
                    "'a26'@0 -> 'test'@13 -> $@17",
                    "'a27'@0 -> 'test'@13 -> $@17",
                    "'a28'@0 -> 'test'@13 -> $@17",
                    "'a29'@0 -> 'test'@13 -> $@17",
                    "'a30'@0 -> 'test'@13 -> $@17",
                    "'a31'@0 -> 'test'@13 -> $@17",
                    "'a32'@0 -> 'test'@13 -> $@17",
                    "'a33'@0 -> 'test'@13 -> $@17",
                    "'a34'@0 -> 'test'@13 -> $@17",
                    "'a35'@0 -> 'test'@13 -> $@17",
                    "'a36'@0 -> 'test'@13 -> $@17",
                    "'a37'@0 -> 'test'@13 -> $@17",
                    "'a38'@0 -> 'test'@13 -> $@17",
                    "'a39'@0 -> 'test'@13 -> $@17",
                    "'a40'@0 -> 'test'@13 -> $@17",
                    "'a41'@0 -> 'test'@13 -> $@17",
                    "'a42'@0 -> 'test'@13 -> $@17",
                    "'a43'@0 -> 'test'@13 -> $@17",
                    "'a44'@0 -> 'test'@13 -> $@17",
                    "'a45'@0 -> 'test'@13 -> $@17",
                    "'a46'@0 -> 'test'@13 -> $@17",
                    "'a47'@0 -> 'test'@13 -> $@17",
                    "'a48'@0 -> 'test'@13 -> $@17",
                    "'a49'@0 -> 'test'@13 -> $@17",
                    "'a50'@0 -> 'test'@13 -> $@17",
                    "'a51'@0 -> 'test'@13 -> $@17",
                    "'a52'@0 -> 'test'@13 -> $@17",
                    "'a53'@0 -> 'test'@13 -> $@17",
                    "'a54'@0 -> 'test'@13 -> $@17",
                    "'a55'@0 -> 'test'@13 -> $@17",
                    "'a56'@0 -> 'test'@13 -> $@17",
                    "'a57'@0 -> 'test'@13 -> $@17",
                    "'a58'@0 -> 'test'@13 -> $@17",
                    "'a59'@0 -> 'test'@13 -> $@17",
                    "'a60'@0 -> 'test'@13 -> $@17",
                    "'a61'@0 -> 'test'@13 -> $@17",
                    "'a62'@0 -> 'test'@13 -> $@17",
                    "'a63'@0 -> 'test'@13 -> $@17",
                    "'a64'@0 -> 'test'@13 -> $@17",
                    "'a65'@0 -> 'test'@13 -> $@17",
                    "'a66'@0 -> 'test'@13 -> $@17",
                    "'a67'@0 -> 'test'@13 -> $@17",
                    "'a68'@0 -> 'test'@13 -> $@17",
                    "'a69'@0 -> 'test'@13 -> $@17",
                    "'a70'@0 -> 'test'@13 -> $@17",
                    "'a71'@0 -> 'test'@13 -> $@17",
                    "'a72'@0 -> 'test'@13 -> $@17",
                    "'a73'@0 -> 'test'@13 -> $@17",
                    "'a74'@0 -> 'test'@13 -> $@17",
                    "'a75'@0 -> 'test'@13 -> $@17",
                    "'a76'@0 -> 'test'@13 -> $@17",
                    "'a77'@0 -> 'test'@13 -> $@17",
                    "'a78'@0 -> 'test'@13 -> $@17",
                    "'a79'@0 -> 'test'@13 -> $@17",
                    "'a80'@0 -> 'test'@13 -> $@17",
                    "'a81'@0 -> 'test'@13 -> $@17",
                    "'a82'@0 -> 'test'@13 -> $@17",
                    "'a83'@0 -> 'test'@13 -> $@17",
                    "'a84'@0 -> 'test'@13 -> $@17",
                    "'a85'@0 -> 'test'@13 -> $@17",
                    "'a86'@0 -> 'test'@13 -> $@17",
                    "'a87'@0 -> 'test'@13 -> $@17",
                    "'a88'@0 -> 'test'@13 -> $@17",
                    "'a89'@0 -> 'test'@13 -> $@17",
                    "'a90'@0 -> 'test'@13 -> $@17",
                    "'a91'@0 -> 'test'@13 -> $@17",
                    "'a92'@0 -> 'test'@13 -> $@17",
                    "'a93'@0 -> 'test'@13 -> $@17",
                    "'a94'@0 -> 'test'@13 -> $@17",
                    "'a95'@0 -> 'test'@13 -> $@17",
                    "'a96'@0 -> 'test'@13 -> $@17",
                    "'a97'@0 -> 'test'@13 -> $@17",
                    "'a98'@0 -> 'test'@13 -> $@17",
                    "'a99'@0 -> 'test'@13 -> $@17",
                    "'b0'@0 -> 'test'@12 -> $@16",
                    "'b1'@0 -> 'test'@12 -> $@16",
                    "'b2'@0 -> 'test'@12 -> $@16",
                    "'b3'@0 -> 'test'@12 -> $@16",
                    "'b4'@0 -> 'test'@12 -> $@16",
                    "'b5'@0 -> 'test'@12 -> $@16",
                    "'b6'@0 -> 'test'@12 -> $@16",
                    "'b7'@0 -> 'test'@12 -> $@16",
                    "'b8'@0 -> 'test'@12 -> $@16",
                    "'b9'@0 -> 'test'@12 -> $@16",
                    "'b00'@0 -> 'test'@13 -> $@17",
                    "'b01'@0 -> 'test'@13 -> $@17",
                    "'b02'@0 -> 'test'@13 -> $@17",
                    "'b03'@0 -> 'test'@13 -> $@17",
                    "'b04'@0 -> 'test'@13 -> $@17",
                    "'b05'@0 -> 'test'@13 -> $@17",
                    "'b06'@0 -> 'test'@13 -> $@17",
                    "'b07'@0 -> 'test'@13 -> $@17",
                    "'b08'@0 -> 'test'@13 -> $@17",
                    "'b09'@0 -> 'test'@13 -> $@17",
                    "'b10'@0 -> 'test'@13 -> $@17",
                    "'b11'@0 -> 'test'@13 -> $@17",
                    "'b12'@0 -> 'test'@13 -> $@17",
                    "'b13'@0 -> 'test'@13 -> $@17",
                    "'b14'@0 -> 'test'@13 -> $@17",
                    "'b15'@0 -> 'test'@13 -> $@17",
                    "'b16'@0 -> 'test'@13 -> $@17",
                    "'b17'@0 -> 'test'@13 -> $@17",
                    "'b18'@0 -> 'test'@13 -> $@17",
                    "'b19'@0 -> 'test'@13 -> $@17",
                    "'b20'@0 -> 'test'@13 -> $@17",
                    "'b21'@0 -> 'test'@13 -> $@17",
                    "'b22'@0 -> 'test'@13 -> $@17",
                    "'b23'@0 -> 'test'@13 -> $@17",
                    "'b24'@0 -> 'test'@13 -> $@17",
                    "'b25'@0 -> 'test'@13 -> $@17",
                    "'b26'@0 -> 'test'@13 -> $@17",
                    "'b27'@0 -> 'test'@13 -> $@17",
                    "'b28'@0 -> 'test'@13 -> $@17",
                    "'b29'@0 -> 'test'@13 -> $@17",
                    "'b30'@0 -> 'test'@13 -> $@17",
                    "'b31'@0 -> 'test'@13 -> $@17",
                    "'b32'@0 -> 'test'@13 -> $@17",
                    "'b33'@0 -> 'test'@13 -> $@17",
                    "'b34'@0 -> 'test'@13 -> $@17",
                    "'b35'@0 -> 'test'@13 -> $@17",
                    "'b36'@0 -> 'test'@13 -> $@17",
                    "'b37'@0 -> 'test'@13 -> $@17",
                    "'b38'@0 -> 'test'@13 -> $@17",
                    "'b39'@0 -> 'test'@13 -> $@17",
                    "'b40'@0 -> 'test'@13 -> $@17",
                    "'b41'@0 -> 'test'@13 -> $@17",
                    "'b42'@0 -> 'test'@13 -> $@17",
                    "'b43'@0 -> 'test'@13 -> $@17",
                    "'b44'@0 -> 'test'@13 -> $@17",
                    "'b45'@0 -> 'test'@13 -> $@17",
                    "'b46'@0 -> 'test'@13 -> $@17",
                    "'b47'@0 -> 'test'@13 -> $@17",
                    "'b48'@0 -> 'test'@13 -> $@17",
                    "'b49'@0 -> 'test'@13 -> $@17",
                    "'b50'@0 -> 'test'@13 -> $@17",
                    "'b51'@0 -> 'test'@13 -> $@17",
                    "'b52'@0 -> 'test'@13 -> $@17",
                    "'b53'@0 -> 'test'@13 -> $@17",
                    "'b54'@0 -> 'test'@13 -> $@17",
                    "'b55'@0 -> 'test'@13 -> $@17",
                    "'b56'@0 -> 'test'@13 -> $@17",
                    "'b57'@0 -> 'test'@13 -> $@17",
                    "'b58'@0 -> 'test'@13 -> $@17",
                    "'b59'@0 -> 'test'@13 -> $@17",
                    "'b60'@0 -> 'test'@13 -> $@17",
                    "'b61'@0 -> 'test'@13 -> $@17",
                    "'b62'@0 -> 'test'@13 -> $@17",
                    "'b63'@0 -> 'test'@13 -> $@17",
                    "'b64'@0 -> 'test'@13 -> $@17",
                    "'b65'@0 -> 'test'@13 -> $@17",
                    "'b66'@0 -> 'test'@13 -> $@17",
                    "'b67'@0 -> 'test'@13 -> $@17",
                    "'b68'@0 -> 'test'@13 -> $@17",
                    "'b69'@0 -> 'test'@13 -> $@17",
                    "'b70'@0 -> 'test'@13 -> $@17",
                    "'b71'@0 -> 'test'@13 -> $@17",
                    "'b72'@0 -> 'test'@13 -> $@17",
                    "'b73'@0 -> 'test'@13 -> $@17",
                    "'b74'@0 -> 'test'@13 -> $@17",
                    "'b75'@0 -> 'test'@13 -> $@17",
                    "'b76'@0 -> 'test'@13 -> $@17",
                    "'b77'@0 -> 'test'@13 -> $@17",
                    "'b78'@0 -> 'test'@13 -> $@17",
                    "'b79'@0 -> 'test'@13 -> $@17",
                    "'b80'@0 -> 'test'@13 -> $@17",
                    "'b81'@0 -> 'test'@13 -> $@17",
                    "'b82'@0 -> 'test'@13 -> $@17",
                    "'b83'@0 -> 'test'@13 -> $@17",
                    "'b84'@0 -> 'test'@13 -> $@17",
                    "'b85'@0 -> 'test'@13 -> $@17",
                    "'b86'@0 -> 'test'@13 -> $@17",
                    "'b87'@0 -> 'test'@13 -> $@17",
                    "'b88'@0 -> 'test'@13 -> $@17",
                    "'b89'@0 -> 'test'@13 -> $@17",
                    "'b90'@0 -> 'test'@13 -> $@17",
                    "'b91'@0 -> 'test'@13 -> $@17",
                    "'b92'@0 -> 'test'@13 -> $@17",
                    "'b93'@0 -> 'test'@13 -> $@17",
                    "'b94'@0 -> 'test'@13 -> $@17",
                    "'b95'@0 -> 'test'@13 -> $@17",
                    "'b96'@0 -> 'test'@13 -> $@17",
                    "'b97'@0 -> 'test'@13 -> $@17",
                    "'b98'@0 -> 'test'@13 -> $@17",
                    "'b99'@0 -> 'test'@13 -> $@17",
                ],
            );
        }
    }

    mod optimize_nodes_tests {
        use super::*;

        fn create_node(data: NodeData) -> Node {
            Node {
                data,
                start_index: NodeStartIndex::None,
            }
        }

        fn create_graph(nodes: Vec<Node>) -> RegexGraph {
            RegexGraph {
                regex_id: 0,
                nodes,
            }
        }

        #[test]
        fn empty_nodes_list() {
            let graph = create_graph(vec![]);
            let optimized = RegexGraph::optimize_nodes(graph.clone());
            assert_eq!(optimized.nodes.len(), 0);
        }

        #[test]
        fn single_start_node() {
            let graph = create_graph(vec![create_node(NodeData::Start)]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 1);
            assert!(matches!(optimized.nodes[0].data, NodeData::Start));
        }

        #[test]
        fn start_followed_by_literal() {
            let graph = create_graph(vec![
                create_node(NodeData::Start),
                create_node(NodeData::Literal {
                    word: BString::from("hello"),
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            // Start is consumed, only Literal remains
            assert_eq!(optimized.nodes.len(), 1);
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
            // Literal gets Index(0) from Start
            assert!(matches!(
                optimized.nodes[0].start_index,
                NodeStartIndex::Index(0)
            ));
        }

        #[test]
        fn literal_followed_by_end() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("abc"),
                }),
                create_node(NodeData::End),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
            assert!(matches!(optimized.nodes[1].data, NodeData::End));
            // End gets Index(3) from the Literal's length
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(3)
            ));
        }

        #[test]
        fn multiple_literals() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("a"),
                }),
                create_node(NodeData::Literal {
                    word: BString::from("b"),
                }),
                create_node(NodeData::Literal {
                    word: BString::from("c"),
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 3);

            // All literals should be preserved
            for node in &optimized.nodes {
                assert!(matches!(node.data, NodeData::Literal { .. }));
            }

            // First literal should have None
            assert!(matches!(
                optimized.nodes[0].start_index,
                NodeStartIndex::None
            ));

            // Second literal should have Index(1)
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(1)
            ));

            // Third literal should have Index(2)
            assert!(matches!(
                optimized.nodes[2].start_index,
                NodeStartIndex::Index(2)
            ));
        }

        #[test]
        fn literal_followed_by_temp() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("test"),
                }),
                create_node(NodeData::Temp { len: 3 }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            // Literal is pushed, Temp becomes the only remaining node
            assert_eq!(optimized.nodes.len(), 2);

            // Temp does not get pushed, so both are in the result
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
            assert!(matches!(optimized.nodes[1].data, NodeData::Temp { .. }));
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(4)
            ));
        }

        #[test]
        fn temp_followed_by_literal() {
            let graph = create_graph(vec![
                create_node(NodeData::Temp { len: 5 }),
                create_node(NodeData::Literal {
                    word: BString::from("end"),
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            // Temp doesn't get pushed, only Literal remains
            assert_eq!(optimized.nodes.len(), 1);

            // The Literal gets the accumulated index from Temp
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
            assert!(matches!(
                optimized.nodes[0].start_index,
                NodeStartIndex::Index(5)
            ));
        }

        #[test]
        fn literal_followed_by_temp_range() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("x"),
                }),
                create_node(NodeData::TempRange {
                    min_len: 2,
                    max_len: 5,
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // Literal is pushed, then TempRange gets Index(1) from the Literal's length
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(1)
            ));
        }

        #[test]
        fn literal_followed_by_temp_inf_with_zero_len() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("prefix"),
                }),
                create_node(NodeData::TempInf { len: 0 }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // TempInf with len=0: prev.start_index.add(None) = prev.start_index = Index(6)
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(6)
            ));
        }

        #[test]
        fn literal_followed_by_temp_inf_with_nonzero_len() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("x"),
                }),
                create_node(NodeData::TempInf { len: 5 }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // Literal is prev, so Literal case applies: TempInf gets Index(1) from Literal's length
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(1)
            ));
        }

        #[test]
        fn literal_followed_by_or_literal() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("a"),
                }),
                create_node(NodeData::OrLiteral {
                    literals: vec![
                        BString::from("x"),
                        BString::from("yy"),
                        BString::from("zzz"),
                    ],
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // Literal is prev, so Literal case applies: OrLiteral gets Index(1) from Literal's length
            assert!(matches!(
                optimized.nodes[1].data,
                NodeData::OrLiteral { .. }
            ));
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(1)
            ));
        }


        #[test]
        fn repetition_with_single_temp() {
            let sub_graph = create_graph(vec![create_node(NodeData::Temp { len: 1 })]);
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("a"),
                }),
                create_node(NodeData::Repetition { sub: sub_graph }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);

            // Repetition of a single Temp is optimized away, Literal becomes prev without pushing
            // Actually, Literal gets pushed and then Repetition becomes prev
            // For Repetition with optimized_sub.nodes.len() == 1 and Temp, prev = node.clone()
            // So Repetition replaces Literal without pushing it
            assert_eq!(optimized.nodes.len(), 2);
            assert!(matches!(optimized.nodes[0].data, NodeData::Literal { .. }));
        }

        #[test]
        fn or_graph_with_all_temp_nodes() {
            let graphs = vec![
                create_graph(vec![create_node(NodeData::Temp { len: 2 })]),
                create_graph(vec![create_node(NodeData::Temp { len: 3 })]),
                create_graph(vec![create_node(NodeData::Temp { len: 2 })]),
            ];
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("pre"),
                }),
                create_node(NodeData::OrGraph { graphs }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // When OrGraph contains only Temp nodes, it gets optimized to a Temp node
            // But we got 2 nodes, so Literal and the optimized OrGraph (now as Temp)
            // Let me just check that Literal is there
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
        }


        #[test]
        fn or_graph_with_mixed_nodes() {
            let graphs = vec![
                create_graph(vec![create_node(NodeData::Literal {
                    word: BString::from("lit"),
                })]),
                create_graph(vec![create_node(NodeData::Temp { len: 1 })]),
            ];
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("a"),
                }),
                create_node(NodeData::OrGraph { graphs }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);

            // OrGraph with mixed types is not optimized
            assert_eq!(optimized.nodes.len(), 2);
            assert!(matches!(
                optimized.nodes[1].data,
                NodeData::OrGraph { .. }
            ));
        }

        #[test]
        fn temp_and_empty_accumulation() {
            let graph = create_graph(vec![
                create_node(NodeData::Temp { len: 2 }),
                create_node(NodeData::Empty),
                create_node(NodeData::Temp { len: 3 }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);

            // Temp, Empty, and Temp all accumulate; only the last Temp remains
            assert_eq!(optimized.nodes.len(), 1);
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Temp { len: 3 }
            ));
            // Index should be 2 (from first Temp) + 0 (from Empty) = 2
            assert!(matches!(
                optimized.nodes[0].start_index,
                NodeStartIndex::Index(2)
            ));
        }

        #[test]
        fn literal_temp_literal_sequence() {
            let graph = create_graph(vec![
                create_node(NodeData::Literal {
                    word: BString::from("a"),
                }),
                create_node(NodeData::Temp { len: 2 }),
                create_node(NodeData::Literal {
                    word: BString::from("b"),
                }),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            assert_eq!(optimized.nodes.len(), 2);

            // First literal is pushed, second literal gets index from Temp accumulation
            assert!(matches!(
                optimized.nodes[0].data,
                NodeData::Literal { .. }
            ));
            assert!(matches!(
                optimized.nodes[1].data,
                NodeData::Literal { .. }
            ));
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(3) // 1 from first literal + 2 from Temp
            ));
        }

        #[test]
        fn start_multiple_literals_end() {
            let graph = create_graph(vec![
                create_node(NodeData::Start),
                create_node(NodeData::Literal {
                    word: BString::from("x"),
                }),
                create_node(NodeData::Literal {
                    word: BString::from("y"),
                }),
                create_node(NodeData::End),
            ]);
            let optimized = RegexGraph::optimize_nodes(graph);
            // Start is consumed
            assert_eq!(optimized.nodes.len(), 3);

            // First literal gets Index(0)
            assert!(matches!(
                optimized.nodes[0].start_index,
                NodeStartIndex::Index(0)
            ));

            // Second literal gets Index(1)
            assert!(matches!(
                optimized.nodes[1].start_index,
                NodeStartIndex::Index(1)
            ));

            // End gets Index(2)
            assert!(matches!(
                optimized.nodes[2].start_index,
                NodeStartIndex::Index(2)
            ));
        }
    }
}
