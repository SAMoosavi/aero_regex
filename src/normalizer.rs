use crate::{regex_expr::RegexExpr, utilities::class_to_list_of_literal};
use itertools::Itertools;

pub(crate) fn normalize(regex: &RegexExpr) -> RegexExpr {
    match regex {
        RegexExpr::Repetition { min, max, sub } => match (*min, *max) {
            (min, Some(max)) if min == max => normalize_repeat(sub, min),
            (min, Some(max)) => {
                let mut current = normalize_repeat(sub, min);

                let mut alternations = Vec::with_capacity((max - min + 1) as usize);
                alternations.push(current.clone());

                for _ in min..max {
                    current = normalize(&RegexExpr::concat(&[current, (**sub).clone()]));
                    alternations.push(current.clone());
                }

                normalize(&RegexExpr::alternation(&alternations))
            }
            (0, None) => regex.clone(),
            (min, None) => {
                let star = RegexExpr::Repetition {
                    min: 0,
                    max: None,
                    sub: sub.clone(),
                };

                normalize(&RegexExpr::concat(&[normalize_repeat(sub, min), star]))
            }
        },
        RegexExpr::Alternation(hirs) => {
            RegexExpr::alternation(&hirs.iter().map(normalize).collect_vec())
        }
        RegexExpr::Concat(hirs) => {
            assert!(hirs.len() >= 2, "Concat must have at least 2 elements");

            let (first, rest) = hirs.split_first().unwrap();

            let result = rest
                .iter()
                .fold(vec![normalize(first)], |mut acc, current| {
                    let current = normalize(current);
                    let prev = acc.pop().unwrap();

                    let lhs = or_literal(&prev);
                    let rhs = or_literal(&current);

                    if lhs.len() * rhs.len() <= 5000 {
                        let merged_product = lhs
                            .iter()
                            .cartesian_product(rhs.iter())
                            .map(|(x, y)| concat(x, y))
                            .collect::<Vec<_>>();
                        acc.push(RegexExpr::alternation(&merged_product));
                    } else {
                        acc.push(prev);
                        acc.push(current);
                    }

                    acc
                });

            RegexExpr::concat(&result)
        }
        RegexExpr::Empty
        | RegexExpr::Literal(_)
        | RegexExpr::Class(_)
        | RegexExpr::Dot
        | RegexExpr::Start
        | RegexExpr::End => regex.clone(),
    }
}

fn normalize_repeat(sub: &RegexExpr, n: u32) -> RegexExpr {
    match n {
        0 => RegexExpr::Empty,
        1 => normalize(sub),
        n if n % 2 == 0 => {
            let half = normalize_repeat(sub, n / 2);
            normalize(&RegexExpr::concat(&[half.clone(), half]))
        }
        n => {
            let half = normalize_repeat(sub, n / 2);
            let doubled = normalize(&RegexExpr::concat(&[half.clone(), half]));
            normalize(&RegexExpr::concat(&[sub.clone(), doubled]))
        }
    }
}

fn concat(left: &RegexExpr, right: &RegexExpr) -> RegexExpr {
    match (left, right) {
        (RegexExpr::Alternation(lhs), _) => {
            let alts = lhs.iter().map(|hir| concat(hir, right)).collect_vec();
            RegexExpr::alternation(&alts)
        }
        (_, RegexExpr::Alternation(rhs)) => {
            let alts = rhs.iter().map(|hir| concat(left, hir)).collect_vec();
            RegexExpr::alternation(&alts)
        }
        _ => RegexExpr::concat(&[left.clone(), right.clone()]),
    }
}

