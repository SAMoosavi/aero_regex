use itertools::Itertools;

use crate::regex_expr::RegexExpr;

pub(crate) type RegexIndex = usize;

pub(crate) fn class_to_list_of_literal(ranges: &[(u32, u32)]) -> Vec<RegexExpr> {
    if ranges.iter().any(|(_, max)| max > &255) {
        return vec![RegexExpr::Class(ranges.to_vec())];
    }

    ranges
        .iter()
        .flat_map(|&(min, max)| {
            (min..=max)
                .map(|x| RegexExpr::Literal(vec![x as u8].into()))
                .collect_vec()
        })
        .collect_vec()
}
