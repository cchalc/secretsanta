//! Correctness against an independent brute-force oracle.
//!
//! The oracle below shares no code with `santa::rules`/`santa::solver`: it is a
//! direct transcription of the four rules (as in the original Python) checked
//! over *every* permutation.

use std::collections::HashMap;
use std::path::Path;

use proptest::prelude::*;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use santa::accommodate::{minimal_policy, solve_with_accommodation};
use santa::domain::{Roster, roster_from_households};
use santa::rules::{Policy, Rule};
use santa::solver::{Budget, Count, SolveError, count_solutions, solve};

// ---------------------------------------------------------------- oracle ----

fn oracle_valid(h: &[usize], p: &[usize]) -> bool {
    let n = p.len();
    let pairwise = (0..n).all(|i| p[i] != i && p[p[i]] != i && h[i] != h[p[i]]);
    let spread = (0..n).all(|i| (0..n).all(|j| i == j || h[i] != h[j] || h[p[i]] != h[p[j]]));
    pairwise && spread
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn go(prefix: &mut Vec<usize>, used: &mut [bool], out: &mut Vec<Vec<usize>>) {
        if prefix.len() == used.len() {
            out.push(prefix.clone());
            return;
        }
        for i in 0..used.len() {
            if !used[i] {
                used[i] = true;
                prefix.push(i);
                go(prefix, used, out);
                prefix.pop();
                used[i] = false;
            }
        }
    }
    let mut out = Vec::new();
    go(&mut Vec::new(), &mut vec![false; n], &mut out);
    out
}

fn oracle_solutions(h: &[usize]) -> Vec<Vec<usize>> {
    permutations(h.len())
        .into_iter()
        .filter(|p| oracle_valid(h, p))
        .collect()
}

fn households_of(r: &Roster) -> Vec<usize> {
    (0..r.len()).map(|i| r.household_of(i).0).collect()
}

fn rng(seed: u64) -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(seed)
}

// ------------------------------------------------------- the real rosters ----

fn load(name: &str) -> Roster {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    santa::roster::parse(std::fs::File::open(path).unwrap()).unwrap()
}

#[test]
fn real_rosters_match_brute_force_counts() {
    // Values independently computed with a Python brute force before the port.
    for (file, expected) in [
        ("list_holtby.csv", 32),
        ("list_testing.csv", 2),
        ("list_chalcraft.csv", 0),
    ] {
        let r = load(file);
        let oracle = oracle_solutions(&households_of(&r)).len() as u64;
        assert_eq!(oracle, expected, "{file}: oracle");
        assert_eq!(
            count_solutions(&r, &Policy::strict(), u64::MAX, u64::MAX),
            Count::Exact(expected),
            "{file}: solver count"
        );
    }
}

#[test]
fn chalcraft_list_is_refused_not_silently_paired() {
    // Regression for the critical Python bug: it wrote an invalid pairing.
    let r = load("list_chalcraft.csv");
    for seed in 0..50 {
        match solve(&r, &Policy::strict(), &mut rng(seed), Budget::default()) {
            Err(SolveError::Infeasible(d)) => {
                assert!(!d.reasons.is_empty());
                assert!(d.blocking_rules.contains(&Rule::HouseholdSpread));
            }
            other => panic!("seed {seed}: expected Infeasible, got {other:?}"),
        }
    }
}

#[test]
fn every_solution_for_real_feasible_rosters_is_oracle_valid() {
    for file in ["list_holtby.csv", "list_testing.csv"] {
        let r = load(file);
        let h = households_of(&r);
        for seed in 0..500 {
            let s = solve(&r, &Policy::strict(), &mut rng(seed), Budget::default()).unwrap();
            assert!(
                oracle_valid(&h, s.assignment.receivers()),
                "{file} seed {seed}"
            );
        }
    }
}

// ------------------------------------------------------------ uniformity ----

#[test]
fn sampling_is_uniform_over_valid_assignments() {
    // Holtby shape: 3 households of 2 → 32 valid assignments.
    let r = roster_from_households(&[0, 0, 1, 1, 2, 2]);
    let solutions = oracle_solutions(&households_of(&r));
    assert_eq!(solutions.len(), 32);
    let index: HashMap<Vec<usize>, usize> = solutions
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, s)| (s, i))
        .collect();

    const SAMPLES: usize = 64_000;
    let mut rng = rng(2026);
    let counts = (0..SAMPLES).fold(vec![0usize; 32], |mut c, _| {
        let s = solve(&r, &Policy::strict(), &mut rng, Budget::default()).unwrap();
        c[index[s.assignment.receivers()]] += 1;
        c
    });

    let expected = SAMPLES as f64 / 32.0;
    let chi2: f64 = counts
        .iter()
        .map(|&o| (o as f64 - expected).powi(2) / expected)
        .sum();
    // 31 degrees of freedom; p = 0.001 critical value ≈ 61.10.
    assert!(chi2 < 61.10, "chi² = {chi2:.2}, counts = {counts:?}");
    assert!(counts.iter().all(|&c| c > 0));
}

// -------------------------------------------------------- property tests ----

