use itertools::Itertools;
use regex_syntax::hir::{self, Class, Hir, HirKind};

pub(crate) fn normalize(regex: &Hir) -> Hir {
    match regex.kind() {
        HirKind::Repetition(rep) => {
            let sub = (*rep.sub).clone();

            match (rep.min, rep.max) {
                (min, Some(max)) if min == max => normalize_repeat(&sub, min),
                (min, Some(max)) => {
                    let mut current = normalize_repeat(&sub, min);

                    let mut alternations = Vec::with_capacity((max - min + 1) as usize);
                    alternations.push(current.clone());

                    for _ in min..max {
                        current = normalize(&Hir::concat(vec![current, sub.clone()]));
                        alternations.push(current.clone());
                    }

                    normalize(&Hir::alternation(alternations))
                }
                (0, None) => regex.clone(),
                (min, None) => {
                    let star = Hir::repetition(hir::Repetition {
                        min: 0,
                        max: None,
                        greedy: true,
                        sub: rep.sub.clone(),
                    });

                    normalize(&Hir::concat(vec![normalize_repeat(&sub, min), star]))
                }
            }
        }
        HirKind::Capture(capture) => normalize(&capture.sub),
        HirKind::Alternation(hirs) => Hir::alternation(hirs.iter().map(normalize).collect()),
        HirKind::Concat(hirs) => {
            assert!(hirs.len() >= 2, "Concat must have at least 2 elements");

            let (first, rest) = hirs.split_first().unwrap();

            let result = rest
                .iter()
                .fold(vec![normalize(first)], |mut acc, current| {
                    let current = normalize(current);
                    let prev = acc.last().unwrap();

                    match (or_literal(prev), or_literal(&current)) {
                        (lhs, rhs) if lhs.len() * rhs.len() <= 5000 => {
                            let merged = lhs
                                .iter()
                                .cartesian_product(rhs.iter())
                                .map(|(x, y)| concat(x, y))
                                .collect();
                            *acc.last_mut().unwrap() = Hir::alternation(merged);
                        }
                        _ => acc.push(current),
                    }

                    acc
                });

            Hir::concat(result)
        }
        HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) | HirKind::Look(_) => {
            regex.clone()
        }
    }
}

fn normalize_repeat(sub: &Hir, n: u32) -> Hir {
    match n {
        0 => Hir::empty(),
        1 => normalize(sub),
        n if n % 2 == 0 => {
            let half = normalize_repeat(sub, n / 2);
            normalize(&Hir::concat(vec![half.clone(), half]))
        }
        n => {
            let half = normalize_repeat(sub, n / 2);
            let doubled = normalize(&Hir::concat(vec![half.clone(), half]));
            normalize(&Hir::concat(vec![sub.clone(), doubled]))
        }
    }
}

fn concat(left: &Hir, right: &Hir) -> Hir {
    match (left.kind(), right.kind()) {
        (HirKind::Alternation(lhs), _) => {
            let alts = lhs.iter().map(|hir| concat(hir, right)).collect();
            Hir::alternation(alts)
        }
        (_, HirKind::Alternation(rhs)) => {
            let alts = rhs.iter().map(|hir| concat(left, hir)).collect();
            Hir::alternation(alts)
        }
        _ => Hir::concat(vec![left.clone(), right.clone()]),
    }
}

fn or_literal(regex: &Hir) -> Vec<Hir> {
    match regex.kind() {
        HirKind::Alternation(hirs) => hirs.iter().flat_map(or_literal).collect_vec(),
        HirKind::Class(class) => extract_class_ranges(class),
        HirKind::Literal(_)
        | HirKind::Empty
        | HirKind::Look(_)
        | HirKind::Repetition(_)
        | HirKind::Capture(_)
        | HirKind::Concat(_) => vec![regex.clone()],
    }
}

