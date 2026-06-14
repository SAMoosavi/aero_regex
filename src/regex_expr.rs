use bstr::BString;
use itertools::Itertools;
use regex_syntax::hir::{Class, Hir, HirKind, Look};

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
