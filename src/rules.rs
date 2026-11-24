//! Pairing rules as *values*, and a [`Policy`] saying which rules apply and
//! how many exceptions each may have.
//!
//! Each [`Rule`] can (a) lazily enumerate its violations on a complete
//! assignment and (b) report the *cost* (new violations) of adding one
//! giver → receiver edge to a partial assignment, which drives the
//! backtracking search.
//!
//! Violations carry only participant *indices*, never names, so error output
//! can never leak who is giving to whom.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::{Assignment, HouseholdId, ParticipantId, Roster};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rule {
    /// Nobody gives to themselves. Never relaxed.
    NoSelf,
    /// Nobody gives to someone in their own household.
    NoSameHousehold,
    /// If A gives to B, B does not give to A.
    NoReciprocal,
    /// Two members of the same household never give into the same household
    /// (if A1 → B1 then not A2 → B2).
    HouseholdSpread,
}

/// All rules, ordered cheapest/most-selective first so rejection sampling
/// bails out as early as possible.
pub const ALL_RULES: [Rule; 4] = [
    Rule::NoSelf,
    Rule::NoSameHousehold,
    Rule::NoReciprocal,
    Rule::HouseholdSpread,
];

impl Rule {
    pub fn name(self) -> &'static str {
        match self {
            Rule::NoSelf => "NoSelf",
            Rule::NoSameHousehold => "NoSameHousehold",
            Rule::NoReciprocal => "NoReciprocal",
            Rule::HouseholdSpread => "HouseholdSpread",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Rule::NoSelf => "nobody gives a gift to themselves",
            Rule::NoSameHousehold => "nobody gives to someone in their own household",
            Rule::NoReciprocal => "if A gives to B, then B does not give to A",
            Rule::HouseholdSpread => {
                "two members of the same household never give to the same household"
            }
        }
    }

    /// What one exception to this rule means, for notices.
    pub fn exception_meaning(self) -> &'static str {
        match self {
            Rule::NoSelf => "someone may give to themselves",
            Rule::NoSameHousehold => "someone may give to a member of their own household",
            Rule::NoReciprocal => "two people may give to each other",
            Rule::HouseholdSpread => {
                "two members of one household may give into the same other household"
            }
        }
    }

    /// Whether an accommodation may ever grant exceptions to this rule.
    /// Nobody may give to themselves, whatever the roster.
    pub fn is_relaxable(self) -> bool {
        self != Rule::NoSelf
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Lazily enumerate violations. Each violation is counted once:
    /// a reciprocal *pair* is one violation, and k housemates giving into the
    /// same household are k − 1 violations.
    pub fn violations<'a>(
        self,
        roster: &'a Roster,
        receivers: &'a [ParticipantId],
    ) -> Box<dyn Iterator<Item = Violation> + 'a> {
        let pairs = receivers.iter().copied().enumerate();
        let violation = move |giver| Violation { rule: self, giver };
        let same_household =
            move |g: usize, r: usize| roster.household_of(g) == roster.household_of(r);
        match self {
            Rule::NoSelf => Box::new(
                pairs
                    .filter(|&(g, r)| g == r)
                    .map(move |(g, _)| violation(g)),
            ),
            Rule::NoSameHousehold => Box::new(
                pairs
                    .filter(move |&(g, r)| g != r && same_household(g, r))
                    .map(move |(g, _)| violation(g)),
            ),
            Rule::NoReciprocal => Box::new(
                pairs
                    .filter(move |&(g, r)| g < r && receivers[r] == g)
                    .map(move |(g, _)| violation(g)),
            ),
            // O(n): count each (giver household → receiver household) edge;
            // every repeat is a violation.
            Rule::HouseholdSpread => {
                let mut edges: HashMap<(HouseholdId, HouseholdId), u32> =
                    HashMap::with_capacity(receivers.len());
                Box::new(pairs.filter_map(move |(g, r)| {
                    let seen = edges
                        .entry((roster.household_of(g), roster.household_of(r)))
                        .or_insert(0);
                    *seen += 1;
                    (*seen > 1).then(|| violation(g))
                }))
            }
        }
    }

    /// New violations caused by adding `giver → receiver` to `partial`.
    /// Consistent with [`Rule::violations`] by construction.
    fn cost(
        self,
        roster: &Roster,
        partial: &Partial,
        giver: ParticipantId,
        receiver: ParticipantId,
    ) -> u32 {
        let same_household = roster.household_of(giver) == roster.household_of(receiver);
        let violates = match self {
            Rule::NoSelf => giver == receiver,
            Rule::NoSameHousehold => giver != receiver && same_household,
            Rule::NoReciprocal => giver != receiver && partial.receiver_of[receiver] == Some(giver),
            Rule::HouseholdSpread => {
                partial.spread_count(roster.household_of(giver), roster.household_of(receiver)) > 0
            }
        };
        u32::from(violates)
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A rule violation, identified by rule and giver *position* only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation {
    pub rule: Rule,
    pub giver: ParticipantId,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} violated by participant #{} ({})",
            self.rule,
            self.giver + 1,
            self.rule.description()
        )
    }
}

