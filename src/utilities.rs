use itertools::Itertools;
use regex_syntax::hir::{Class, Hir};

pub(crate) type RegexIndex = usize;

pub(crate) fn extract_class_to_ranges(class: &Class) -> Vec<(u32, u32)> {
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
    ranges
}

pub(crate) fn class_to_list_of_literal(class: &Class) -> Vec<Hir> {
    let ranges = extract_class_to_ranges(class);

    if ranges.iter().any(|(_, max)| max > &255) {
        return vec![Hir::class(class.clone())];
    }

    ranges
        .iter()
        .flat_map(|&(min, max)| {
            (min..=max)
                .map(|x| Hir::literal(vec![x as u8]))
                .collect_vec()
        })
        .collect_vec()
}
