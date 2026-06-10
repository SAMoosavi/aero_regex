use crate::utilities::{RegexIndex, class_to_list_of_literal, extract_class_to_ranges};
use bstr::{BString, ByteVec};
use itertools::Itertools;
use regex_syntax::hir::{Hir, HirKind, Look};

#[derive(Debug)]
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

#[derive(Debug)]
pub(crate) struct Node {
    data: NodeData,
}

#[derive(Debug)]
pub(crate) struct RegexGraph {
    id: usize,
    regex_index: RegexIndex,
    nodes: Vec<Node>,
}

impl RegexGraph {
    pub(crate) fn new(hir: &Hir, regex_index: RegexIndex) -> Vec<Self> {
        let hirs = Self::split(hir);
        hirs.iter().map(Self::hir_to_graph).collect()
    }

    fn split(hir: &Hir) -> Vec<Hir> {
        match hir.kind() {
            HirKind::Alternation(hirs) => hirs.iter().flat_map(Self::split).collect(),
            HirKind::Concat(hirs) => {
                if hirs.len() == 2
                    && let HirKind::Look(Look::Start) = hirs[0].kind()
                {
                    match hirs[1].kind() {
                        HirKind::Class(class) => {
                            let ranges = extract_class_to_ranges(class);

                            if ranges.iter().any(|(_, max)| max > &255) {
                                return vec![hir.clone()];
                            }

                            ranges
                                .iter()
                                .flat_map(|&(min, max)| {
                                    (min..=max)
                                        .map(|x| {
                                            Hir::concat(vec![
                                                Hir::look(Look::Start),
                                                Hir::literal(vec![x as u8]),
                                            ])
                                        })
                                        .collect_vec()
                                })
                                .collect_vec()
                        }
                        HirKind::Alternation(alter_hirs) => alter_hirs
                            .iter()
                            .map(|hir| Hir::concat(vec![Hir::look(Look::Start), hir.clone()]))
                            .collect(),
                        _ => vec![hir.clone()],
                    }
                } else {
                    vec![hir.clone()]
                }
            }
            HirKind::Class(class) => class_to_list_of_literal(class),
            _ => vec![hir.clone()],
        }
    }

    fn hir_to_graph(hir: &Hir) -> Self {
        let nodes = Self::hir_to_nodes(hir);
        Self {
            id: 0,
            regex_index: 0,
            nodes,
        }
    }

