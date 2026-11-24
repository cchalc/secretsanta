//! Automatic accommodation for rosters that cannot satisfy every rule
//! (e.g. `list_chalcraft.csv`).
//!
//! The accommodation is the *smallest* relaxation, chosen lexicographically by
//! how much we care about each rule:
//!
//! 1. `NoSelf` is never relaxed.
//! 2. `NoSameHousehold` exceptions are minimised first,
//! 3. then `NoReciprocal`,
//! 4. then `HouseholdSpread`.
//!
//! Each step finds the fewest exceptions that still leave a solution, with the
//! later rules temporarily unbounded. Sampling under the resulting limits is
//! uniform over exactly the optimal assignments: any assignment within the
//! limits must hit every limit exactly, since each one is a minimum.

use std::fmt;

use rand::Rng;

use crate::domain::Roster;
use crate::rules::{Policy, Rule, UNBOUNDED};
use crate::solver::{self, Budget, Diagnosis, Solution, SolveError};

/// Relaxable rules, most important first.
pub const KEEP_PRIORITY: [Rule; 3] = [
    Rule::NoSameHousehold,
    Rule::NoReciprocal,
    Rule::HouseholdSpread,
];

/// A solution, the policy it satisfies, and why that policy had to be relaxed
/// (if it was).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pairing {
    pub solution: Solution,
    pub policy: Policy,
    relaxed_because: Option<Diagnosis>,
}

impl Pairing {
    /// The accommodation that was applied, if any.
    pub fn accommodation(&self) -> Option<Accommodation<'_>> {
        self.relaxed_because
            .as_ref()
            .map(|diagnosis| Accommodation {
                diagnosis,
                policy: &self.policy,
            })
    }
}

/// What was relaxed, and why. Displays as a plain-language notice that
/// contains no participant names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accommodation<'a> {
    /// Why the original policy was impossible.
    pub diagnosis: &'a Diagnosis,
    /// The minimal relaxed policy used instead.
    pub policy: &'a Policy,
}

impl fmt::Display for Accommodation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "⚠️  This roster cannot satisfy every rule:")?;
        for reason in &self.diagnosis.reasons {
            writeln!(f, "     - {reason}")?;
        }
        writeln!(
            f,
            "   santa is accommodating this automatically with the smallest possible change:"
        )?;
        for (rule, n) in self.policy.exceptions() {
            let exceptions = if n == 1 {
                "1 exception".to_owned()
            } else {
                format!("{n} exceptions")
            };
            writeln!(
                f,
                "     - {rule}: {exceptions} allowed, so {}",
                rule.exception_meaning()
            )?;
        }
        writeln!(
            f,
            "   Everyone still gives exactly one gift and receives exactly one gift.\n   Every other rule stays strict, and nobody ever gives to themselves."
        )
    }
}

/// The smallest relaxation of `base` that admits at least one assignment.
/// Returns `base` itself when it is already satisfiable.
pub fn minimal_policy(
    roster: &Roster,
    base: &Policy,
    node_budget: u64,
) -> Result<Policy, SolveError> {
    let relaxable: Vec<Rule> = KEEP_PRIORITY
        .iter()
        .copied()
        .filter(|&r| r.is_relaxable() && base.limit(r).is_some())
        .collect();
    let wide_open = relaxable.iter().fold(base.clone(), |policy, &rule| {
        policy.with_limit(rule, UNBOUNDED)
    });

    relaxable.iter().try_fold(wide_open, |policy, &rule| {
        let floor = base.limit(rule).unwrap_or(0);
        smallest_limit(roster, &policy, rule, floor, node_budget)
            .map(|k| policy.with_limit(rule, k))
    })
}

/// Fewest exceptions to `rule` (at least `floor`) for which `policy` is
/// satisfiable.
///
/// Feasibility is monotone in the limit, so this gallops (floor, +1, +2, +4,
/// …) to the first feasible limit and then binary-searches the gap. The answer
/// is usually 0 or 1, which takes one or two probes; large answers take
/// O(log n) probes instead of O(n). Each infeasible probe is an exhaustive
/// search, so the probe count is what matters.
fn smallest_limit(
    roster: &Roster,
    policy: &Policy,
    rule: Rule,
    floor: u32,
    node_budget: u64,
) -> Result<u32, SolveError> {
    let feasible_at = |k: u32| {
        solver::feasible(roster, &policy.with_limit(rule, k), node_budget)
            .ok_or(SolveError::Undetermined)
    };
    // `n` exceptions always suffice: the previous step proved a solution exists
    // with this rule unbounded, and no rule can be broken more than n times.
    let max = (roster.len() as u32).max(floor);

    let (mut lo, mut hi, mut step) = (floor, floor, 1);
    while !feasible_at(hi)? {
        if hi == max {
            return Err(SolveError::Undetermined);
        }
        lo = hi + 1;
        hi = hi.saturating_add(step).min(max);
        step = step.saturating_mul(2);
    }
    // Invariant: every limit below `lo` is infeasible and `hi` is feasible.
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if feasible_at(mid)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(hi)
}

