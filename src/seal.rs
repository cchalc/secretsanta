//! Sealed pairings: the assignment is stored as participant *indices* bound to
//! a fingerprint of the exact roster it was generated from. Nothing here ever
//! prints names; `verify` re-checks everything without revealing anything.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::accommodate::minimal_policy;
use crate::domain::{Assignment, AssignmentError, Roster, hex};
use crate::rules::{Policy, Violation};
use crate::solver::{Method, Solution};

/// v2 added the policy (accommodations) to the seal.
pub const SEAL_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub version: u32,
    pub roster_fingerprint: String,
    pub created_unix: u64,
    /// "uniform-sample" or "search".
    pub method: String,
    /// The rules (and any accommodation) the pairing was drawn under.
    pub policy: Policy,
    /// `receivers[giver] = receiver`, by roster row order.
    pub receivers: Vec<usize>,
}

impl Sealed {
    pub fn new(roster: &Roster, solution: &Solution, policy: &Policy, created_unix: u64) -> Self {
        Sealed {
            version: SEAL_VERSION,
            roster_fingerprint: roster.fingerprint(),
            created_unix,
            method: match solution.method {
                Method::Sampled { .. } => "uniform-sample".into(),
                Method::Searched => "search".into(),
            },
            policy: policy.clone(),
            receivers: solution.assignment.receivers().to_vec(),
        }
    }

    /// Identifies this exact pairing (used to bind the send ledger to it).
    pub fn pairing_id(&self) -> String {
        let h = self.receivers.iter().fold(
            Sha256::new().chain_update(self.roster_fingerprint.as_bytes()),
            |h, r| h.chain_update((*r as u64).to_le_bytes()),
        );
        hex(&h.finalize())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(
        "sealed pairing has unsupported version {0} (expected {SEAL_VERSION}); re-run `santa pair`"
    )]
    Version(u32),
    #[error(
        "the roster has changed since the pairing was sealed (fingerprint mismatch); re-run `santa pair` before sending"
    )]
    RosterMismatch,
    #[error("sealed pairing is corrupt: {0}")]
    Invalid(#[from] AssignmentError),
    #[error(
        "sealed pairing claims an accommodation ({sealed}) but the smallest one this roster needs is ({expected})"
    )]
    PolicyNotMinimal { sealed: Policy, expected: Policy },
    #[error("could not confirm the accommodation is minimal (search budget exhausted)")]
    Undetermined,
    #[error("sealed pairing breaks the rules:\n{}", .0.iter().map(|v| format!("  - {v}")).collect::<Vec<_>>().join("\n"))]
    Violations(Vec<Violation>),
}

/// Re-validate a sealed pairing against a roster and the base policy.
///
/// If the seal carries an accommodation, it must be exactly the minimal one
/// for this roster, so a hand-edited seal cannot grant itself extra exceptions.
pub fn verify(
    sealed: &Sealed,
    roster: &Roster,
    base: &Policy,
    node_budget: u64,
) -> Result<Assignment, VerifyError> {
    if sealed.version != SEAL_VERSION {
        return Err(VerifyError::Version(sealed.version));
    }
    if sealed.roster_fingerprint != roster.fingerprint() {
        return Err(VerifyError::RosterMismatch);
    }
    if sealed.policy != *base {
        let expected =
            minimal_policy(roster, base, node_budget).map_err(|_| VerifyError::Undetermined)?;
        if sealed.policy != expected {
            return Err(VerifyError::PolicyNotMinimal {
                sealed: sealed.policy.clone(),
                expected,
            });
        }
    }
    let assignment = Assignment::from_receivers(sealed.receivers.clone(), roster.len())?;
    sealed
        .policy
        .check(roster, &assignment)
        .map_err(VerifyError::Violations)?;
    Ok(assignment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;
    use crate::rules::Rule;
    use crate::solver::{Budget, solve};
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn sealed_for(roster: &Roster, policy: Policy, receivers: Vec<usize>) -> Sealed {
        Sealed {
            version: SEAL_VERSION,
            roster_fingerprint: roster.fingerprint(),
            created_unix: 0,
            method: "uniform-sample".into(),
            policy,
            receivers,
        }
    }

    fn strict_seal(roster: &Roster, receivers: Vec<usize>) -> Sealed {
        sealed_for(roster, Policy::strict(), receivers)
    }

    fn check(s: &Sealed, r: &Roster) -> Result<Assignment, VerifyError> {
        verify(s, r, &Policy::strict(), u64::MAX)
    }

    #[test]
    fn verify_accepts_valid() {
        let r = synthetic_roster(6, &[2]);
        assert!(check(&strict_seal(&r, vec![2, 4, 5, 1, 3, 0]), &r).is_ok());
    }

    #[test]
    fn verify_rejects_changed_roster() {
        let r = synthetic_roster(6, &[2]);
        let other = synthetic_roster(6, &[3]);
        assert!(matches!(
            check(&strict_seal(&r, vec![2, 4, 5, 1, 3, 0]), &other),
            Err(VerifyError::RosterMismatch)
        ));
    }

    #[test]
    fn verify_rejects_tampered_assignment() {
        let r = synthetic_roster(6, &[2]);
        assert!(matches!(
            check(&strict_seal(&r, vec![2, 2, 5, 1, 3, 0]), &r),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            check(&strict_seal(&r, vec![2, 3, 4, 5, 0, 1]), &r),
            Err(VerifyError::Violations(_))
        ));
    }

    #[test]
    fn verify_accepts_only_the_minimal_accommodation() {
        // Chalcraft shape: minimal accommodation is one HouseholdSpread exception.
        let r = synthetic_roster(5, &[2, 1, 2]);
        let one = Policy::strict().with_limit(Rule::HouseholdSpread, 1);
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        let s = solve(&r, &one, &mut rng, Budget::default()).unwrap();
        let receivers = s.assignment.receivers().to_vec();
        assert!(check(&sealed_for(&r, one, receivers.clone()), &r).is_ok());

        // A seal granting itself more exceptions than needed is rejected.
        let greedy = Policy::strict().with_limit(Rule::HouseholdSpread, 2);
        assert!(matches!(
            check(&sealed_for(&r, greedy, receivers.clone()), &r),
            Err(VerifyError::PolicyNotMinimal { .. })
        ));
        // A feasible roster must not carry an accommodation at all.
        let six = synthetic_roster(6, &[2]);
        let relaxed = Policy::strict().with_limit(Rule::HouseholdSpread, 1);
        assert!(matches!(
            check(&sealed_for(&six, relaxed, vec![2, 4, 5, 1, 3, 0]), &six),
            Err(VerifyError::PolicyNotMinimal { .. })
        ));
    }

    #[test]
    fn pairing_id_depends_on_assignment() {
        let r = synthetic_roster(6, &[2]);
        assert_ne!(
            strict_seal(&r, vec![2, 4, 5, 1, 3, 0]).pairing_id(),
            strict_seal(&r, vec![4, 2, 0, 5, 1, 3]).pairing_id()
        );
    }
}
