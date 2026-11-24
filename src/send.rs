//! The send pipeline: plan (pure) → select (pure) → deliver (effectful, but
//! the effects are injected). A ledger records every successful delivery so an
//! interrupted run can resume without emailing anyone twice.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domain::{Assignment, Participant, ParticipantId, Roster, email_key};
use crate::mailer::{MailError, Mailer};
use crate::message::{Letter, Template, render};

/// Placeholder shown instead of the receiver in previews.
pub const REDACTED: &str = "[redacted]";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The sealed pairing these deliveries belong to.
    pub pairing_id: String,
    /// Lower-cased addresses that were successfully sent to.
    pub sent: BTreeSet<String>,
}

impl Ledger {
    pub fn new(pairing_id: impl Into<String>) -> Self {
        Ledger {
            pairing_id: pairing_id.into(),
            sent: BTreeSet::new(),
        }
    }

    pub fn contains(&self, letter: &Letter) -> bool {
        self.sent.contains(&key(letter))
    }

    pub fn has_sent_any(&self) -> bool {
        !self.sent.is_empty()
    }

    /// The ledger with one more delivery recorded.
    pub fn with(mut self, letter: &Letter) -> Ledger {
        self.sent.insert(key(letter));
        self
    }
}

fn key(letter: &Letter) -> String {
    email_key(letter.to.as_ref())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub giver: ParticipantId,
    pub letter: Letter,
}

/// How the receiver's name appears in rendered letters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Real,
    Redacted,
}

