//! Roster parsing with *error accumulation* (the "Validated" applicative):
//! instead of stopping at the first bad row, every problem in the file is
//! collected and reported together.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::io::Read;

use crate::domain::{Participant, Roster};

/// Minimum participants: with fewer than 3, "no self" + "no reciprocal" can
/// never be satisfied.
pub const MIN_PARTICIPANTS: usize = 3;

const NAME_HEADERS: &[&str] = &["full name", "name"];
const HOUSEHOLD_HEADERS: &[&str] = &["room number", "room", "household", "team"];
const EMAIL_HEADERS: &[&str] = &["email address", "email", "e-mail"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterError {
    Csv(String),
    MissingColumn(&'static str),
    EmptyField {
        line: u64,
        field: &'static str,
    },
    InvalidEmail {
        line: u64,
        value: String,
    },
    DuplicateName {
        line: u64,
        name: String,
        first_line: u64,
    },
    DuplicateEmail {
        line: u64,
        email: String,
        first_line: u64,
    },
    TooFew {
        got: usize,
    },
}

impl fmt::Display for RosterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RosterError::Csv(e) => write!(f, "CSV error: {e}"),
            RosterError::MissingColumn(c) => write!(f, "missing required column: {c}"),
            RosterError::EmptyField { line, field } => {
                write!(f, "line {line}: {field} is empty")
            }
            RosterError::InvalidEmail { line, value } => {
                write!(f, "line {line}: invalid email address {value:?}")
            }
            RosterError::DuplicateName {
                line,
                name,
                first_line,
            } => write!(
                f,
                "line {line}: duplicate name {name:?} (first seen on line {first_line})"
            ),
            RosterError::DuplicateEmail {
                line,
                email,
                first_line,
            } => write!(
                f,
                "line {line}: duplicate email {email:?} (first seen on line {first_line})"
            ),
            RosterError::TooFew { got } => write!(
                f,
                "need at least {MIN_PARTICIPANTS} participants, found {got}"
            ),
        }
    }
}

/// All errors from a failed parse, displayed one per line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct RosterErrors(pub Vec<RosterError>);

impl fmt::Display for RosterErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "roster has {} problem(s):", self.0.len())?;
        self.0.iter().try_for_each(|e| writeln!(f, "  - {e}"))
    }
}

struct Columns {
    name: usize,
    household: usize,
    email: usize,
}

/// Parse a roster CSV. The header row is required; columns are matched by
/// name (case-insensitive), so extra columns such as `Timestamp` are ignored.
pub fn parse<R: Read>(input: R) -> Result<Roster, RosterErrors> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(input);

    let headers = reader
        .headers()
        .map_err(|e| RosterErrors(vec![RosterError::Csv(e.to_string())]))?
        .clone();
    let columns = find_columns(&headers).map_err(RosterErrors)?;

    // Row-level validation: collect Ok rows and every Err.
    let (rows, row_errors): (Vec<_>, Vec<_>) = reader
        .records()
        .enumerate()
        .map(|(i, rec)| {
            let line = i as u64 + 2; // 1-based, after header
            rec.map_err(|e| vec![RosterError::Csv(e.to_string())])
                .and_then(|r| parse_row(line, &r, &columns))
        })
        .partition(Result::is_ok);

    let total = rows.len() + row_errors.len();
    let rows: Vec<(u64, Participant)> = rows.into_iter().flatten().collect();
    let errors: Vec<RosterError> = row_errors
        .into_iter()
        .filter_map(Result::err)
        .flatten()
        .chain(roster_level_errors(&rows, total))
        .collect();

    if errors.is_empty() {
        Ok(Roster::new(rows.into_iter().map(|(_, p)| p).collect()))
    } else {
        Err(RosterErrors(errors))
    }
}

fn find_columns(headers: &csv::StringRecord) -> Result<Columns, Vec<RosterError>> {
    let find = |aliases: &[&str]| {
        headers
            .iter()
            .position(|h| aliases.contains(&h.trim().to_lowercase().as_str()))
    };
    match (
        find(NAME_HEADERS),
        find(HOUSEHOLD_HEADERS),
        find(EMAIL_HEADERS),
    ) {
        (Some(name), Some(household), Some(email)) => Ok(Columns {
            name,
            household,
            email,
        }),
        (n, h, e) => Err([
            n.is_none()
                .then_some(RosterError::MissingColumn("Full Name")),
            h.is_none()
                .then_some(RosterError::MissingColumn("Room number")),
            e.is_none()
                .then_some(RosterError::MissingColumn("Email address")),
        ]
        .into_iter()
        .flatten()
        .collect()),
    }
}

