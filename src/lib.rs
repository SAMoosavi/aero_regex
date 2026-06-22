//! # AeroRegex — Accelerated Regex Engine
//!
//! A high-performance regex engine that compiles regex patterns into
//! [Aho-Corasick](https://en.wikipedia.org/wiki/Aho%E2%80%93Corasick_algorithm)
//! automata for fast multi-pattern matching, combined with a graph-based
//! representation for constraint propagation.
//!
//! ## Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    Input: regex pattern                  │
//! └─────────────────────────┬───────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  regex_expr.rs — Parse regex → RegexExpr AST            │
//! │  (wraps regex_syntax::Hir into a simplified enum)       │
//! └─────────────────────────┬───────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  normalizer.rs — Flatten repetitions, expand classes,   │
//! │  merge concat/alternation, distribute anchors           │
//! └─────────────────────────┬───────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  regex_graph.rs — Compile RegexExpr → node graph        │
//! │  (Literal, Temp*, OrLiteral, OrGraph, Repetition nodes) │
//! └─────────────────────────┬───────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  aero_regex.rs — Build Aho-Corasick DFA from literal   │
//! │  words in graphs; stream bytes through DFA for matching │
//! └─────────────────────────┬───────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  utilities.rs — Shared types (RegexId) and helpers      │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Pipeline
//!
//! 1. **Parse** — `RegexExpr::parse` converts a regex string to a `RegexExpr` AST
//!    using `regex_syntax`.
//! 2. **Normalize** — `normalizer::normalize` simplifies the AST: collapses
//!    bounded repetitions into alternations of literals, distributes concatenation
//!    over alternations (cartesian product), and flattens nested structures.
//! 3. **Graph** — `RegexGraph::new` splits alternations into independent sub-graphs,
//!    converts each to a linear node chain, and optimizes adjacent compatible nodes.
//! 4. **Match** — `AeroRegex` collects all literal words from graphs, builds an
//!    Aho-Corasick DFA, and feeds input bytes through it. Node-graph constraints
//!    validate positional matches.

mod aero_regex;
mod normalizer;
mod regex_expr;
mod regex_graph;
mod utilities;

use normalizer::normalize;
use regex_syntax::parse;

use crate::regex_graph::RegexGraph;

pub fn bench_normalize(index: usize, input: &str) {
    let hir = parse(input).unwrap();

    RegexGraph::new(&normalize(&hir.into()), index);
}
