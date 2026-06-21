mod normalizer;
mod regex_expr;
mod regex_graph;
mod utilities;
mod aero_regex;

use normalizer::normalize;
use regex_syntax::parse;

use crate::regex_graph::RegexGraph;

pub fn bench_normalize(index: usize, input: &str) {
    let hir = parse(input).unwrap();

    RegexGraph::new(&normalize(&hir.into()), index);
}