fn parse_row(
    line: u64,
    record: &csv::StringRecord,
    cols: &Columns,
) -> Result<(u64, Participant), Vec<RosterError>> {
    let field = |idx: usize, label: &'static str| {
        record
            .get(idx)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(RosterError::EmptyField { line, field: label })
    };
    let name = field(cols.name, "name");
    let household = field(cols.household, "room number");
    let email = field(cols.email, "email").and_then(|raw| {
        raw.parse::<lettre::Address>()
            .map_err(|_| RosterError::InvalidEmail { line, value: raw })
    });

    // Applicative combination: succeed only if all three succeed, otherwise
    // return every error.
    match (name, household, email) {
        (Ok(name), Ok(household), Ok(email)) => Ok((
            line,
            Participant {
                name,
                household,
                email,
            },
        )),
        (n, h, e) => Err([n.err(), h.err(), e.err()].into_iter().flatten().collect()),
    }
}

/// Cross-row checks. Duplicates are checked among valid rows; the size check
/// counts every data row so a typo doesn't also produce a misleading TooFew.
fn roster_level_errors(rows: &[(u64, Participant)], total: usize) -> Vec<RosterError> {
    let dup_names = duplicates(rows, Participant::name_key).map(|(line, name, first_line)| {
        RosterError::DuplicateName {
            line,
            name,
            first_line,
        }
    });
    let dup_emails = duplicates(rows, Participant::email_key).map(|(line, email, first_line)| {
        RosterError::DuplicateEmail {
            line,
            email,
            first_line,
        }
    });
    let too_few = (total < MIN_PARTICIPANTS).then_some(RosterError::TooFew { got: total });
    dup_names.chain(dup_emails).chain(too_few).collect()
}

/// `(line, key, first_line)` for every row whose key was already seen.
fn duplicates<'a>(
    rows: &'a [(u64, Participant)],
    key: fn(&Participant) -> String,
) -> impl Iterator<Item = (u64, String, u64)> + 'a {
    let mut first_seen: HashMap<String, u64> = HashMap::new();
    rows.iter()
        .filter_map(move |(line, p)| match first_seen.entry(key(p)) {
            Entry::Occupied(e) => Some((*line, e.key().clone(), *e.get())),
            Entry::Vacant(e) => {
                e.insert(*line);
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Roster, RosterErrors> {
        parse(s.as_bytes())
    }

    const HEADER: &str = "Timestamp,Full Name,Room number,Email address\n";

    #[test]
    fn parses_python_format_and_trims_whitespace() {
        // Regression: Tristan's row in list_chalcraft.csv had a trailing space.
        let r = parse_str(&format!(
            "{HEADER}2023,Chris,1,a@x.com\n2023, Nat ,1,b@x.com \n2023,Dan,2,c@x.com\n"
        ))
        .unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r.participant(1).name, "Nat");
        assert_eq!(r.participant(1).email.to_string(), "b@x.com");
        assert_eq!(r.household_count(), 2);
    }

    #[test]
    fn names_with_commas_round_trip_through_csv_quoting() {
        // Regression: the Python writer packed all fields into one string.
        let r = parse_str(&format!(
            "{HEADER}1,\"Smith, Jane\",1,a@x.com\n1,Bob,2,b@x.com\n1,Al,3,c@x.com\n"
        ))
        .unwrap();
        assert_eq!(r.participant(0).name, "Smith, Jane");
    }

    #[test]
    fn columns_are_found_by_name_in_any_order() {
        let r = parse_str("email,name,household\na@x.com,A,1\nb@x.com,B,2\nc@x.com,C,3\n").unwrap();
        assert_eq!(r.participant(2).name, "C");
    }

    #[test]
    fn accumulates_all_errors() {
        let err = parse_str(&format!(
            "{HEADER}1,A,1,a@x.com\n1,,2,b@x.com\n1,a,3,c@x.com\n1,D,,not-an-email\n"
        ))
        .unwrap_err();
        assert_eq!(
            err.0,
            vec![
                RosterError::EmptyField {
                    line: 3,
                    field: "name"
                },
                RosterError::EmptyField {
                    line: 5,
                    field: "room number"
                },
                RosterError::InvalidEmail {
                    line: 5,
                    value: "not-an-email".into()
                },
                RosterError::DuplicateName {
                    line: 4,
                    name: "a".into(),
                    first_line: 2
                },
            ]
        );
    }

    #[test]
    fn too_few_participants() {
        assert_eq!(
            parse_str(&format!("{HEADER}1,A,1,a@x.com\n1,B,2,b@x.com\n"))
                .unwrap_err()
                .0,
            vec![RosterError::TooFew { got: 2 }]
        );
    }

    #[test]
    fn duplicate_emails_are_case_insensitive() {
        let err = parse_str(&format!(
            "{HEADER}1,A,1,a@x.com\n1,B,2,A@X.com\n1,C,3,c@x.com\n"
        ))
        .unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [RosterError::DuplicateEmail { line: 3, .. }]
        ));
    }

    #[test]
    fn missing_columns_are_all_reported() {
        let err = parse_str("Timestamp,Whatever\n1,2\n").unwrap_err();
        assert_eq!(err.0.len(), 3);
    }

    #[test]
    fn empty_file_is_an_error() {
        assert!(parse_str("").is_err());
        assert_eq!(
            parse_str(HEADER).unwrap_err().0,
            vec![RosterError::TooFew { got: 0 }]
        );
    }
}