/// Which rules are enforced, and how many exceptions each allows.
/// `0` = strict; a rule that is absent is not enforced at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    limits: BTreeMap<Rule, u32>,
}

/// Placeholder limit meaning "any number of exceptions" (used while searching
/// for the smallest accommodation; never sealed).
pub const UNBOUNDED: u32 = u32::MAX;

impl Policy {
    /// All four rules, no exceptions.
    pub fn strict() -> Self {
        Policy {
            limits: ALL_RULES.iter().map(|&r| (r, 0)).collect(),
        }
    }

    /// # Panics
    /// If asked to give exceptions to a rule that is never relaxable.
    pub fn with_limit(&self, rule: Rule, limit: u32) -> Self {
        assert!(
            rule.is_relaxable() || limit == 0,
            "{rule} can never be relaxed"
        );
        let mut next = self.clone();
        next.limits.insert(rule, limit);
        next
    }

    /// # Panics
    /// If asked to drop a rule that is never relaxable.
    pub fn without(&self, rule: Rule) -> Self {
        assert!(rule.is_relaxable(), "{rule} can never be relaxed");
        let mut next = self.clone();
        next.limits.remove(&rule);
        next
    }

    pub fn limit(&self, rule: Rule) -> Option<u32> {
        self.limits.get(&rule).copied()
    }

    pub fn is_strict_about(&self, rule: Rule) -> bool {
        self.limit(rule) == Some(0)
    }