fn or_literal(regex: &RegexExpr) -> Vec<RegexExpr> {
    match regex {
        RegexExpr::Alternation(hirs) => hirs.iter().flat_map(or_literal).collect_vec(),
        RegexExpr::Class(class) => class_to_list_of_literal(class),
        RegexExpr::Literal(_)
        | RegexExpr::Empty
        | RegexExpr::Repetition { .. }
        | RegexExpr::Dot
        | RegexExpr::Start
        | RegexExpr::End
        | RegexExpr::Concat(_) => vec![regex.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(pattern: &str) -> RegexExpr {
        RegexExpr::parse(pattern)
    }

    fn check(input: &str, expected: &str) {
        let normalized = normalize(&parse(input));
        let expected = parse(expected);
        assert_eq!(normalized, expected, "normalize({input:?}) != {expected:?}");
    }

    mod repetition {
        use super::*;

        #[test]
        fn exact_repeat() {
            check(r"a{3}", r"aaa");
        }

        #[test]
        fn exact_repeat_zero() {
            check(r"a{0}", r"");
        }

        #[test]
        fn exact_repeat_one() {
            check(r"a{1}", r"a");
        }

        #[test]
        fn range_repeat() {
            check(r"a{2,4}", r"aa|aaa|aaaa");
        }

        #[test]
        fn range_repeat_from_zero() {
            check(r"a{0,2}", r"|a|aa");
        }

        #[test]
        fn range_repeat_to_infinite() {
            check(r"a{2,}", r"aaa*");
        }

        #[test]
        fn unbounded_repeat_unchanged() {
            check(r"a+", r"aa*");
        }

        #[test]
        fn unbounded_star_unchanged() {
            let input = parse(r"a*");
            assert_eq!(normalize(&input), input);
        }

        #[test]
        fn optional_literal_expands_to_alternation() {
            check(r"a?b", r"b|ab");
        }

        #[test]
        fn optional_wildcard_expands_to_alternation() {
            check(r".?a", r"a|.a");
        }

        #[test]
        fn optional_wildcard_expands_to_alternation2() {
            check(r"[1-2]{3}", r"111|112|121|122|211|212|221|222");
        }
    }

    mod concat {
        use super::*;

        #[test]
        fn literal_concat() {
            check(r"ab", r"ab");
        }

        #[test]
        fn alternation_concat_merges() {
            check(r"(a|b)(c|d)", r"ac|ad|bc|bd");
        }
    }

    mod capture {
        use super::*;

        #[test]
        fn capture_with_repeat() {
            check(r"(a{2}|b{3})", r"aa|bbb");
        }
    }

    mod alternation {
        use super::*;

        #[test]
        fn alternation_with_repeats() {
            check(r"a{2}|b{2}", r"aa|bb");
        }

        #[test]
        fn nested_alternation() {
            check(r"(?:a{2}|b{2})|c{3}", r"aa|bb|ccc");
        }

        #[test]
        fn nested_alternation_with_renge_repeat() {
            check(r"(?:a{2,4}|b{2})|c{3}", r"aa|aaa|aaaa|bb|ccc");
        }

        #[test]
        fn alternation_with_big_range() {
            check(r"a(.|a)", r"a.|aa");
        }

        #[test]
        fn case_insensitive() {
            check(r"(?i)a", r"a|A");
        }
    }

    mod complex {
        use super::*;

        #[test]
        fn nested_repeat() {
            check(r"(a{2}){3}", r"aaaaaa");
        }

        #[test]
        fn mixed_concat_and_repeat() {
            check(r"a{2}b{3}", r"aabbb");
        }

        #[test]
        fn deeply_nested() {
            check(r"((a{2}|b{2})(c{2}|d{2}))", r"aacc|aadd|bbcc|bbdd");
        }

        #[test]
        fn deeply_nested_range_repeat() {
            check(
                r"((a{2,3}|b{2,3})(c{2}|d{2}))",
                r"aacc|aadd|aaacc|aaadd|bbcc|bbdd|bbbcc|bbbdd",
            );
        }
    }

    mod anchored {
        use super::*;

        #[test]
        fn end_anchors_distribute_over_alternation() {
            check(r"a(a|b)$", r"aa$|ab$");
        }
    }

    mod bench {
        use super::*;
        use std::hint;
        use std::time::Instant;

        fn bench_normalize(index: usize, input: &str) {
            let hir = parse(input);

            let start = Instant::now();
            let normalized = normalize(&hir);
            let elapsed = start.elapsed();

            println!("[{index}] elapsed: {elapsed:?}");

            hint::black_box(normalized);
        }

        #[test]
        fn test_dont_crash() {
            let test_cases = [
                r"(^a([1-9]|1[0-9]|2[0-9]|3[0-9]|4[0-9]|5[0-9])([0-9]{2})\.(b|g|g2|gd|na|q|w7)a$)",
                r"(^a([1-9]|1[0-9]|2[0-9]|3[0-9]|4[0-9]|5[0-9])([0-9]{2})\.(b|g|g2|gd|na|q|w7)a$)|(^a1$)|(^a2$)|(^a3$)|(^a4$)|(^a5$)|(^a6$)|(^a7$)|(^a8$)|(^a9$)|(^a10$)|(^a11$)|(^a12$)|(^a13$)|(^a14$)|(^a15$)|(^a16$)|(^a17$)|(^a18$)|(^a19$)|(^a20$)|(^a21$)|(^a22$)|(^a23$)|(^a24$)|(^a25$)|(^a26$)|(^a27$)|(^a28$)|(^a29$)|(^a30$)|(^a31$)|(^a32$)|(^a33$)|(^a34$)|(^a35$)|(^a36$)|(^a37$)|(^a38$)|(^a39$)|(^a40$)|(^a41$)|(^a42$)|(^a43$)",
                r"^word[a-z]{3}(0|1|2|3|4|5|6|7|8|9).{3,4}word$",
                r"^(Qm[0-9]{44})|(b[0-9]{58})a$",
                r"^(Qm[1-9A-HJ-NP-Za-km-z]{44})|(b[a-z2-7]{58})a$",
                r"^(Qm[1-9A-HJ-NP-Za-km-z]{44,58})|(b[a-z2-7]{58})a$",
                r"^(?:a|b)[0-9]{2,5}.{10}test([0-9]){2,}test$",
            ];

            test_cases.iter().enumerate().for_each(|(i, input)| {
                bench_normalize(i, input);
            })
        }
    }
}
