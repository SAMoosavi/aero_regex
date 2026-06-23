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
//! | `TempRang { min, max }` | Variable-width wildcard (e.g., `.{2,5}`) |
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
use std::fmt;

#[derive(Debug, Clone)]
pub(crate) enum NodeData {
    Start,
    End,
    Literal { word: BString },
    OrLiteral { literals: Vec<BString> },
    OrGraph { graphs: Vec<RegexGraph> },
    Temp { len: usize },
    TempRang { min_len: usize, max_len: usize },
    TempInf { len: usize },
    Empty,
    Repetition { sub: RegexGraph },
}

#[derive(Debug, Clone)]
pub(crate) enum NodeLen {
    None,
    Index(usize),
    Range { min: usize, max: usize },
    AtLeast(usize),
}

impl NodeLen {
    pub(self) fn add(&self, other: NodeLen) -> NodeLen {
        match (self, other) {
            (NodeLen::None, other) => other,
            (_, NodeLen::None) => NodeLen::None,
            (NodeLen::Index(a), NodeLen::Index(b)) => NodeLen::Index(a + b),
            (
                NodeLen::Range {
                    min: a_min,
                    max: a_max,
                },
                NodeLen::Range {
                    min: b_min,
                    max: b_max,
                },
            ) => NodeLen::Range {
                min: a_min + b_min,
                max: a_max + b_max,
            },
            (NodeLen::AtLeast(a), NodeLen::AtLeast(b)) => NodeLen::AtLeast(a + b),
            (NodeLen::Index(a), NodeLen::AtLeast(b)) => NodeLen::AtLeast(a + b),
            (NodeLen::Range { min: a_min, max: _ }, NodeLen::AtLeast(b)) => {
                NodeLen::AtLeast(a_min + b)
            }
            (
                NodeLen::Range {
                    min: a_min,
                    max: a_max,
                },
                NodeLen::Index(b),
            ) => NodeLen::Range {
                min: a_min + b,
                max: a_max + b,
            },
            (
                NodeLen::Index(a),
                NodeLen::Range {
                    min: b_min,
                    max: b_max,
                },
            ) => NodeLen::Range {
                min: a + b_min,
                max: a + b_max,
            },
            (NodeLen::AtLeast(a), NodeLen::Index(b)) => NodeLen::AtLeast(a + b),
            (NodeLen::AtLeast(a), NodeLen::Range { min: b_min, max: _ }) => {
                NodeLen::AtLeast(a + b_min)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Node {
    id: usize,
    pub data: NodeData,
    pub len: NodeLen,
}

#[derive(Debug, Clone)]
pub(crate) struct RegexGraph {
    id: usize,
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
                    .nodes
                    .iter_mut()
                    .enumerate()
                    .for_each(|(index, node)| node.id = index);
                graph
            })
            .collect()
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
        Self {
            id: 0,
            regex_id: 0,
            nodes,
        }
    }

    fn hir_to_nodes(hir: &RegexExpr) -> Vec<Node> {
        let single = |data| {
            vec![Node {
                id: 0,
                data,
                len: NodeLen::None,
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
                                    id: 0,
                                    data: NodeData::Literal { word },
                                    len: NodeLen::None,
                                },
                                Node {
                                    id: 0,
                                    data: NodeData::Repetition {
                                        sub: Self::hir_to_graph(sub),
                                    },
                                    len: NodeLen::None,
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
                        Some(m) => NodeData::TempRang {
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

        let mut prev = nodes.first().unwrap().clone();
        let tail = &nodes[1..];
        let mut nodes = Vec::with_capacity(nodes.len());

        for node in tail {
            match (&prev.data, &node.data) {
                (NodeData::Temp { len: l_len }, NodeData::Temp { len: r_len }) => {
                    prev.data = NodeData::Temp { len: l_len + r_len }
                }
                (NodeData::Temp { len: l_len }, NodeData::TempInf { len: r_len })
                | (NodeData::TempRang { min_len: l_len, .. }, NodeData::TempInf { len: r_len })
                | (NodeData::TempInf { len: l_len }, NodeData::TempRang { min_len: r_len, .. })
                | (NodeData::TempInf { len: l_len }, NodeData::TempInf { len: r_len })
                | (NodeData::TempInf { len: l_len }, NodeData::Temp { len: r_len }) => {
                    prev.data = NodeData::TempInf { len: l_len + r_len }
                }
                (NodeData::Temp { len }, NodeData::TempRang { min_len, max_len })
                | (NodeData::TempRang { min_len, max_len }, NodeData::Temp { len }) => {
                    prev.data = NodeData::TempRang {
                        min_len: min_len + len,
                        max_len: max_len + len,
                    }
                }
                (
                    NodeData::TempRang {
                        min_len: l_min_len,
                        max_len: l_max_len,
                    },
                    NodeData::TempRang {
                        min_len: r_min_len,
                        max_len: r_max_len,
                    },
                ) => {
                    prev.data = NodeData::TempRang {
                        min_len: l_min_len + r_min_len,
                        max_len: l_max_len + r_max_len,
                    }
                }
                (_, _) => {
                    nodes.push(prev);
                    prev = node.clone();
                }
            }
        }

        nodes.push(prev);

        nodes.iter_mut().for_each(|node| {
            node.len = match &node.data {
                NodeData::End | NodeData::Start => NodeLen::Index(0),
                NodeData::Literal { word } => NodeLen::Index(word.len()),
                NodeData::Temp { len } => NodeLen::Index(*len),
                NodeData::TempRang { min_len, max_len } => NodeLen::Range {
                    min: *min_len,
                    max: *max_len,
                },
                NodeData::TempInf { len: 0 } => NodeLen::None,
                NodeData::TempInf { len } => NodeLen::AtLeast(*len),
                NodeData::Empty => NodeLen::Index(0),
                NodeData::OrLiteral { literals } => NodeLen::Range {
                    min: literals.iter().map(|word| word.len()).min().unwrap(),
                    max: literals.iter().map(|word| word.len()).max().unwrap(),
                },
                NodeData::Repetition { .. } => NodeLen::None,
                NodeData::OrGraph { graphs: _ } => todo!(),
            }
        });

        Self {
            id: 0,
            regex_id: 0,
            nodes,
        }
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
                _ => {}
            }
        }
        result
    }

    pub(crate) fn nodes(&self, start_node_id: usize, end_node_id: usize) -> &[Node] {
        &self.nodes[start_node_id..=end_node_id]
    }
}

impl fmt::Display for RegexGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
        write!(f, "{}@{}", self.data, self.len)
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
            NodeData::TempRang { min_len, max_len } => format!(".{{{min_len},{max_len}}}"),
            NodeData::TempInf { len } => format!(".{{{len}, }}"),
            NodeData::Empty => "ε".to_string(),
            NodeData::Repetition { sub } => format!("REP([{sub}] )"),
        };

        write!(f, "{data_str}")
    }
}

