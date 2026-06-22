//! # RegexExpr — Simplified Regex AST
//!
//! Provides a simplified representation of regular expressions, built on top
//! of `regex_syntax`'s HIR (High-level Intermediate Representation). The key
//! simplification: capture groups are erased, look-arounds are reduced to
//! `Start`/`End`/`Empty`, and character classes with >20 codepoints collapse
//! to `Dot`.
//!
//! ## Variants
//!
//! | Variant | Meaning |
//! |---------|---------|
//! | `Empty` | ε (empty string) |
//! | `Dot` | Any single byte |
//! | `Start` | `^` anchor |
//! | `End` | `$` anchor |
//! | `Literal(BString)` | Exact byte sequence |
//! | `Class(Vec<(u32,u32)>)` | Character class as codepoint ranges |
//! | `Repetition { min, max, sub }` | `sub{min,max}` |
//! | `Concat(Vec<RegexExpr>)` | Sequence of expressions |
//! | `Alternation(Vec<RegexExpr>)` | `expr1|expr2|...` |
//!
//! ## Construction helpers
//!
//! - `RegexExpr::concat(&[...])` — builds a Concat, merging adjacent literals
//!   and flattening nested Concats.
//! - `RegexExpr::alternation(&[...])` — builds an Alternation, flattening nested
//!   ones and deduplicating.

use bstr::BString;
use itertools::Itertools;
use regex_syntax::{
    Parser,
    hir::{Class, Hir, HirKind, Look},
};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum RegexExpr {
    Empty,
    Dot,
    Start,
    End,
    Literal(BString),
    Class(Vec<(u32, u32)>),
    Repetition {
        min: u32,
        max: Option<u32>,
        sub: Box<Self>,
    },
    Concat(Vec<RegexExpr>),
    Alternation(Vec<RegexExpr>),
}

impl From<&Hir> for RegexExpr {
    fn from(value: &Hir) -> Self {
        (value.kind()).into()
    }
}

impl From<Hir> for RegexExpr {
    fn from(value: Hir) -> Self {
        (value.kind()).into()
    }
}

impl From<HirKind> for RegexExpr {
    fn from(value: HirKind) -> Self {
        (&value).into()
    }
}

impl From<&HirKind> for RegexExpr {
    fn from(value: &HirKind) -> Self {
        match value {
            HirKind::Empty => Self::Empty,
            HirKind::Literal(literal) => Self::Literal(BString::from(literal.0.to_vec())),
            HirKind::Class(class) => {
                let ranges: Vec<(u32, u32)> = match class {
                    Class::Unicode(u) => u
                        .iter()
                        .map(|r| (r.start() as u32, r.end() as u32))
                        .collect(),
                    Class::Bytes(b) => b
                        .iter()
                        .map(|r| (r.start() as u32, r.end() as u32))
                        .collect(),
                };
                if ranges.iter().any(|(_, max)| max > &255)
                    || ranges.iter().map(|(min, max)| max - min).sum::<u32>() >= 20
                {
                    Self::Dot
                } else {
                    Self::Class(ranges)
                }
            }
            HirKind::Look(Look::Start) => Self::Start,
            HirKind::Look(Look::End) => Self::End,
            HirKind::Look(_) => Self::Empty,
            HirKind::Repetition(repetition) => Self::Repetition {
                min: repetition.min,
                max: repetition.max,
                sub: Box::new(repetition.sub.kind().into()),
            },
            HirKind::Capture(capture) => capture.sub.kind().into(),
            HirKind::Concat(hirs) => {
                Self::Concat(hirs.iter().map(|hir| hir.kind().into()).collect())
            }
            HirKind::Alternation(hirs) => {
                Self::Alternation(hirs.iter().map(|hir| hir.kind().into()).collect())
            }
        }
    }
}

impl RegexExpr {
    pub(crate) fn parse(regex: &str) -> Self {
        Parser::new().parse(regex).unwrap().into()
    }

