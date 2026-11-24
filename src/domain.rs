//! Core immutable domain types.
//!
//! Everything here follows *parse, don't validate*: a [`Roster`] or an
//! [`Assignment`] can only be constructed in a valid state, so the rest of the
//! program never has to re-check basic invariants.

use std::collections::HashMap;
use std::fmt;

use lettre::Address;
use sha2::{Digest, Sha256};

/// Index of a participant inside a [`Roster`] (row order of the input file).
pub type ParticipantId = usize;

/// Dense integer id for a household ("room number" in the CSV). Interned so
/// rule checks compare integers instead of strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HouseholdId(pub usize);

impl fmt::Display for HouseholdId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One person taking part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub name: String,
    pub household: String,
    pub email: Address,
}

impl Participant {
    /// Case-insensitive identity of an email address. Used for duplicate
    /// detection, the send ledger and `--only`, which must all agree.
    pub fn email_key(&self) -> String {
        email_key(self.email.as_ref())
    }

    /// Case-insensitive identity of a name.
    pub fn name_key(&self) -> String {
        name_key(&self.name)
    }
}

pub fn email_key(address: &str) -> String {
    address.trim().to_lowercase()
}

pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A validated, immutable list of participants with interned households.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roster {
    participants: Vec<Participant>,
    household_of: Vec<HouseholdId>,
    household_labels: Vec<String>,
}

impl Roster {
    /// Build a roster from already-validated participants. Households are
    /// numbered in order of first appearance. Validation of names/emails lives
    /// in [`crate::roster`]; this constructor only interns.
    pub fn new(participants: Vec<Participant>) -> Self {
        let mut ids: HashMap<&str, HouseholdId> = HashMap::new();
        let mut household_labels: Vec<String> = Vec::new();
        let household_of = participants
            .iter()
            .map(|p| {
                *ids.entry(p.household.as_str()).or_insert_with(|| {
                    household_labels.push(p.household.clone());
                    HouseholdId(household_labels.len() - 1)
                })
            })
            .collect();
        Roster {
            participants,
            household_of,
            household_labels,
        }
    }

    pub fn len(&self) -> usize {
        self.participants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    pub fn participant(&self, id: ParticipantId) -> &Participant {
        &self.participants[id]
    }

    pub fn household_of(&self, id: ParticipantId) -> HouseholdId {
        self.household_of[id]
    }

    pub fn household_count(&self) -> usize {
        self.household_labels.len()
    }

    pub fn household_label(&self, h: HouseholdId) -> &str {
        &self.household_labels[h.0]
    }

    /// Members per household, indexed by `HouseholdId.0`.
    pub fn household_sizes(&self) -> Vec<usize> {
        self.household_of
            .iter()
            .fold(vec![0; self.household_count()], |mut acc, h| {
                acc[h.0] += 1;
                acc
            })
    }

    /// Find a participant by name (case-insensitive).
    pub fn find_by_name(&self, name: &str) -> Option<ParticipantId> {
        let wanted = name_key(name);
        self.participants
            .iter()
            .position(|p| p.name_key() == wanted)
    }

    /// Stable SHA-256 fingerprint of the roster contents. Used to bind a sealed
    /// pairing to exactly the roster it was generated from.
    pub fn fingerprint(&self) -> String {
        let hasher = self.participants.iter().fold(Sha256::new(), |h, p| {
            h.chain_update(p.name.as_bytes())
                .chain_update([0x1f])
                .chain_update(p.household.as_bytes())
                .chain_update([0x1f])
                .chain_update(p.email_key().as_bytes())
                .chain_update([0x1e])
        });
        hex(&hasher.finalize())
    }
}

/// Lower-case hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A complete giver → receiver mapping. Invariant: it is a permutation of
/// `0..n` (everyone gives exactly once and receives exactly once).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Assignment(Vec<ParticipantId>);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssignmentError {
    #[error("assignment has {got} entries but roster has {expected} participants")]
    WrongLength { expected: usize, got: usize },
    #[error("participant #{0} is out of range")]
    OutOfRange(usize),
    #[error("participant #{0} would receive more than one gift")]
    DuplicateReceiver(usize),
}