/// Render one letter per giver. Pure.
pub fn plan(
    roster: &Roster,
    assignment: &Assignment,
    template: &Template,
    year: i32,
    visibility: Visibility,
) -> Vec<Outgoing> {
    assignment
        .pairs()
        .map(|(giver, receiver)| {
            let giver_p: &Participant = roster.participant(giver);
            let receiver_name = match visibility {
                Visibility::Real => roster.participant(receiver).name.as_str(),
                Visibility::Redacted => REDACTED,
            };
            Outgoing {
                giver,
                letter: render(template, giver_p, receiver_name, year),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Everyone not yet in the ledger.
    Pending,
    /// Exactly this address, even if already sent (explicit resend).
    Only(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("{0} is not a participant in this roster")]
    UnknownAddress(String),
}

/// Choose which letters to send. Pure.
pub fn select<'a>(
    outgoing: &'a [Outgoing],
    ledger: &Ledger,
    selection: &Selection,
) -> Result<Vec<&'a Outgoing>, SelectError> {
    match selection {
        Selection::Pending => Ok(outgoing
            .iter()
            .filter(|o| !ledger.contains(&o.letter))
            .collect()),
        Selection::Only(addr) => {
            let wanted = email_key(addr);
            let picked: Vec<&Outgoing> = outgoing
                .iter()
                .filter(|o| key(&o.letter) == wanted)
                .collect();
            if picked.is_empty() {
                Err(SelectError::UnknownAddress(addr.clone()))
            } else {
                Ok(picked)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("sending to {address} failed: {source}")]
    Mail { address: String, source: MailError },
    #[error("could not save the send ledger after emailing {address}: {reason}")]
    Ledger { address: String, reason: String },
}

/// Result of a delivery run: always returns the latest ledger, even on error.
#[derive(Debug)]
pub struct Delivery {
    pub ledger: Ledger,
    pub sent: Vec<String>,
    pub error: Option<SendError>,
}

/// Deliver letters in order, persisting the ledger after every success.
/// Stops at the first failure. `on_sent` is a progress callback.
pub fn deliver(
    batch: &[&Outgoing],
    mailer: &dyn Mailer,
    ledger: Ledger,
    persist: &mut dyn FnMut(&Ledger) -> Result<(), String>,
    on_sent: &mut dyn FnMut(&Letter, usize, usize),
) -> Delivery {
    let total = batch.len();
    let mut ledger = ledger;
    let mut sent = Vec::with_capacity(total);
    for (i, outgoing) in batch.iter().enumerate() {
        let letter = &outgoing.letter;
        let address = letter.to.to_string();
        if let Err(source) = mailer.deliver(letter) {
            let error = SendError::Mail { address, source };
            return Delivery {
                ledger,
                sent,
                error: Some(error),
            };
        }
        ledger = ledger.with(letter);
        sent.push(address.clone());
        on_sent(letter, i + 1, total);
        if let Err(reason) = persist(&ledger) {
            let error = SendError::Ledger { address, reason };
            return Delivery {
                ledger,
                sent,
                error: Some(error),
            };
        }
    }
    Delivery {
        ledger,
        sent,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;
    use crate::mailer::RecordingMailer;
    use crate::rules::Policy;
    use crate::solver::{Budget, solve};
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn letters(r: &Roster, a: &Assignment, visibility: Visibility) -> Vec<Outgoing> {
        plan(r, a, &Template::default_template(), 2026, visibility)
    }

    fn setup() -> (Roster, Assignment) {
        let r = synthetic_roster(6, &[2]);
        let s = solve(
            &r,
            &Policy::strict(),
            &mut ChaCha20Rng::seed_from_u64(9),
            Budget::default(),
        )
        .unwrap();
        (r, s.assignment)
    }

    fn noop_persist() -> impl FnMut(&Ledger) -> Result<(), String> {
        |_| Ok(())
    }

    #[test]
    fn everyone_gets_exactly_one_letter_naming_only_their_receiver() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Real);
        let mailer = RecordingMailer::default();
        let batch = select(&out, &Ledger::new("p"), &Selection::Pending).unwrap();
        let d = deliver(
            &batch,
            &mailer,
            Ledger::new("p"),
            &mut noop_persist(),
            &mut |_, _, _| {},
        );
        assert!(d.error.is_none());

        let sent = mailer.sent.borrow();
        assert_eq!(sent.len(), r.len());
        let recipients: BTreeSet<String> = sent.iter().map(|l| l.to.to_string()).collect();
        assert_eq!(recipients.len(), r.len(), "one letter per person");

        for letter in sent.iter() {
            let giver = r
                .participants()
                .iter()
                .position(|p| p.email == letter.to)
                .unwrap();
            let receiver = a.receiver_of(giver);
            assert!(letter.text.contains(&r.participant(receiver).name));
            assert!(letter.html.contains(&r.participant(receiver).name));
            // Leak check: no other participant's name appears, in either part.
            for (i, p) in r.participants().iter().enumerate() {
                if i != giver && i != receiver {
                    assert!(
                        !letter.text.contains(&p.name) && !letter.html.contains(&p.name),
                        "letter to {} mentions {}",
                        letter.to,
                        p.name
                    );
                }
            }
        }
    }

    #[test]
    fn redacted_plan_never_contains_receiver_names() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Redacted);
        for o in &out {
            let receiver = &r.participant(a.receiver_of(o.giver)).name;
            for part in [&o.letter.text, &o.letter.html, &o.letter.subject] {
                assert!(
                    !part.contains(receiver.as_str()),
                    "receiver leaked in preview"
                );
            }
            assert!(o.letter.text.contains(REDACTED));
            assert!(o.letter.html.contains(REDACTED));
        }
    }

    #[test]
    fn resume_after_outage_sends_no_duplicates() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Real);

        // First run fails on the 4th delivery.
        let flaky = RecordingMailer::failing_on(4);
        let mut saved = Ledger::new("p");
        let batch = select(&out, &saved, &Selection::Pending).unwrap();
        let d = deliver(
            &batch,
            &flaky,
            saved.clone(),
            &mut |l| {
                saved = l.clone();
                Ok(())
            },
            &mut |_, _, _| {},
        );
        assert!(matches!(d.error, Some(SendError::Mail { .. })));
        assert_eq!(d.sent.len(), 3);
        assert_eq!(saved.sent.len(), 3, "ledger persisted after each success");

        // Resume.
        let good = RecordingMailer::default();
        let batch = select(&out, &saved, &Selection::Pending).unwrap();
        assert_eq!(batch.len(), 3);
        let d2 = deliver(
            &batch,
            &good,
            saved.clone(),
            &mut noop_persist(),
            &mut |_, _, _| {},
        );
        assert!(d2.error.is_none());

        let mut all: Vec<String> = flaky
            .sent
            .borrow()
            .iter()
            .chain(good.sent.borrow().iter())
            .map(|l| l.to.to_string())
            .collect();
        all.sort();
        let unique: BTreeSet<_> = all.iter().cloned().collect();
        assert_eq!(all.len(), 6);
        assert_eq!(unique.len(), 6);
        assert_eq!(d2.ledger.sent.len(), 6);
    }

    #[test]
    fn fully_sent_ledger_selects_nothing() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Real);
        let ledger = out.iter().fold(Ledger::new("p"), |l, o| l.with(&o.letter));
        assert!(
            select(&out, &ledger, &Selection::Pending)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn only_selects_one_address_even_if_already_sent() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Real);
        let ledger = out.iter().fold(Ledger::new("p"), |l, o| l.with(&o.letter));
        let target = out[2].letter.to.to_string().to_uppercase();
        let picked = select(&out, &ledger, &Selection::Only(target)).unwrap();
        assert_eq!(picked.len(), 1);
        assert!(matches!(
            select(&out, &ledger, &Selection::Only("nobody@example.com".into())),
            Err(SelectError::UnknownAddress(_))
        ));
    }

    #[test]
    fn ledger_persist_failure_stops_the_run() {
        let (r, a) = setup();
        let out = letters(&r, &a, Visibility::Real);
        let mailer = RecordingMailer::default();
        let batch = select(&out, &Ledger::new("p"), &Selection::Pending).unwrap();
        let d = deliver(
            &batch,
            &mailer,
            Ledger::new("p"),
            &mut |_| Err("disk full".into()),
            &mut |_, _, _| {},
        );
        assert!(matches!(d.error, Some(SendError::Ledger { .. })));
        assert_eq!(
            mailer.sent.borrow().len(),
            1,
            "must not keep sending when the ledger cannot be saved"
        );
    }
}