/// Solve under `base`; if that is impossible, solve under the minimal
/// accommodation instead and report what was relaxed.
pub fn solve_with_accommodation<R: Rng + ?Sized>(
    roster: &Roster,
    base: &Policy,
    rng: &mut R,
    budget: Budget,
) -> Result<Pairing, SolveError> {
    match solver::solve(roster, base, rng, budget) {
        Ok(solution) => Ok(Pairing {
            solution,
            policy: base.clone(),
            relaxed_because: None,
        }),
        Err(SolveError::Infeasible(diagnosis)) => {
            let policy = minimal_policy(roster, base, budget.search_nodes)?;
            let solution = solver::solve(roster, &policy, rng, budget)?;
            Ok(Pairing {
                solution,
                policy,
                relaxed_because: Some(diagnosis),
            })
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn rng() -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(5)
    }

    #[test]
    fn feasible_roster_needs_no_accommodation() {
        let r = synthetic_roster(6, &[2]);
        assert_eq!(
            minimal_policy(&r, &Policy::strict(), u64::MAX),
            Ok(Policy::strict())
        );
        let p =
            solve_with_accommodation(&r, &Policy::strict(), &mut rng(), Budget::default()).unwrap();
        assert!(p.accommodation().is_none());
        assert_eq!(p.policy, Policy::strict());
    }

    #[test]
    fn chalcraft_shape_needs_exactly_one_household_spread_exception() {
        let r = synthetic_roster(5, &[2, 1, 2]);
        let expected = Policy::strict().with_limit(Rule::HouseholdSpread, 1);
        assert_eq!(
            minimal_policy(&r, &Policy::strict(), u64::MAX),
            Ok(expected.clone())
        );

        let p =
            solve_with_accommodation(&r, &Policy::strict(), &mut rng(), Budget::default()).unwrap();
        let acc = p.accommodation().expect("must be accommodated");
        assert_eq!(*acc.policy, expected);
        let notice = acc.to_string();
        assert!(
            notice.contains("HouseholdSpread: 1 exception allowed"),
            "{notice}"
        );
        assert!(!notice.contains("Person"), "notice must not name anyone");
        assert!(!acc.diagnosis.reasons.is_empty());
        assert!(expected.check(&r, &p.solution.assignment).is_ok());
        assert_eq!(
            expected.exceptions_used(&r, &p.solution.assignment),
            vec![(Rule::HouseholdSpread, 1)]
        );
    }

    #[test]
    fn oversized_household_relaxes_the_least_important_rule_it_can() {
        // Household of 3 among 5: NoSameHousehold is impossible to keep strictly
        // (3 > 5 − 3), so it gets the fewest exceptions possible.
        let r = synthetic_roster(5, &[3, 1, 1]);
        let p = minimal_policy(&r, &Policy::strict(), u64::MAX).unwrap();
        assert_eq!(p.limit(Rule::NoSelf), Some(0));
        assert_eq!(p.limit(Rule::NoSameHousehold), Some(1));
        let s = solver::solve(&r, &p, &mut rng(), Budget::default()).unwrap();
        assert!(p.check(&r, &s.assignment).is_ok());
    }

    #[test]
    fn everyone_in_one_household_still_gets_a_pairing() {
        let r = synthetic_roster(4, &[4]);
        let p =
            solve_with_accommodation(&r, &Policy::strict(), &mut rng(), Budget::default()).unwrap();
        assert!(p.accommodation().is_some());
        assert_eq!(
            p.policy.limit(Rule::NoSelf),
            Some(0),
            "NoSelf is never relaxed"
        );
        assert!(p.policy.check(&r, &p.solution.assignment).is_ok());
    }
}

#[cfg(test)]
mod gallop_tests {
    use super::*;
    use crate::domain::synthetic_roster;

    #[test]
    fn large_answers_are_found_exactly() {
        // Everyone in one household of 8: NoSameHousehold needs 8 exceptions.
        let r = synthetic_roster(8, &[8]);
        let p = minimal_policy(&r, &Policy::strict(), u64::MAX).unwrap();
        assert_eq!(p.limit(Rule::NoSameHousehold), Some(8));
    }
}