    /// Enforced rules with their limits, in [`ALL_RULES`] (check) order.
    pub fn rules(&self) -> impl Iterator<Item = (Rule, u32)> + '_ {
        ALL_RULES
            .iter()
            .filter_map(|&r| self.limit(r).map(|l| (r, l)))
    }

    /// Rules that allow at least one exception.
    pub fn exceptions(&self) -> Vec<(Rule, u32)> {
        self.rules().filter(|&(_, l)| l > 0).collect()
    }

    /// Does a complete assignment satisfy the policy? Lazy: stops as soon as
    /// any rule exceeds its limit.
    pub fn admits(&self, roster: &Roster, receivers: &[ParticipantId]) -> bool {
        self.rules().all(|(rule, limit)| {
            rule.violations(roster, receivers)
                .nth(limit as usize)
                .is_none()
        })
    }

    /// Check a complete assignment; on failure return every violation of each
    /// rule that exceeded its limit.
    pub fn check(&self, roster: &Roster, assignment: &Assignment) -> Result<(), Vec<Violation>> {
        let over: Vec<Violation> = self
            .rules()
            .map(|(rule, limit)| {
                (
                    limit,
                    rule.violations(roster, assignment.receivers())
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(limit, v)| v.len() > *limit as usize)
            .flat_map(|(_, v)| v)
            .collect();
        if over.is_empty() { Ok(()) } else { Err(over) }
    }

    /// Number of exceptions an assignment actually uses, per relaxed rule.
    pub fn exceptions_used(&self, roster: &Roster, assignment: &Assignment) -> Vec<(Rule, usize)> {
        self.exceptions()
            .into_iter()
            .map(|(rule, _)| {
                (
                    rule,
                    rule.violations(roster, assignment.receivers()).count(),
                )
            })
            .collect()
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .rules()
            .map(|(r, l)| match l {
                0 => r.name().to_owned(),
                UNBOUNDED => format!("{r} (relaxed)"),
                n => format!("{r} (≤{n} exception{})", if n == 1 { "" } else { "s" }),
            })
            .collect();
        f.write_str(&parts.join(", "))
    }
}

/// Mutable search state for backtracking, private to the solver: the
/// solver's public interface stays pure.
#[derive(Clone, Debug)]
pub(crate) struct Partial {
    pub(crate) receiver_of: Vec<Option<ParticipantId>>,
    taken: Vec<bool>,
    households: usize,
    /// Edges per (giver household, receiver household), row-major.
    spread: Vec<u32>,
    /// Exceptions used so far, indexed by `Rule::index`.
    used: [u32; ALL_RULES.len()],
    /// Cost each giver's edge contributed, to undo on backtrack.
    edge_cost: Vec<[u32; ALL_RULES.len()]>,
}

impl Partial {
    pub fn new(roster: &Roster) -> Self {
        let h = roster.household_count();
        Partial {
            receiver_of: vec![None; roster.len()],
            taken: vec![false; roster.len()],
            households: h,
            spread: vec![0; h * h],
            used: [0; ALL_RULES.len()],
            edge_cost: vec![[0; ALL_RULES.len()]; roster.len()],
        }
    }

    fn cell(&self, from: HouseholdId, to: HouseholdId) -> usize {
        from.0 * self.households + to.0
    }

    fn spread_count(&self, from: HouseholdId, to: HouseholdId) -> u32 {
        self.spread[self.cell(from, to)]
    }

    fn costs(
        &self,
        policy: &Policy,
        roster: &Roster,
        giver: ParticipantId,
        receiver: ParticipantId,
    ) -> [u32; ALL_RULES.len()] {
        policy
            .rules()
            .fold([0; ALL_RULES.len()], |mut acc, (rule, _)| {
                acc[rule.index()] = rule.cost(roster, self, giver, receiver);
                acc
            })
    }

    /// Can `giver → receiver` be added without exceeding any limit?
    pub fn admits(
        &self,
        policy: &Policy,
        roster: &Roster,
        giver: ParticipantId,
        receiver: ParticipantId,
    ) -> bool {
        !self.taken[receiver]
            && policy.rules().all(|(rule, limit)| {
                let i = rule.index();
                rule.cost(roster, self, giver, receiver) == 0 || self.used[i] < limit
            })
    }

    pub fn assign(
        &mut self,
        policy: &Policy,
        roster: &Roster,
        giver: ParticipantId,
        receiver: ParticipantId,
    ) {
        let costs = self.costs(policy, roster, giver, receiver);
        let cell = self.cell(roster.household_of(giver), roster.household_of(receiver));
        self.receiver_of[giver] = Some(receiver);
        self.taken[receiver] = true;
        self.spread[cell] += 1;
        self.used.iter_mut().zip(costs).for_each(|(u, c)| *u += c);
        self.edge_cost[giver] = costs;
    }

    pub fn unassign(&mut self, roster: &Roster, giver: ParticipantId) {
        if let Some(receiver) = self.receiver_of[giver].take() {
            let cell = self.cell(roster.household_of(giver), roster.household_of(receiver));
            self.taken[receiver] = false;
            self.spread[cell] -= 1;
            let costs = std::mem::take(&mut self.edge_cost[giver]);
            self.used.iter_mut().zip(costs).for_each(|(u, c)| *u -= c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;

    // Households: [0,0,1,1,2,2]  (people 0,1 | 2,3 | 4,5)
    fn six() -> Roster {
        synthetic_roster(6, &[2])
    }

    fn rules_violated(roster: &Roster, receivers: &[usize]) -> Vec<Rule> {
        ALL_RULES
            .iter()
            .filter(|r| r.violations(roster, receivers).next().is_some())
            .copied()
            .collect()
    }

    fn count(rule: Rule, roster: &Roster, receivers: &[usize]) -> usize {
        rule.violations(roster, receivers).count()
    }

    #[test]
    fn valid_assignment_passes_all_rules() {
        // 0→2, 1→4, 2→5, 3→1, 4→3, 5→0. Household edges: (0,1),(0,2),(1,2),(1,0),(2,1),(2,0).
        let rec = [2, 4, 5, 1, 3, 0];
        assert_eq!(rules_violated(&six(), &rec), vec![]);
        assert!(Policy::strict().admits(&six(), &rec));
    }

    #[test]
    fn no_self() {
        assert!(rules_violated(&six(), &[0, 2, 3, 4, 5, 1]).contains(&Rule::NoSelf));
    }

    #[test]
    fn no_same_household() {
        // 0→1, 2→3, 4→5 are all within a household.
        assert_eq!(count(Rule::NoSameHousehold, &six(), &[1, 2, 3, 4, 5, 0]), 3);
    }

    #[test]
    fn reciprocal_pairs_count_once() {
        let rec = [2, 4, 0, 5, 1, 3]; // 0↔2, 1↔4, 3↔5
        assert_eq!(count(Rule::NoReciprocal, &six(), &rec), 3);
        assert!(!rules_violated(&six(), &rec).contains(&Rule::NoSelf));
    }

    #[test]
    fn household_spread_counts_repeats() {
        let rec = [2, 3, 4, 5, 0, 1]; // each household gives both gifts into one household
        assert_eq!(rules_violated(&six(), &rec), vec![Rule::HouseholdSpread]);
        assert_eq!(count(Rule::HouseholdSpread, &six(), &rec), 3);
    }

    #[test]
    fn policy_limits_allow_exactly_that_many_exceptions() {
        let rec = [2, 3, 4, 5, 0, 1]; // 3 HouseholdSpread violations
        let r = six();
        assert!(!Policy::strict().admits(&r, &rec));
        assert!(
            !Policy::strict()
                .with_limit(Rule::HouseholdSpread, 2)
                .admits(&r, &rec)
        );
        assert!(
            Policy::strict()
                .with_limit(Rule::HouseholdSpread, 3)
                .admits(&r, &rec)
        );
        assert!(
            Policy::strict()
                .without(Rule::HouseholdSpread)
                .admits(&r, &rec)
        );
        let a = Assignment::from_receivers(rec.to_vec(), 6).unwrap();
        assert_eq!(Policy::strict().check(&r, &a).unwrap_err().len(), 3);
        let relaxed = Policy::strict().with_limit(Rule::HouseholdSpread, 3);
        assert_eq!(
            relaxed.exceptions_used(&r, &a),
            vec![(Rule::HouseholdSpread, 3)]
        );
    }

    #[test]
    #[should_panic(expected = "NoSelf can never be relaxed")]
    fn no_self_can_never_be_relaxed() {
        let _ = Policy::strict().with_limit(Rule::NoSelf, 1);
    }

    #[test]
    fn only_no_self_is_unrelaxable() {
        let fixed: Vec<Rule> = ALL_RULES
            .into_iter()
            .filter(|r| !r.is_relaxable())
            .collect();
        assert_eq!(fixed, vec![Rule::NoSelf]);
    }

    #[test]
    fn violation_display_contains_no_names() {
        let s = Violation {
            rule: Rule::NoSelf,
            giver: 3,
        }
        .to_string();
        assert!(!s.contains("Person"));
        assert!(s.contains("#4"));
    }

    #[test]
    fn partial_enforces_limits_incrementally() {
        let r = six();
        let strict = Policy::strict();
        let mut p = Partial::new(&r);
        p.assign(&strict, &r, 0, 2);
        assert!(
            !p.admits(&strict, &r, 1, 3),
            "housemate into same household"
        );
        assert!(!p.admits(&strict, &r, 2, 0), "reciprocal");
        assert!(!p.admits(&strict, &r, 4, 2), "already taken");
        assert!(p.admits(&strict, &r, 1, 4));

        // One HouseholdSpread exception: 1→3 repeats household edge 0→1 and uses it.
        let one_spread = strict.with_limit(Rule::HouseholdSpread, 1);
        assert!(p.admits(&one_spread, &r, 1, 3));
        p.assign(&one_spread, &r, 1, 3);
        // 2→4 is a fresh edge (household 1→2): free.
        assert!(p.admits(&one_spread, &r, 2, 4));
        p.assign(&one_spread, &r, 2, 4);
        // 3→5 would repeat household edge 1→2: the single exception is used up.
        assert!(!p.admits(&one_spread, &r, 3, 5));
        // Other rules stay strict under the relaxed policy.
        // 3→1: receiver free, fresh household edge 1→0, but reciprocates 1→3.
        assert!(
            !p.admits(&one_spread, &r, 3, 1),
            "reciprocal still forbidden"
        );

        // Undo restores the budget.
        p.unassign(&r, 1);
        assert!(p.admits(&one_spread, &r, 3, 5));
        p.unassign(&r, 2);
        p.unassign(&r, 0);
        assert!(p.admits(&strict, &r, 1, 3));
    }
}