impl fmt::Display for NodeLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let idx_str = match &self {
            NodeLen::None => "?".to_string(),
            NodeLen::Index(i) => i.to_string(),
            NodeLen::Range { min, max } => format!("{min}..{max}"),
            NodeLen::AtLeast(i) => format!("{i}.."),
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
            check_graphs("abc", &["'abc'@3"]);
        }

        #[test]
        fn start_anchor() {
            check_graphs("^abc", &["^@0 -> 'abc'@3"]);
        }

        #[test]
        fn end_anchor() {
            check_graphs("abc$", &["'abc'@3 -> $@0"]);
        }

        #[test]
        fn both_anchors() {
            check_graphs("^abc$", &["^@0 -> 'abc'@3 -> $@0"]);
        }

        #[test]
        fn temp_skip() {
            check_graphs("^a.b$", &["^@0 -> 'a'@1 -> .{1}@1 -> 'b'@1 -> $@0"]);
        }

        #[test]
        fn temp_range() {
            check_graphs(
                "^a.{2,4}b$",
                &[
                    "^@0 -> 'a'@1 -> .{2}@2 -> 'b'@1 -> $@0",
                    "^@0 -> 'a'@1 -> .{3}@3 -> 'b'@1 -> $@0",
                    "^@0 -> 'a'@1 -> .{4}@4 -> 'b'@1 -> $@0",
                ],
            );
        }

        #[test]
        fn unbounded_temp() {
            check_graphs(
                "^a.*b$",
                &["^@0 -> 'a'@1 -> REP([.{1}@?] )@? -> 'b'@1 -> $@0"],
            );
        }

        #[test]
        fn alternation_split() {
            check_graphs(
                "^(a|b)c$",
                &["^@0 -> 'ac'@2 -> $@0", "^@0 -> 'bc'@2 -> $@0"],
            );
        }

        #[test]
        fn or_literal() {
            check_graphs("a|b", &["'a'@1", "'b'@1"]);
        }

        #[test]
        fn repetition_with_or() {
            check_graphs("^(a|b)*c$", &["^@0 -> .{0, }@? -> 'c'@1 -> $@0"]);
        }
    }

    mod words {
        use super::*;

        fn check_words(input: &str, expected: &[(&str, usize)]) {
            let input_hir = parse(input);
            let graphs = RegexGraph::new(&input_hir, 0);
            let actual: Vec<(String, usize)> = graphs
                .iter()
                .flat_map(|g| g.words())
                .map(|(w, id)| (String::from_utf8_lossy(&w).to_string(), id))
                .collect();
            let expected_owned: Vec<(String, usize)> = expected
                .iter()
                .map(|(s, id)| (s.to_string(), *id))
                .collect();
            assert_eq!(actual, expected_owned);
        }

        #[test]
        fn simple_literal() {
            check_words("abc", &[("abc", 0)]);
        }

        #[test]
        fn alternation_literals() {
            check_words("a|b", &[("a", 0), ("b", 0)]);
        }

        #[test]
        fn literal_with_start_anchor() {
            check_words("^abc", &[("abc", 1)]);
        }

        #[test]
        fn literal_with_end_anchor() {
            check_words("abc$", &[("abc", 0)]);
        }

        #[test]
        fn concat_literals() {
            check_words("^ac$", &[("ac", 1)]);
        }

        #[test]
        fn alternation_in_concat() {
            check_words("^(a|b)c$", &[("ac", 1), ("bc", 1)]);
        }

        #[test]
        fn repetition_exact_literal() {
            check_words("^(?:a){2}$", &[("aa", 1)]);
        }

        #[test]
        fn repetition_range_literal() {
            check_words("^(?:a){1,3}$", &[("a", 1), ("aa", 1), ("aaa", 1)]);
        }

        #[test]
        fn no_literals_only_temp_nodes() {
            check_words("^.{2}$", &[]);
        }

        #[test]
        fn empty_graph() {
            check_words("", &[]);
        }

        #[test]
        fn or_literal_in_concat() {
            check_words("^(?:a|b)cd", &[("acd", 1), ("bcd", 1)]);
        }
    }
}
