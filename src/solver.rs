//! Finding an assignment that satisfies a [`Policy`].
//!
//! Strategy:
//! 1. **Pre-checks**: cheap necessary conditions that explain the most common
//!    infeasible rosters in plain language.
//! 2. **Rejection sampling**: shuffle (Fisher–Yates, uniform over all n!
//!    permutations) until the policy holds. Conditioning a uniform
//!    distribution on a predicate yields a *uniform* draw over the valid
//!    assignments, so everyone is equally likely to get anyone allowed.
//! 3. **Exhaustive backtracking** when the sampling budget runs out: either
//!    finds a valid assignment or *proves* none exists. It never returns an
//!    invalid result.

use std::fmt;

use rand::Rng;
use rand::seq::SliceRandom;

use crate::domain::{Assignment, HouseholdId, ParticipantId, Roster};
use crate::rules::{Partial, Policy, Rule};

/// Giver → receiver edges that a draw may not use, normally because they were
/// delivered in the previous year. This stays separate from [`Policy`]: the
/// household rules describe a roster, while history is private run state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForbiddenEdges {
    n: usize,
    edges: Vec<bool>,
}

impl ForbiddenEdges {
    pub fn none(n: usize) -> Self {
        Self {
            n,
            edges: vec![false; n * n],
        }
    }

    pub fn forbid(&mut self, giver: ParticipantId, receiver: ParticipantId) {
        if giver < self.n && receiver < self.n {
            self.edges[giver * self.n + receiver] = true;
        }
    }

    pub fn contains(&self, giver: ParticipantId, receiver: ParticipantId) -> bool {
        giver < self.n && receiver < self.n && self.edges[giver * self.n + receiver]
    }

    pub fn extend(&mut self, assignment: &Assignment) {
        assignment.pairs().for_each(|(g, r)| self.forbid(g, r));
    }