    pub(crate) fn concat(elements: &[Self]) -> Self {
        let mut result = Vec::new();

        for element in elements {
            match element {
                Self::Empty => {
                    // εx = x
                }
                Self::Concat(inner) => {
                    for item in inner {
                        Self::push_concat_item(&mut result, item.clone());
                    }
                }
                other => {
                    Self::push_concat_item(&mut result, other.clone());
                }
            }
        }

        match result.len() {
            0 => Self::Empty,
            1 => result.pop().unwrap(),
            _ => Self::Concat(result),
        }
    }

    fn push_concat_item(result: &mut Vec<RegexExpr>, item: RegexExpr) {
        match item {
            RegexExpr::Literal(mut current) => {
                if let Some(RegexExpr::Literal(prev)) = result.last_mut() {
                    prev.extend_from_slice(&current);
                } else {
                    result.push(RegexExpr::Literal(std::mem::take(&mut current)));
                }
            }

            other => result.push(other),
        }
    }

    pub(crate) fn alternation(elements: &[Self]) -> Self {
        let mut elements = elements
            .iter()
            .flat_map(|regex_structure| {
                if let RegexExpr::Alternation(ele) = regex_structure {
                    ele.clone()
                } else {
                    vec![regex_structure.clone()]
                }
            })
            .unique()
            .collect_vec();

        match elements.len() {
            0 => Self::Empty,
            1 => elements.pop().unwrap(),
            _ => Self::Alternation(elements),
        }
    }
}

fn write_literal_byte(f: &mut fmt::Formatter<'_>, b: u8) -> fmt::Result {
    match b {
        b'.' | b'^' | b'$' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'{' | b'}'
        | b'|' | b'\\' => write!(f, "\\{}", b as char),
        b if b.is_ascii_graphic() || b == b' ' => write!(f, "{}", b as char),
        _ => write!(f, "\\x{:02x}", b),
    }
}