impl Assignment {
    /// Smart constructor: accepts `receivers[giver] = receiver` only if it is
    /// a permutation of `0..expected_len`.
    pub fn from_receivers(
        receivers: Vec<ParticipantId>,
        expected_len: usize,
    ) -> Result<Self, AssignmentError> {
        if receivers.len() != expected_len {
            return Err(AssignmentError::WrongLength {
                expected: expected_len,
                got: receivers.len(),
            });
        }
        receivers
            .iter()
            .try_fold(vec![false; expected_len], |mut seen, &r| {
                match seen.get(r).copied() {
                    None => Err(AssignmentError::OutOfRange(r)),
                    Some(true) => Err(AssignmentError::DuplicateReceiver(r)),
                    Some(false) => {
                        seen[r] = true;
                        Ok(seen)
                    }
                }
            })
            .map(|_| Assignment(receivers))
    }

    pub fn receiver_of(&self, giver: ParticipantId) -> ParticipantId {
        self.0[giver]
    }

    pub fn receivers(&self) -> &[ParticipantId] {
        &self.0
    }

    /// `(giver, receiver)` pairs in giver order.
    pub fn pairs(&self) -> impl Iterator<Item = (ParticipantId, ParticipantId)> + '_ {
        self.0.iter().copied().enumerate()
    }
}

/// Deterministic synthetic roster for tests and benchmarks: `n` people spread
/// over households whose sizes cycle through `sizes`.
#[doc(hidden)]
pub fn synthetic_roster(n: usize, sizes: &[usize]) -> Roster {
    let households: Vec<usize> = sizes
        .iter()
        .cycle()
        .enumerate()
        .flat_map(|(h, &size)| std::iter::repeat_n(h, size.max(1)))
        .take(n)
        .collect();
    roster_from_households(&households)
}

/// Deterministic roster where person `i` lives in household `households[i]`
/// (named "Person i", household "H{n}", email person{i}@example.com).
#[doc(hidden)]
pub fn roster_from_households(households: &[usize]) -> Roster {
    let people = households
        .iter()
        .enumerate()
        .map(|(i, h)| Participant {
            name: format!("Person {i}"),
            household: format!("H{h}"),
            email: format!("person{i}@example.com")
                .parse()
                .expect("synthetic email is valid"),
        })
        .collect();
    Roster::new(people)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_accepts_permutation() {
        assert!(Assignment::from_receivers(vec![1, 2, 0], 3).is_ok());
    }

    #[test]
    fn assignment_rejects_non_permutations() {
        assert_eq!(
            Assignment::from_receivers(vec![1, 1, 0], 3),
            Err(AssignmentError::DuplicateReceiver(1))
        );
        assert_eq!(
            Assignment::from_receivers(vec![1, 5, 0], 3),
            Err(AssignmentError::OutOfRange(5))
        );
        assert_eq!(
            Assignment::from_receivers(vec![1, 0], 3),
            Err(AssignmentError::WrongLength {
                expected: 3,
                got: 2
            })
        );
    }

    #[test]
    fn households_are_interned_in_first_appearance_order() {
        let r = synthetic_roster(5, &[2, 1, 2]);
        let ids: Vec<usize> = (0..5).map(|i| r.household_of(i).0).collect();
        assert_eq!(ids, vec![0, 0, 1, 2, 2]);
        assert_eq!(r.household_sizes(), vec![2, 1, 2]);
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        let a = synthetic_roster(4, &[2]);
        let b = synthetic_roster(4, &[2]);
        let c = synthetic_roster(4, &[1]);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
        assert_eq!(a.fingerprint().len(), 64);
    }
}