fn extract_class_ranges(class: &Class) -> Vec<Hir> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use regex_syntax::Parser;

    fn parse(pattern: &str) -> Hir {
        Parser::new().parse(pattern).unwrap()
    }

    fn check(input: &str, expected: &str) {
        let normalized = normalize(&parse(input));
        let expected = parse(expected);
        assert_eq!(normalized, expected, "normalize({input:?}) != {expected:?}");
    }

    // Repetition tests
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
            //
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

    // Concat tests
    mod concat {
        use super::*;

        #[test]
        fn literal_concat() {
            check(r"ab", r"ab");
        }

        #[test]
        fn alternation_concat_merges() {
            // (a|b)(c|d) should expand when small enough
            check(r"(a|b)(c|d)", r"ac|ad|bc|bd");
        }
    }

    // Capture tests
    mod capture {
        use super::*;

        #[test]
        fn capture_with_repeat() {
            check(r"(a{2}|b{3})", r"aa|bbb");
        }
    }

    // Alternation tests
    mod alternation {
        use super::*;

        #[test]
        fn alternation_with_repeats() {
            check(r"a{2}|b{2}", r"aa|bb");
        }

        #[test]
        fn nested_alternation() {
            check(r"(a{2}|b{2})|c{3}", r"aa|bb|ccc");
        }

        #[test]
        fn nested_alternation_with_renge_repeat() {
            check(r"(a{2,4}|b{2})|c{3}", r"aa|aaa|aaaa|bb|ccc");
        }
    }

    // Complex tests
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

        #[test]
        fn a() {
            check(r"a(.|a)", r"a.|aa");
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

            // prevent optimization if benchmarking in release
            hint::black_box(normalized);
        }

        #[test]
        fn test_dont_crash() {
            let test_cases = [
                r"(^a([1-9]|1[0-9]|2[0-9]|3[0-9]|4[0-9]|5[0-9])([0-9]{2})\.(b|g|g2|gd|na|q|w7)\.akamai\.net$)",
                r"(^a([1-9]|1[0-9]|2[0-9]|3[0-9]|4[0-9]|5[0-9])([0-9]{2})\.(b|g|g2|gd|na|q|w7)\.akamai\.net$)|(^prfzquobemaspprl-a.akamaihd.net$)|(^www.moodpovest.com$)|(^www.campusvnweightlosswall.com$)|(^www.apparelbeachmint.com$)|(^yryirjrhqffsxwpg-a.akamaihd.net$)|(^www.firmextremedrink.com$)|(^www.buffalothreetronicsquad.com$)|(^www.desktopsoleixfarms.com$)|(^morer-emaging-disual.b-cdn.net$)|(^www.carzooadvice.com$)|(^www.phototrustrad.com$)|(^www.stickersrbspecial.com$)|(^a248.e.akamai.net$)|(^www.irantelcasinoruby.com$)|(^www.traveleryouthza.com$)|(^www.trisoftwaremotordream.com$)|(^alchzwwfzmrtgoaf-a.akamaihd.net$)|(^www.streambaracademysoftware.com$)|(^www.boxsignalnet.com$)|(^www.taxtomobilemarketplace.com$)|(^prod.global.ssl.fastly.net$)|(^www.oilcoffeembapixel.com$)|(^www.coffeeomgmultimediapsychic.net$)|(^www.whybdci.com$)|(^www.whizwiredak.com$)|(^www.networksservicestatmaryland.com$)|(^www.profkiwibellstudy.com$)|(^www.memodevmodelradar.com$)|(^www.checkbingnutritionclick.com$)|(^www.titanficentraleat.com$)|(^www.luxurybillitalylift.com$)|(^www.gsmyounginabox.com$)|(^www.storycitiesprofit.com$)|(^www.insightevilact.com$)|(^www.honeyelitengprotection.com$)|(^www.guiderevolutionligolf.com$)|(^www.mafiaearproperty.com$)|(^www.flexipixelsmagical.com$)|(^www.smithleafpartner.com$)|(^www.delivptattoo.com$)|(^www.contactiwebincorporated.com$)|(^www.watchescapins.com$)|(^www.investorbaltimoreloop.com$)",
                r"^word[a-z]{3}(0|1|2|3|4|5|6|7|8|9).{3,4}word$",
                r"^(Qm[0-9]{44})|(b[0-9]{58})\.ipfs\.dweb\.link$",
                r"^(Qm[1-9A-HJ-NP-Za-km-z]{44})|(b[a-z2-7]{58})\.ipfs\.dweb\.link$",
                r"^(Qm[1-9A-HJ-NP-Za-km-z]{44,58})|(b[a-z2-7]{58})\.ipfs\.dweb\.link$",
            ];

            test_cases.iter().enumerate().for_each(|(i, input)| {
                bench_normalize(i, input);
            })
        }
    }
}