impl fmt::Display for RegexExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegexExpr::Empty => Ok(()),
            RegexExpr::Dot => write!(f, "."),
            RegexExpr::Start => write!(f, "^"),
            RegexExpr::End => write!(f, "$"),
            RegexExpr::Literal(bytes) => {
                for &b in bytes.iter() {
                    write_literal_byte(f, b)?;
                }
                Ok(())
            }
            RegexExpr::Class(ranges) => {
                write!(f, "[")?;
                for &(lo, hi) in ranges {
                    if lo == hi {
                        let c = char::from_u32(lo).unwrap_or('?');
                        if c == ']' || c == '\\' || c == '^' {
                            write!(f, "\\{}", c)?;
                        } else {
                            write!(f, "{}", c)?;
                        }
                    } else {
                        let lo_c = char::from_u32(lo).unwrap_or('?');
                        let hi_c = char::from_u32(hi).unwrap_or('?');
                        write!(f, "{}-{}", lo_c, hi_c)?;
                    }
                }
                write!(f, "]")
            }
            RegexExpr::Repetition { min, max, sub } => {
                let needs_group = matches!(**sub, RegexExpr::Alternation(_) | RegexExpr::Concat(_));
                if needs_group {
                    write!(f, "(?:")?;
                }
                write!(f, "{}", sub)?;
                if needs_group {
                    write!(f, ")")?;
                }
                match (min, max) {
                    (0, Some(1)) => write!(f, "?"),
                    (0, None) => write!(f, "*"),
                    (1, None) => write!(f, "+"),
                    (m, Some(n)) if m == n => write!(f, "{{{}}}", m),
                    (m, Some(n)) => write!(f, "{{{},{}}}", m, n),
                    (m, None) => write!(f, "{{{},}}", m),
                }
            }
            RegexExpr::Concat(parts) => {
                for part in parts {
                    write!(f, "{}", part)?;
                }
                Ok(())
            }
            RegexExpr::Alternation(parts) => {
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, "|")?;
                    }
                    write!(f, "{}", part)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex_syntax::Parser;

    fn parse(pattern: &str) -> RegexExpr {
        Parser::new().parse(pattern).unwrap().into()
    }

    mod from_hir {
        use super::*;

        #[test]
        fn empty_pattern() {
            assert_eq!(parse(""), RegexExpr::Empty);
        }

        #[test]
        fn literal() {
            assert_eq!(
                parse("abc"),
                RegexExpr::Literal(BString::from(b"abc".as_slice()))
            );
        }

        #[test]
        fn dot() {
            assert_eq!(parse("."), RegexExpr::Dot);
        }

        #[test]
        fn start_anchor() {
            assert_eq!(parse("^"), RegexExpr::Start);
        }

        #[test]
        fn end_anchor() {
            assert_eq!(parse("$"), RegexExpr::End);
        }

        #[test]
        fn look_around_word_boundary() {
            assert_eq!(parse("\\b"), RegexExpr::Empty);
        }

        #[test]
        fn small_class() {
            assert_eq!(parse("[a-c]"), RegexExpr::Class(vec![(97, 99)]));
        }

        #[test]
        fn class_collapses_to_dot_large_span() {
            assert_eq!(parse("[a-z]"), RegexExpr::Dot);
        }

        #[test]
        fn class_collapses_to_dot_high_char() {
            assert_eq!(parse("[\\x{100}-\\x{200}]"), RegexExpr::Dot);
        }

        #[test]
        fn repetition_bounded() {
            assert_eq!(
                parse("a{2,5}"),
                RegexExpr::Repetition {
                    min: 2,
                    max: Some(5),
                    sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                }
            );
        }

        #[test]
        fn repetition_plus() {
            assert_eq!(
                parse("a+"),
                RegexExpr::Repetition {
                    min: 1,
                    max: None,
                    sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                }
            );
        }

        #[test]
        fn capture_group_erased() {
            assert_eq!(
                parse("(abc)"),
                RegexExpr::Literal(BString::from(b"abc".as_slice()))
            );
        }

        #[test]
        fn alternation() {
            assert_eq!(
                parse("ab|cd"),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"ab".as_slice())),
                    RegexExpr::Literal(BString::from(b"cd".as_slice())),
                ])
            );
        }

        #[test]
        fn repetition_star() {
            assert_eq!(
                parse("a*"),
                RegexExpr::Repetition {
                    min: 0,
                    max: None,
                    sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                }
            );
        }

        #[test]
        fn repetition_question() {
            assert_eq!(
                parse("a?"),
                RegexExpr::Repetition {
                    min: 0,
                    max: Some(1),
                    sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                }
            );
        }

        #[test]
        fn nested_repetition() {
            assert_eq!(
                parse("(a{2,3})+"),
                RegexExpr::Repetition {
                    min: 1,
                    max: None,
                    sub: Box::new(RegexExpr::Repetition {
                        min: 2,
                        max: Some(3),
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }),
                }
            );
        }

        #[test]
        fn nested_capture() {
            assert_eq!(
                parse("((abc))"),
                RegexExpr::Literal(BString::from(b"abc".as_slice()))
            );
        }

        #[test]
        fn multi_alternation() {
            assert_eq!(
                parse("(?:a|b)|c"),
                RegexExpr::Alternation(vec![
                    RegexExpr::Class(vec![(97, 98)]),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                ])
            );
        }

        #[test]
        fn alternation_with_literal_and_class() {
            assert_eq!(
                parse("a|[0-1]"),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Class(vec![(48, 49)]),
                ])
            );
        }

        #[test]
        fn concat_with_anchors() {
            assert_eq!(
                parse("^abc$"),
                RegexExpr::Concat(vec![
                    RegexExpr::Start,
                    RegexExpr::Literal(BString::from(b"abc".as_slice())),
                    RegexExpr::End,
                ])
            );
        }

        #[test]
        fn class_single_char_simplifies_to_literal() {
            assert_eq!(
                parse("[x]"),
                RegexExpr::Literal(BString::from(b"x".as_slice()))
            );
        }

        #[test]
        fn class_byte_range() {
            assert_eq!(parse("[\\x00-\\x03]"), RegexExpr::Class(vec![(0, 3)]));
        }

        #[test]
        fn repetition_bounded_unbounded() {
            assert_eq!(
                parse("a{3,}"),
                RegexExpr::Repetition {
                    min: 3,
                    max: None,
                    sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                }
            );
        }
    }

    mod concat {
        use super::*;

        #[test]
        fn empty_input() {
            assert_eq!(RegexExpr::concat(&[]), RegexExpr::Empty);
        }

        #[test]
        fn single_element() {
            assert_eq!(
                RegexExpr::concat(&[RegexExpr::Literal(BString::from(b"a".as_slice()))]),
                RegexExpr::Literal(BString::from(b"a".as_slice())),
            );
        }

        #[test]
        fn merge_adjacent_literals() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
                RegexExpr::Literal(BString::from(b"ab".as_slice())),
            );
        }

        #[test]
        fn flatten_nested_concat() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Concat(vec![RegexExpr::Literal(BString::from(b"a".as_slice())),]),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
                RegexExpr::Literal(BString::from(b"ab".as_slice())),
            );
        }

        #[test]
        fn empty_absorbed() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Empty,
                    RegexExpr::Literal(BString::from(b"a".as_slice()))
                ]),
                RegexExpr::Literal(BString::from(b"a".as_slice())),
            );
        }

        #[test]
        fn all_empty() {
            assert_eq!(
                RegexExpr::concat(&[RegexExpr::Empty, RegexExpr::Empty]),
                RegexExpr::Empty,
            );
        }

        #[test]
        fn mixed_types() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
                RegexExpr::Concat(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
            );
        }

        #[test]
        fn adjacent_literals_merge_through_non_literal() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                ]),
                RegexExpr::Concat(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                    RegexExpr::Literal(BString::from(b"bc".as_slice())),
                ]),
            );
        }

        #[test]
        fn three_plus_empties() {
            assert_eq!(
                RegexExpr::concat(&[RegexExpr::Empty, RegexExpr::Empty, RegexExpr::Empty]),
                RegexExpr::Empty,
            );
        }

        #[test]
        fn three_plus_literals_merge() {
            assert_eq!(
                RegexExpr::concat(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                ]),
                RegexExpr::Literal(BString::from(b"abc".as_slice())),
            );
        }

        #[test]
        fn deeply_nested_concat_flattened() {
            assert_eq!(
                RegexExpr::concat(&[RegexExpr::Concat(vec![RegexExpr::Concat(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                ]),])]),
                RegexExpr::Concat(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Dot,
                ]),
            );
        }
    }

    mod alternation {
        use super::*;

        #[test]
        fn empty_input() {
            assert_eq!(RegexExpr::alternation(&[]), RegexExpr::Empty);
        }

        #[test]
        fn single_element() {
            assert_eq!(
                RegexExpr::alternation(&[RegexExpr::Literal(BString::from(b"a".as_slice()))]),
                RegexExpr::Literal(BString::from(b"a".as_slice())),
            );
        }

        #[test]
        fn two_distinct() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
            );
        }

        #[test]
        fn flatten_nested() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Literal(BString::from(b"b".as_slice())),
                    ]),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                ]),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                ]),
            );
        }

        #[test]
        fn dedup() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                ]),
                RegexExpr::Literal(BString::from(b"a".as_slice())),
            );
        }

        #[test]
        fn dedup_after_flatten() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                    ]),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                ]),
            );
        }

        #[test]
        fn all_identical() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                ]),
                RegexExpr::Literal(BString::from(b"a".as_slice())),
            );
        }

        #[test]
        fn deeply_nested_flatten() {
            assert_eq!(
                RegexExpr::alternation(&[
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Literal(BString::from(b"b".as_slice())),
                    ]),
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"c".as_slice())),
                        RegexExpr::Literal(BString::from(b"d".as_slice())),
                    ]),
                ]),
                RegexExpr::Alternation(vec![
                    RegexExpr::Literal(BString::from(b"a".as_slice())),
                    RegexExpr::Literal(BString::from(b"b".as_slice())),
                    RegexExpr::Literal(BString::from(b"c".as_slice())),
                    RegexExpr::Literal(BString::from(b"d".as_slice())),
                ]),
            );
        }
    }

    mod display {
        use super::*;

        #[test]
        fn empty() {
            assert_eq!(format!("{}", RegexExpr::Empty), "");
        }

        #[test]
        fn dot() {
            assert_eq!(format!("{}", RegexExpr::Dot), ".");
        }

        #[test]
        fn start() {
            assert_eq!(format!("{}", RegexExpr::Start), "^");
        }

        #[test]
        fn end() {
            assert_eq!(format!("{}", RegexExpr::End), "$");
        }

        #[test]
        fn literal() {
            assert_eq!(
                format!("{}", RegexExpr::Literal(BString::from(b"abc".as_slice()))),
                "abc"
            );
        }

        #[test]
        fn literal_special_chars() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Literal(BString::from(b"a.b*c+d".as_slice()))
                ),
                "a\\.b\\*c\\+d"
            );
        }

        #[test]
        fn class_single_char() {
            assert_eq!(format!("{}", RegexExpr::Class(vec![(120, 120)])), "[x]");
        }

        #[test]
        fn class_range() {
            assert_eq!(format!("{}", RegexExpr::Class(vec![(97, 99)])), "[a-c]");
        }

        #[test]
        fn class_multiple_ranges() {
            assert_eq!(
                format!("{}", RegexExpr::Class(vec![(48, 57), (65, 70)])),
                "[0-9A-F]"
            );
        }

        #[test]
        fn repetition_star() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 0,
                        max: None,
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a*"
            );
        }

        #[test]
        fn repetition_plus() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 1,
                        max: None,
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a+"
            );
        }

        #[test]
        fn repetition_question() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 0,
                        max: Some(1),
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a?"
            );
        }

        #[test]
        fn repetition_bounded() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 2,
                        max: Some(5),
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a{2,5}"
            );
        }

        #[test]
        fn repetition_exact() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 3,
                        max: Some(3),
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a{3}"
            );
        }

        #[test]
        fn repetition_unbounded() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 2,
                        max: None,
                        sub: Box::new(RegexExpr::Literal(BString::from(b"a".as_slice()))),
                    }
                ),
                "a{2,}"
            );
        }

        #[test]
        fn repetition_alternation_wrapped() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Repetition {
                        min: 0,
                        max: None,
                        sub: Box::new(RegexExpr::Alternation(vec![
                            RegexExpr::Literal(BString::from(b"a".as_slice())),
                            RegexExpr::Literal(BString::from(b"b".as_slice())),
                        ])),
                    }
                ),
                "(?:a|b)*"
            );
        }

        #[test]
        fn concat() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Concat(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Dot,
                        RegexExpr::Literal(BString::from(b"b".as_slice())),
                    ])
                ),
                "a.b"
            );
        }

        #[test]
        fn alternation() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Literal(BString::from(b"b".as_slice())),
                    ])
                ),
                "a|b"
            );
        }

        #[test]
        fn alternation_three() {
            assert_eq!(
                format!(
                    "{}",
                    RegexExpr::Alternation(vec![
                        RegexExpr::Literal(BString::from(b"a".as_slice())),
                        RegexExpr::Literal(BString::from(b"b".as_slice())),
                        RegexExpr::Literal(BString::from(b"c".as_slice())),
                    ])
                ),
                "a|b|c"
            );
        }
    }
}
