//! Private delivered-assignment history.
//!
//! Runs use normalized email addresses as stable participant keys so changing
//! roster row order does not erase the previous-year exclusion. History files
//! reveal pairings and must only be read/written through the private file edge.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::{Assignment, Roster, email_key, hex, name_key};
use crate::rules::Policy;
use crate::solver::ForbiddenEdges;

pub const HISTORY_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    #[default]
    Normal,
    Correction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalAssignment {
    giver: String,
    receiver: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRun {
    pub year: i32,
    pub pairing_id: String,
    pub kind: RunKind,
    pub superseded: bool,
    assignments: Vec<HistoricalAssignment>,
}

impl HistoryRun {
    pub fn delivered(
        year: i32,
        pairing_id: impl Into<String>,
        kind: RunKind,
        superseded: bool,
        roster: &Roster,
        assignment: &Assignment,
    ) -> Self {
        let assignments = assignment
            .pairs()
            .map(|(giver, receiver)| HistoricalAssignment {
                giver: roster.participant(giver).email_key(),
                receiver: roster.participant(receiver).email_key(),
            })
            .collect();
        Self {
            year,
            pairing_id: pairing_id.into(),
            kind,
            superseded,
            assignments,
        }
    }

    pub fn assignment_count(&self) -> usize {
        self.assignments.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub version: u32,
    pub runs: Vec<HistoryRun>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            version: HISTORY_VERSION,
            runs: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistoryError {
    #[error("history has unsupported version {0} (expected {HISTORY_VERSION})")]
    Version(u32),
    #[error("history already contains pairing {0}")]
    Duplicate(String),
    #[error("legacy pairing row {row} has {got} columns; expected 6")]
    LegacyWidth { row: usize, got: usize },
    #[error("legacy pairing row {0} does not match this roster")]
    LegacyRoster(usize),
    #[error("legacy pairing repeats or omits a giver or receiver")]
    LegacyPermutation,
    #[error("legacy pairing does not satisfy the current rules")]
    LegacyRules,
    #[error("cannot parse legacy pairing CSV: {0}")]
    LegacyCsv(String),
}

impl History {
    pub fn validate(&self) -> Result<(), HistoryError> {
        if self.version != HISTORY_VERSION {
            return Err(HistoryError::Version(self.version));
        }
        let mut ids = HashSet::new();
        for run in &self.runs {
            if !ids.insert(&run.pairing_id) {
                return Err(HistoryError::Duplicate(run.pairing_id.clone()));
            }
        }
        Ok(())
    }

    pub fn append(&mut self, run: HistoryRun) -> Result<(), HistoryError> {
        self.validate()?;
        if self.runs.iter().any(|r| r.pairing_id == run.pairing_id) {
            return Err(HistoryError::Duplicate(run.pairing_id));
        }
        self.runs.push(run);
        Ok(())
    }

    pub fn has_pairing(&self, pairing_id: &str) -> bool {
        self.runs.iter().any(|run| run.pairing_id == pairing_id)
    }

    pub fn mark_superseded(&mut self, pairing_id: &str) -> bool {
        self.runs
            .iter_mut()
            .find(|run| run.pairing_id == pairing_id)
            .map(|run| run.superseded = true)
            .is_some()
    }

    pub fn runs_in_year(&self, year: i32) -> usize {
        self.runs.iter().filter(|run| run.year == year).count()
    }

    pub fn latest_year(&self) -> Option<i32> {
        self.runs.iter().map(|run| run.year).max()
    }

    pub fn minimum_repeat_gap(&self) -> Option<i32> {
        // Superseded notices remain exclusions for the following year, but
        // they are not the official sequence whose cycle gap we report.
        let mut runs: Vec<&HistoryRun> = self.runs.iter().filter(|run| !run.superseded).collect();
        runs.sort_by_key(|run| run.year);
        let mut last_seen: HashMap<(&str, &str), i32> = HashMap::new();
        let mut minimum = None;
        for run in runs {
            for edge in &run.assignments {
                let key = (edge.giver.as_str(), edge.receiver.as_str());
                if let Some(previous) = last_seen.insert(key, run.year) {
                    let gap = run.year - previous;
                    minimum = Some(minimum.map_or(gap, |old: i32| old.min(gap)));
                }
            }
        }
        minimum
    }

    /// Exclusions used to create or verify a run. A normal draw avoids every
    /// delivered edge from the previous year. A correction also avoids a
    /// superseded draw from its own year.
    pub fn forbidden_for(
        &self,
        roster: &Roster,
        year: i32,
        kind: RunKind,
        skip_pairing: Option<&str>,
    ) -> ForbiddenEdges {
        let by_email: HashMap<String, usize> = roster
            .participants()
            .iter()
            .enumerate()
            .map(|(i, participant)| (participant.email_key(), i))
            .collect();
        let mut forbidden = ForbiddenEdges::none(roster.len());
        self.runs
            .iter()
            .filter(|run| Some(run.pairing_id.as_str()) != skip_pairing)
            .filter(|run| {
                run.year == year - 1
                    || (kind == RunKind::Correction && run.year == year && run.superseded)
            })
            .flat_map(|run| &run.assignments)
            .for_each(|edge| {
                if let (Some(&giver), Some(&receiver)) =
                    (by_email.get(&edge.giver), by_email.get(&edge.receiver))
                {
                    forbidden.forbid(giver, receiver);
                }
            });
        forbidden
    }

    pub fn import_legacy(
        &mut self,
        bytes: &[u8],
        roster: &Roster,
        year: i32,
    ) -> Result<HistoryRun, HistoryError> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .quote(b'|')
            .from_reader(bytes);
        let by_name: HashMap<String, usize> = roster
            .participants()
            .iter()
            .enumerate()
            .map(|(i, participant)| (participant.name_key(), i))
            .collect();
        let mut receivers = vec![None; roster.len()];
        let mut seen_receivers = HashSet::new();
        for (row, record) in reader.records().enumerate() {
            let record = record.map_err(|e| HistoryError::LegacyCsv(e.to_string()))?;
            if record.len() != 6 {
                return Err(HistoryError::LegacyWidth {
                    row: row + 1,
                    got: record.len(),
                });
            }
            let clean = |i: usize| record[i].trim().trim_matches('"');
            let giver = by_name.get(&name_key(clean(0))).copied();
            let receiver = by_name.get(&name_key(clean(3))).copied();
            let (Some(giver), Some(receiver)) = (giver, receiver) else {
                return Err(HistoryError::LegacyRoster(row + 1));
            };
            let giver_matches = roster.participant(giver).email_key() == email_key(clean(2));
            let receiver_matches = roster.participant(receiver).email_key() == email_key(clean(5));
            if !giver_matches || !receiver_matches || receivers[giver].is_some() {
                return Err(HistoryError::LegacyRoster(row + 1));
            }
            receivers[giver] = Some(receiver);
            if !seen_receivers.insert(receiver) {
                return Err(HistoryError::LegacyPermutation);
            }
        }
        let Some(receivers) = receivers.into_iter().collect::<Option<Vec<_>>>() else {
            return Err(HistoryError::LegacyPermutation);
        };
        let assignment = Assignment::from_receivers(receivers, roster.len())
            .map_err(|_| HistoryError::LegacyPermutation)?;
        Policy::strict()
            .check(roster, &assignment)
            .map_err(|_| HistoryError::LegacyRules)?;
        let id = legacy_id(year, roster, &assignment);
        let run = HistoryRun::delivered(
            year,
            format!("legacy-{id}"),
            RunKind::Normal,
            false,
            roster,
            &assignment,
        );
        self.append(run.clone())?;
        Ok(run)
    }
}

fn legacy_id(year: i32, roster: &Roster, assignment: &Assignment) -> String {
    let hasher = assignment.pairs().fold(
        Sha256::new().chain_update(year.to_le_bytes()),
        |hasher, (giver, receiver)| {
            hasher
                .chain_update(roster.participant(giver).email_key().as_bytes())
                .chain_update([0x1f])
                .chain_update(roster.participant(receiver).email_key().as_bytes())
                .chain_update([0x1e])
        },
    );
    hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::synthetic_roster;

    #[test]
    fn previous_year_edges_are_forbidden() {
        let roster = synthetic_roster(6, &[2]);
        let assignment = Assignment::from_receivers(vec![2, 4, 5, 1, 3, 0], roster.len()).unwrap();
        let run = HistoryRun::delivered(2025, "p", RunKind::Normal, false, &roster, &assignment);
        let history = History {
            version: HISTORY_VERSION,
            runs: vec![run],
        };
        let forbidden = history.forbidden_for(&roster, 2026, RunKind::Normal, None);
        assert_eq!(forbidden.count(), 6);
        assert!(assignment.pairs().all(|(g, r)| forbidden.contains(g, r)));
        assert_eq!(
            history
                .forbidden_for(&roster, 2027, RunKind::Normal, None)
                .count(),
            0
        );
    }

    #[test]
    fn correction_also_avoids_superseded_same_year_run() {
        let roster = synthetic_roster(6, &[2]);
        let assignment = Assignment::from_receivers(vec![2, 4, 5, 1, 3, 0], roster.len()).unwrap();
        let run = HistoryRun::delivered(2026, "p", RunKind::Normal, true, &roster, &assignment);
        let history = History {
            version: HISTORY_VERSION,
            runs: vec![run],
        };
        assert_eq!(
            history
                .forbidden_for(&roster, 2026, RunKind::Correction, None)
                .count(),
            6
        );
        assert_eq!(
            history
                .forbidden_for(&roster, 2026, RunKind::Normal, None)
                .count(),
            0
        );
    }
}