    fn hir_to_nodes(hir: &Hir) -> Vec<Node> {
        // Helper closure to eliminate boilerplate when returning a single Node
        let single = |data| vec![Node { data }];

        match hir.kind() {
            HirKind::Empty => single(NodeData::Empty),
            HirKind::Literal(lit) => single(NodeData::Literal {
                word: BString::from(lit.0.to_vec()),
            }),
            HirKind::Class(_) => single(NodeData::Temp { len: 1 }),
            HirKind::Look(Look::Start) => single(NodeData::Start),
            HirKind::Look(Look::End) => single(NodeData::End),
            HirKind::Look(_) => single(NodeData::Temp { len: 0 }),
            HirKind::Repetition(rep) => {
                let min = rep.min as usize;
                let max = rep.max.map(|m| m as usize);

                // ZERO-COST BORROW: We borrow `sub` instead of cloning the entire AST branch
                let sub = &rep.sub;

                match sub.kind() {
                    HirKind::Empty => single(NodeData::Empty),
                    HirKind::Literal(lit) => {
                        let base = lit.0.to_vec();
                        let mut word = BString::from(base.repeat(min));

                        match max {
                            None => vec![
                                Node {
                                    data: NodeData::Literal { word },
                                },
                                Node {
                                    data: NodeData::Repetition {
                                        sub: Self::hir_to_graph(sub),
                                    },
                                },
                            ],
                            Some(m) if m == min => single(NodeData::Literal { word }),
                            Some(m) => {
                                let mut literals = Vec::with_capacity(m - min + 1);
                                literals.push(word.clone());

                                for _ in 0..(m - min) {
                                    word.extend_from_slice(&base);
                                    literals.push(word.clone());
                                }
                                single(NodeData::OrLiteral { literals })
                            }
                        }
                    }

                    HirKind::Class(_) => single(match max {
                        None => NodeData::TempInf { len: min },
                        Some(m) if m == min => NodeData::Temp { len: min },
                        Some(m) => NodeData::TempRang {
                            min_len: min,
                            max_len: m,
                        },
                    }),

                    HirKind::Capture(cap) => single(NodeData::Repetition {
                        sub: Self::hir_to_graph(&cap.sub),
                    }),

                    // Merged fallback for Repetition, Look, Alternation, Concat
                    _ => single(NodeData::Repetition {
                        sub: Self::hir_to_graph(sub),
                    }),
                }
            }

            HirKind::Capture(cap) => Self::hir_to_nodes(&cap.sub), // Removed clone!
            HirKind::Concat(concat) => concat.iter().flat_map(Self::hir_to_nodes).collect(),
            HirKind::Alternation(alternation) => {
                // OPTIMIZATION: `collect()` into an Option automatically short-circuits.
                // If it hits a non-literal, it immediately stops and returns `None`,
                // avoiding the double-iteration in your original `all()` + `map()` code.
                let all_literals: Option<Vec<BString>> = alternation
                    .iter()
                    .map(|alter| {
                        if let HirKind::Literal(literal) = alter.kind() {
                            Some(BString::from(literal.0.to_vec()))
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
        }
    }

    // fn optimaize_graph(&mut self) -> Self {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex_syntax::Parser;

    use crate::normalizer;

    fn parse(pattern: &str) -> Hir {
        normalizer::normalize(&Parser::new().parse(pattern).unwrap())
    }

    mod split {
        use super::*;

        fn check_split(input: &str, expected: &[&str]) {
            let input_hir = parse(input);
            let expected_hirs: Vec<_> = expected.iter().map(|&s| parse(s)).collect();

            assert_eq!(RegexGraph::split(&input_hir), expected_hirs);
        }

        #[test]
        fn test_no_alternation() {
            check_split(r"abc", &[r"abc"]);
            check_split(r"^hello$", &[r"^hello$"]);
        }

        #[test]
        fn test_simple_alternation() {
            check_split(r"a|b", &[r"a", r"b"]);
            check_split(r"aa|bb|cc", &[r"aa", r"bb", r"cc"]);
        }

        #[test]
        fn test_anchors_and_alternations() {
            check_split(r"^(a|b)", &[r"^a", r"^b"]);
            check_split(r"(a|b)$", &[r"a$", r"b$"]);
            check_split(r"^(a|b)$", &[r"^a$", r"^b$"]);
            check_split(
                r"^prefix(a|b)suffix$",
                &[r"^prefixasuffix$", r"^prefixbsuffix$"],
            );
        }

        #[test]
        fn test_cartesian_product_multiple_groups() {
            check_split(r"(?:a|b)(?:c|d)", &[r"ac", r"ad", r"bc", r"bd"]);
        }

        #[test]
        fn test_nested_alternations() {
            check_split(r"(?:a|(?:b|c))", &[r"a", r"b", r"c"]);
            check_split(r"^(?:a|(?:b|c))$", &[r"^a$", r"^b$", r"^c$"]);
        }

        #[test]
        fn test_capture_groups_are_preserved() {
            check_split(r"(a|b)", &[r"a", r"b"]);

            check_split(r"^(a|b)(c|d)", &[r"^ac", r"^ad", r"^bc", r"^bd"]);
        }

        #[test]
        fn test_repetitions_are_not_split() {
            check_split(r"(?:a|b)*", &[r"(?:a|b)*"]);
            check_split(r"^(a|b)+$", &[r"^a(a|b)*$", r"^b(a|b)*$"]);
        }

        #[test]
        fn test_complex_real_world_scenario() {
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

        #[test]
        fn a() {
            let input_hir = parse(r"^(?:a|b)[0-9]{2}.{10}test([0-9]){2,}test$");
            let a = RegexGraph::new(&input_hir, 0);
            println!("{a:#?}");
        }
    }
}