fn households_strategy() -> impl Strategy<Value = Vec<usize>> {
    (3usize..=7).prop_flat_map(|n| proptest::collection::vec(0usize..4, n))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn count_matches_oracle(h in households_strategy()) {
        let r = roster_from_households(&h);
        let oracle = oracle_solutions(&households_of(&r)).len() as u64;
        prop_assert_eq!(count_solutions(&r, &Policy::strict(), u64::MAX, u64::MAX), Count::Exact(oracle));
    }

    #[test]
    fn solve_agrees_with_oracle(h in households_strategy(), seed in any::<u64>()) {
        let r = roster_from_households(&h);
        let hh = households_of(&r);
        let feasible = !oracle_solutions(&hh).is_empty();
        for budget in [Budget::default(), Budget { sample_attempts: 0, ..Budget::default() }] {
            match solve(&r, &Policy::strict(), &mut rng(seed), budget) {
                Ok(s) => {
                    prop_assert!(feasible);
                    prop_assert!(oracle_valid(&hh, s.assignment.receivers()));
                    prop_assert!(Policy::strict().check(&r, &s.assignment).is_ok());
                }
                Err(SolveError::Infeasible(_)) => prop_assert!(!feasible),
                Err(SolveError::Undetermined) => prop_assert!(false, "small roster must be decided"),
            }
        }
    }

    #[test]
    fn prechecks_never_reject_a_feasible_roster(h in households_strategy()) {
        let r = roster_from_households(&h);
        if !oracle_solutions(&households_of(&r)).is_empty() {
            prop_assert!(santa::solver::precheck(&r, &Policy::strict()).is_empty());
        }
    }
}

#[test]
fn eight_person_rosters_match_oracle() {
    for h in [
        [0, 0, 1, 1, 2, 2, 3, 3],
        [0, 0, 0, 1, 1, 2, 2, 3],
        [0, 1, 2, 3, 4, 5, 6, 7],
        [0, 0, 0, 1, 1, 1, 2, 2],
    ] {
        let r = roster_from_households(&h);
        let oracle = oracle_solutions(&h).len() as u64;
        assert_eq!(
            count_solutions(&r, &Policy::strict(), u64::MAX, u64::MAX),
            Count::Exact(oracle),
            "{h:?}"
        );
    }
}

// ---------------------------------------------------------- accommodation ----

/// Exception counts per relaxable rule: (same household, reciprocal pairs,
/// household-spread repeats). Independent of `santa::rules`.
fn oracle_exceptions(h: &[usize], p: &[usize]) -> Option<(u32, u32, u32)> {
    let n = p.len();
    if (0..n).any(|i| p[i] == i) {
        return None; // NoSelf is never relaxed
    }
    let same = (0..n).filter(|&i| h[i] == h[p[i]]).count() as u32;
    let recip = (0..n).filter(|&i| i < p[i] && p[p[i]] == i).count() as u32;
    let mut cells: HashMap<(usize, usize), u32> = HashMap::new();
    (0..n).for_each(|i| *cells.entry((h[i], h[p[i]])).or_default() += 1);
    let spread = cells.values().map(|c| c - 1).sum();
    Some((same, recip, spread))
}

/// Lexicographically smallest exception profile over every permutation.
fn oracle_min_exceptions(h: &[usize]) -> (u32, u32, u32) {
    permutations(h.len())
        .iter()
        .filter_map(|p| oracle_exceptions(h, p))
        .min()
        .expect("n >= 2 always has a derangement")
}

fn policy_from(min: (u32, u32, u32)) -> Policy {
    Policy::strict()
        .with_limit(Rule::NoSameHousehold, min.0)
        .with_limit(Rule::NoReciprocal, min.1)
        .with_limit(Rule::HouseholdSpread, min.2)
}

#[test]
fn chalcraft_list_is_accommodated_with_one_household_spread_exception() {
    let r = load("list_chalcraft.csv");
    let h = households_of(&r);
    assert_eq!(oracle_min_exceptions(&h), (0, 0, 1));
    let policy = minimal_policy(&r, &Policy::strict(), u64::MAX).unwrap();
    assert_eq!(
        policy,
        Policy::strict().with_limit(Rule::HouseholdSpread, 1)
    );
    for seed in 0..200 {
        let p = solve_with_accommodation(&r, &Policy::strict(), &mut rng(seed), Budget::default())
            .unwrap();
        assert_eq!(
            oracle_exceptions(&h, p.solution.assignment.receivers()),
            Some((0, 0, 1))
        );
    }
}

#[test]
fn accommodated_sampling_is_uniform_over_optimal_assignments() {
    let r = load("list_chalcraft.csv");
    let h = households_of(&r);
    let optimal: Vec<Vec<usize>> = permutations(h.len())
        .into_iter()
        .filter(|p| oracle_exceptions(&h, p) == Some((0, 0, 1)))
        .collect();
    let index: HashMap<Vec<usize>, usize> = optimal
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, s)| (s, i))
        .collect();
    const SAMPLES: usize = 20_000;
    let mut rng = rng(99);
    let counts = (0..SAMPLES).fold(vec![0usize; optimal.len()], |mut c, _| {
        let p =
            solve_with_accommodation(&r, &Policy::strict(), &mut rng, Budget::default()).unwrap();
        c[index[p.solution.assignment.receivers()]] += 1;
        c
    });
    let expected = SAMPLES as f64 / optimal.len() as f64;
    for (i, &c) in counts.iter().enumerate() {
        let dev = (c as f64 - expected).abs() / expected;
        assert!(
            dev < 0.15,
            "outcome {i}: {c} vs expected {expected:.0} ({} optimal)",
            optimal.len()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn accommodation_is_the_lexicographic_minimum(h in households_strategy(), seed in any::<u64>()) {
        let r = roster_from_households(&h);
        let hh = households_of(&r);
        let min = oracle_min_exceptions(&hh);
        let expected = policy_from(min);
        let policy = minimal_policy(&r, &Policy::strict(), u64::MAX).unwrap();
        prop_assert_eq!(&policy, &expected);

        let p = solve_with_accommodation(&r, &Policy::strict(), &mut rng(seed), Budget::default()).unwrap();
        prop_assert_eq!(oracle_exceptions(&hh, p.solution.assignment.receivers()), Some(min));
        prop_assert_eq!(p.accommodation().is_some(), min != (0, 0, 0));
    }
}