    pub fn count(&self) -> usize {
        self.edges.iter().filter(|&&edge| edge).count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Maximum random shuffles before falling back to exhaustive search.
    pub sample_attempts: u64,
    /// Maximum search-tree nodes before giving up with `Undetermined`.
    pub search_nodes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            sample_attempts: 200_000,
            search_nodes: 50_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Uniform draw over all valid assignments.
    Sampled { attempts: u64 },
    /// Found by randomized backtracking: valid, but not guaranteed uniform.
    Searched,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solution {
    pub assignment: Assignment,
    pub method: Method,
}

/// Why a roster cannot be paired under a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnosis {
    /// Plain-language explanations from the pre-checks (may be empty when the
    /// infeasibility is only discovered by exhaustive search).
    pub reasons: Vec<String>,
    /// Rules whose removal *alone* would make the roster solvable.
    pub blocking_rules: Vec<Rule>,
}

impl fmt::Display for Diagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "no pairing satisfies every rule for this roster.")?;
        self.reasons
            .iter()
            .try_for_each(|r| writeln!(f, "  - {r}"))?;
        match self.blocking_rules.as_slice() {
            [] => writeln!(
                f,
                "  - no single rule is responsible; several rules conflict jointly."
            ),
            rules => {
                let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
                writeln!(
                    f,
                    "  - relaxing any one of these would help: {}",
                    names.join(", ")
                )
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SolveError {
    #[error("{0}")]
    Infeasible(Diagnosis),
    #[error("search budget exhausted before a pairing was found or ruled out; try a larger budget")]
    Undetermined,
}

/// Find an assignment for `roster` that satisfies `policy`.
pub fn solve<R: Rng + ?Sized>(
    roster: &Roster,
    policy: &Policy,
    rng: &mut R,
    budget: Budget,
) -> Result<Solution, SolveError> {
    solve_avoiding(
        roster,
        policy,
        &ForbiddenEdges::none(roster.len()),
        rng,
        budget,
    )
}

/// Find an assignment that satisfies `policy` and avoids every historical
/// edge. Rejection sampling remains uniform because it conditions the same
/// uniform permutation stream on one additional predicate.
pub fn solve_avoiding<R: Rng + ?Sized>(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    rng: &mut R,
    budget: Budget,
) -> Result<Solution, SolveError> {
    let reasons = precheck(roster, policy);
    if !reasons.is_empty() {
        return Err(infeasible(roster, policy, budget, reasons));
    }

    if let Some((assignment, attempts)) =
        sample(roster, policy, forbidden, rng, budget.sample_attempts)
    {
        return Ok(Solution {
            assignment,
            method: Method::Sampled { attempts },
        });
    }
    match search(
        roster,
        policy,
        forbidden,
        &mut |c| c.shuffle(rng),
        budget.search_nodes,
    ) {
        Outcome::Found(assignment) => Ok(Solution {
            assignment,
            method: Method::Searched,
        }),
        Outcome::Exhausted if forbidden.count() > 0 => Err(SolveError::Infeasible(Diagnosis {
            reasons: vec![
                "no pairing avoids every delivered giver/receiver edge from the previous year"
                    .into(),
            ],
            blocking_rules: Vec::new(),
        })),
        Outcome::Exhausted => Err(infeasible(roster, policy, budget, Vec::new())),
        Outcome::OutOfBudget => Err(SolveError::Undetermined),
    }
}

/// Rejection sampling. Returns the assignment and the attempt it was found on.
fn sample<R: Rng + ?Sized>(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    rng: &mut R,
    attempts: u64,
) -> Option<(Assignment, u64)> {
    let n = roster.len();
    let mut perm: Vec<ParticipantId> = (0..n).collect();
    (1..=attempts).find_map(|attempt| {
        perm.shuffle(rng);
        (policy.admits(roster, &perm)
            && perm
                .iter()
                .copied()
                .enumerate()
                .all(|(g, r)| !forbidden.contains(g, r)))
        .then(|| {
            let a =
                Assignment::from_receivers(perm.clone(), n).expect("a shuffle is a permutation");
            (a, attempt)
        })
    })
}

/// Cheap necessary conditions, with plain-language explanations. Only rules
/// the policy enforces *strictly* are pre-checked.
pub fn precheck(roster: &Roster, policy: &Policy) -> Vec<String> {
    let facts = Facts::new(roster, policy);
    [
        too_few_people(&facts),
        crowded_households(&facts),
        too_big_to_spread(&facts),
        overloaded_households(&facts),
    ]
    .concat()
}

/// Shared inputs for the pre-checks.
struct Facts<'a> {
    roster: &'a Roster,
    policy: &'a Policy,
    sizes: Vec<usize>,
    /// Households each member may give into under HouseholdSpread.
    targets: usize,
}

impl<'a> Facts<'a> {
    fn new(roster: &'a Roster, policy: &'a Policy) -> Self {
        let sizes = roster.household_sizes();
        let own_household_allowed = !policy.is_strict_about(Rule::NoSameHousehold);
        let targets = sizes.len() - 1 + usize::from(own_household_allowed);
        Facts {
            roster,
            policy,
            sizes,
            targets,
        }
    }

    fn strict(&self, rule: Rule) -> bool {
        self.policy.is_strict_about(rule)
    }

    fn label(&self, household: usize) -> String {
        format!("{:?}", self.roster.household_label(HouseholdId(household)))
    }

    fn households(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.sizes.iter().copied().enumerate()
    }
}

fn too_few_people(f: &Facts) -> Vec<String> {
    let pair_only = f.roster.len() == 2 && f.strict(Rule::NoSelf) && f.strict(Rule::NoReciprocal);
    pair_only
        .then(|| "with 2 participants they would have to give to each other".to_owned())
        .into_iter()
        .collect()
}

/// NoSameHousehold: a household's members need receivers outside it.
fn crowded_households(f: &Facts) -> Vec<String> {
    if !f.strict(Rule::NoSameHousehold) {
        return Vec::new();
    }
    let n = f.roster.len();
    f.households()
        .filter(|&(_, k)| k > n - k)
        .map(|(i, k)| {
            format!(
                "household {} has {k} of {n} participants, but its members need {k} receivers outside it",
                f.label(i)
            )
        })
        .collect()
}

/// HouseholdSpread: each member gives into a different household.
fn too_big_to_spread(f: &Facts) -> Vec<String> {
    if !f.strict(Rule::HouseholdSpread) {
        return Vec::new();
    }
    f.households()
        .filter(|&(_, k)| k > f.targets)
        .map(|(i, k)| {
            format!(
                "household {} has {k} members, but they must each give into a different household and only {} are available",
                f.label(i),
                f.targets
            )
        })
        .collect()
}

/// A household with exactly `targets` members must send one gift into
/// *every* other household; count those forced gifts per receiving household.
fn overloaded_households(f: &Facts) -> Vec<String> {
    if !(f.strict(Rule::HouseholdSpread) && f.strict(Rule::NoSameHousehold)) {
        return Vec::new();
    }
    let saturated: Vec<usize> = f
        .households()
        .filter(|&(_, k)| k == f.targets)
        .map(|(i, _)| i)
        .collect();
    f.households()
        .filter_map(|(j, size)| {
            let forced: Vec<String> = saturated.iter().filter(|&&i| i != j).map(|&i| f.label(i)).collect();
            (forced.len() > size).then(|| {
                format!(
                    "households {} each have {} members, so under the strict rules each would have to give one gift into every other household. That would mean household {} ({size} member(s)) receiving {} gifts, which is impossible because everyone receives exactly one",
                    forced.join(", "),
                    f.targets,
                    f.label(j),
                    forced.len()
                )
            })
        })
        .collect()
}

fn infeasible(
    roster: &Roster,
    policy: &Policy,
    budget: Budget,
    reasons: Vec<String>,
) -> SolveError {
    SolveError::Infeasible(Diagnosis {
        reasons,
        blocking_rules: blocking_rules(roster, policy, budget),
    })
}

/// Relaxable rules whose removal alone makes the roster feasible.
fn blocking_rules(roster: &Roster, policy: &Policy, budget: Budget) -> Vec<Rule> {
    policy
        .rules()
        .map(|(rule, _)| rule)
        .filter(|&rule| rule.is_relaxable())
        .filter(|&rule| feasible(roster, &policy.without(rule), budget.search_nodes) == Some(true))
        .collect()
}

/// `Some(true/false)` if decided within budget, `None` otherwise.
pub fn feasible(roster: &Roster, policy: &Policy, node_budget: u64) -> Option<bool> {
    feasible_avoiding(
        roster,
        policy,
        &ForbiddenEdges::none(roster.len()),
        node_budget,
    )
}

pub fn feasible_avoiding(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    node_budget: u64,
) -> Option<bool> {
    match search(roster, policy, forbidden, &mut |_| {}, node_budget) {
        Outcome::Found(_) => Some(true),
        Outcome::Exhausted => Some(false),
        Outcome::OutOfBudget => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    Exact(u64),
    /// Stopped counting at the limit.
    AtLeast(u64),
    /// Ran out of search budget; at least this many were seen.
    Unknown {
        seen: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolutionSet {
    Exact(Vec<Assignment>),
    AtLeast(usize),
    Unknown(usize),
}

/// Count valid assignments exactly (up to `limit`).
pub fn count_solutions(roster: &Roster, policy: &Policy, limit: u64, node_budget: u64) -> Count {
    count_solutions_avoiding(
        roster,
        policy,
        &ForbiddenEdges::none(roster.len()),
        limit,
        node_budget,
    )
}

pub fn count_solutions_avoiding(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    limit: u64,
    node_budget: u64,
) -> Count {
    let mut seen = 0u64;
    let flow = Search::new(
        roster,
        policy,
        forbidden,
        node_budget,
        &mut |_| {},
        &mut |_| {
            seen += 1;
            if seen >= limit {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
    .run();
    match flow {
        Flow::Continue => Count::Exact(seen),
        Flow::Stop => Count::AtLeast(seen),
        Flow::OutOfBudget => Count::Unknown { seen },
    }
}

/// Collect every admissible assignment up to `limit`. Intended for small
/// correction rosters where the caller must apply an additional look-ahead
/// predicate and then sample uniformly from the survivors.
pub fn collect_solutions_avoiding(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    limit: usize,
    node_budget: u64,
) -> SolutionSet {
    let mut found = Vec::new();
    let flow = Search::new(
        roster,
        policy,
        forbidden,
        node_budget,
        &mut |_| {},
        &mut |receivers| {
            let assignment = Assignment::from_receivers(receivers.to_vec(), roster.len())
                .expect("search completion is a permutation");
            found.push(assignment);
            if found.len() >= limit {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
    .run();
    match flow {
        Flow::Continue => SolutionSet::Exact(found),
        Flow::Stop => SolutionSet::AtLeast(found.len()),
        Flow::OutOfBudget => SolutionSet::Unknown(found.len()),
    }
}

enum Outcome {
    Found(Assignment),
    Exhausted,
    OutOfBudget,
}

/// Find the first assignment, exploring candidates in the order chosen by
/// `order` (a shuffle for randomized search, identity for deterministic).
fn search(
    roster: &Roster,
    policy: &Policy,
    forbidden: &ForbiddenEdges,
    order: &mut dyn FnMut(&mut Vec<ParticipantId>),
    node_budget: u64,
) -> Outcome {
    let mut found = None;
    let flow = Search::new(
        roster,
        policy,
        forbidden,
        node_budget,
        order,
        &mut |receivers| {
            found = Some(receivers.to_vec());
            Flow::Stop
        },
    )
    .run();
    match (flow, found) {
        (Flow::Stop, Some(receivers)) => {
            let a = Assignment::from_receivers(receivers, roster.len())
                .expect("search yields a permutation");
            debug_assert!(policy.admits(roster, a.receivers()));
            Outcome::Found(a)
        }
        (Flow::OutOfBudget, _) => Outcome::OutOfBudget,
        _ => Outcome::Exhausted,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
    OutOfBudget,
}

/// How the search picks the next giver.
enum GiverOrder {
    /// The giver with the fewest admissible receivers (minimum remaining
    /// values). Best pruning, but O(n²) admissibility checks per node.
    FewestOptions,
    /// A fixed order, largest households first. O(n) per node; used for big
    /// rosters, which are almost always solvable greedily.
    Fixed(Vec<ParticipantId>),
}

/// Rosters up to this size use `GiverOrder::FewestOptions`. At 50 people a
/// full search with it takes ~0.4 ms (see `benches/solve.rs`).
const FEWEST_OPTIONS_MAX_N: usize = 64;

/// Backtracking engine. The mutable `Partial` lives only inside one search;
/// callers see the pure functions above.
struct Search<'a> {
    roster: &'a Roster,
    policy: &'a Policy,
    forbidden: &'a ForbiddenEdges,
    partial: Partial,
    giver_order: GiverOrder,
    nodes: u64,
    budget: u64,
    order: &'a mut dyn FnMut(&mut Vec<ParticipantId>),
    on_complete: &'a mut dyn FnMut(&[ParticipantId]) -> Flow,
}

impl<'a> Search<'a> {
    fn new(
        roster: &'a Roster,
        policy: &'a Policy,
        forbidden: &'a ForbiddenEdges,
        budget: u64,
        order: &'a mut dyn FnMut(&mut Vec<ParticipantId>),
        on_complete: &'a mut dyn FnMut(&[ParticipantId]) -> Flow,
    ) -> Self {
        let giver_order = if roster.len() <= FEWEST_OPTIONS_MAX_N {
            GiverOrder::FewestOptions
        } else {
            let sizes = roster.household_sizes();
            let mut fixed: Vec<ParticipantId> = (0..roster.len()).collect();
            fixed.sort_by_key(|&g| std::cmp::Reverse(sizes[roster.household_of(g).0]));
            GiverOrder::Fixed(fixed)
        };
        Search {
            roster,
            policy,
            forbidden,
            partial: Partial::new(roster),
            giver_order,
            nodes: 0,
            budget,
            order,
            on_complete,
        }
    }

    fn run(mut self) -> Flow {
        self.step()
    }

    fn admissible(&self, giver: ParticipantId) -> impl Iterator<Item = ParticipantId> + '_ {
        (0..self.roster.len()).filter(move |&r| {
            !self.forbidden.contains(giver, r)
                && self.partial.admits(self.policy, self.roster, giver, r)
        })
    }

    fn is_open(&self, giver: ParticipantId) -> bool {
        self.partial.receiver_of[giver].is_none()
    }

    /// Next giver to decide; `None` when everyone has a receiver.
    fn next_giver(&self) -> Option<ParticipantId> {
        match &self.giver_order {
            GiverOrder::Fixed(order) => order.iter().copied().find(|&g| self.is_open(g)),
            GiverOrder::FewestOptions => {
                // Count options without allocating, never counting past the
                // best so far, and stop at a dead end (zero options).
                let mut best: Option<(ParticipantId, usize)> = None;
                for g in (0..self.roster.len()).filter(|&g| self.is_open(g)) {
                    let bound = best.map_or(usize::MAX, |(_, count)| count);
                    let count = self.admissible(g).take(bound).count();
                    if count < bound {
                        best = Some((g, count));
                        if count == 0 {
                            break;
                        }
                    }
                }
                best.map(|(g, _)| g)
            }
        }
    }

    fn step(&mut self) -> Flow {
        self.nodes += 1;
        if self.nodes > self.budget {
            return Flow::OutOfBudget;
        }
        match self.next_giver() {
            None => {
                let receivers: Vec<ParticipantId> = self
                    .partial
                    .receiver_of
                    .iter()
                    .map(|r| r.expect("complete"))
                    .collect();
                (self.on_complete)(&receivers)
            }
            Some(giver) => {
                let mut candidates: Vec<ParticipantId> = self.admissible(giver).collect();
                (self.order)(&mut candidates);
                for receiver in candidates {
                    self.partial
                        .assign(self.policy, self.roster, giver, receiver);
                    let flow = self.step();
                    self.partial.unassign(self.roster, giver);
                    if flow != Flow::Continue {
                        return flow;
                    }
                }
                Flow::Continue
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn rng(seed: u64) -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(seed)
    }

    fn strict() -> Policy {
        Policy::strict()
    }

    #[test]
    fn solves_and_result_is_valid() {
        let r = synthetic_roster(6, &[2]);
        let s = solve(&r, &strict(), &mut rng(1), Budget::default()).unwrap();
        assert!(strict().check(&r, &s.assignment).is_ok());
        assert!(matches!(s.method, Method::Sampled { .. }));
    }

    #[test]
    fn same_seed_same_result() {
        let r = synthetic_roster(8, &[2, 1, 3]);
        let a = solve(&r, &strict(), &mut rng(42), Budget::default()).unwrap();
        let b = solve(&r, &strict(), &mut rng(42), Budget::default()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn falls_back_to_search_when_sampling_budget_is_zero() {
        let no_sampling = Budget {
            sample_attempts: 0,
            ..Budget::default()
        };
        for (n, sizes) in [(8, &[2][..]), (100, &[1, 2, 3, 4][..])] {
            let r = synthetic_roster(n, sizes);
            let s = solve(&r, &strict(), &mut rng(3), no_sampling).unwrap();
            assert_eq!(s.method, Method::Searched);
            assert!(strict().check(&r, &s.assignment).is_ok());
        }
    }

    #[test]
    fn chalcraft_shape_is_infeasible_with_explanation() {
        // Household sizes 2,1,2 — the shape of list_chalcraft.csv.
        let r = synthetic_roster(5, &[2, 1, 2]);
        let SolveError::Infeasible(d) =
            solve(&r, &strict(), &mut rng(0), Budget::default()).unwrap_err()
        else {
            panic!("expected infeasible")
        };
        assert_eq!(
            d.blocking_rules,
            vec![Rule::NoSameHousehold, Rule::HouseholdSpread]
        );
        assert_eq!(d.reasons.len(), 1, "{:?}", d.reasons);
        assert!(
            d.reasons[0]
                .contains("household \"H1\" (1 member(s)) receiving 2 gifts, which is impossible")
        );
    }

    #[test]
    fn oversized_household_is_explained() {
        let r = synthetic_roster(5, &[3, 1, 1]);
        let SolveError::Infeasible(d) =
            solve(&r, &strict(), &mut rng(0), Budget::default()).unwrap_err()
        else {
            panic!()
        };
        assert!(
            d.reasons
                .iter()
                .any(|s| s.contains("household \"H0\" has 3 of 5"))
        );
    }

    #[test]
    fn relaxed_policy_skips_prechecks_for_relaxed_rules() {
        let r = synthetic_roster(5, &[2, 1, 2]);
        let p = strict().with_limit(Rule::HouseholdSpread, 1);
        assert!(precheck(&r, &p).is_empty());
        let s = solve(&r, &p, &mut rng(0), Budget::default()).unwrap();
        assert!(p.check(&r, &s.assignment).is_ok());
    }

    #[test]
    fn count_matches_known_values() {
        let all = |n, sizes: &[usize]| {
            count_solutions(&synthetic_roster(n, sizes), &strict(), u64::MAX, u64::MAX)
        };
        assert_eq!(all(6, &[2]), Count::Exact(32));
        assert_eq!(all(3, &[1]), Count::Exact(2));
        assert_eq!(all(5, &[2, 1, 2]), Count::Exact(0));
        assert_eq!(
            count_solutions(&synthetic_roster(6, &[2]), &strict(), 10, u64::MAX),
            Count::AtLeast(10)
        );
    }

    #[test]
    fn history_exclusion_reduces_six_person_shape_without_repeats() {
        let roster = synthetic_roster(6, &[2]);
        let previous = Assignment::from_receivers(vec![2, 4, 5, 1, 3, 0], 6).unwrap();
        let mut forbidden = ForbiddenEdges::none(6);
        forbidden.extend(&previous);
        assert_eq!(
            count_solutions_avoiding(&roster, &Policy::strict(), &forbidden, u64::MAX, u64::MAX,),
            Count::Exact(10)
        );
        let mut rng = ChaCha20Rng::seed_from_u64(99);
        let solution = solve_avoiding(
            &roster,
            &Policy::strict(),
            &forbidden,
            &mut rng,
            Budget::default(),
        )
        .unwrap();
        assert!(
            solution
                .assignment
                .pairs()
                .all(|(giver, receiver)| !forbidden.contains(giver, receiver))
        );
    }

    #[test]
    fn tiny_budget_is_undetermined_not_wrong() {
        let budget = Budget {
            sample_attempts: 0,
            search_nodes: 2,
        };
        let r = synthetic_roster(7, &[1]);
        assert_eq!(
            solve(&r, &strict(), &mut rng(0), budget),
            Err(SolveError::Undetermined)
        );
    }

    #[test]
    fn large_roster_regression_for_python_identity_bug() {
        // Python's `i is not ind` broke for indices > 256.
        let r = synthetic_roster(400, &[1, 2, 3, 4]);
        let s = solve(&r, &strict(), &mut rng(7), Budget::default()).unwrap();
        assert!(strict().check(&r, &s.assignment).is_ok());
    }
}
