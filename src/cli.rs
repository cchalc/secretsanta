//! Imperative shell: argument parsing, files, terminal, environment. Each
//! subcommand is a short pipeline of pure core functions joined with `?`.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use lettre::message::Mailbox;
use rand::SeedableRng;
use rand::seq::IndexedRandom;
use rand_chacha::ChaCha20Rng;
use serde::Serialize;

use crate::accommodate;
use crate::backup;
use crate::domain::{Assignment, HouseholdId, Participant, Roster};
use crate::files::{read_json, read_optional_json, write_json, write_private};
use crate::history::{History, HistoryRun, RunKind};
use crate::html::BANNER_CID;
use crate::mailer::{Mailer, OutboxMailer, Security, SmtpConfig, SmtpMailer};
use crate::message::{Banner, Template, render, year_of};
use crate::roster;
use crate::rules::{ALL_RULES, Policy};
use crate::seal::{self, Sealed, VerifyError};
use crate::send::{self, Delivery, Ledger, Outgoing, Selection, Visibility};
use crate::solver::{
    self, Budget, Count, ForbiddenEdges, Method, Solution, SolutionSet, SolveError,
};

const FROM_NAME: &str = "Santa's Elf";
const EXACT_COUNT_MAX_N: usize = 12;
const COUNT_LIMIT: u64 = 1_000_000;
/// Where `santa history restore` looks for the age secret key by default.
const DEFAULT_IDENTITY: &str = ".config/santa/age-key.txt";
const CREDENTIALS_HINT: &str = "put it in .envrc (see .envrc.example) and run `direnv allow`";

#[derive(Parser, Debug)]
#[command(
    name = "santa",
    version,
    about = "Secret Santa pairing and emailing, safely."
)]
pub struct Cli {
    /// Where sealed pairings and send ledgers are stored.
    #[arg(long, global = true, default_value = ".santa")]
    pub state_dir: PathBuf,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Validate a roster; explain and preview any accommodation it needs.
    Check {
        roster: PathBuf,
        /// Calendar year whose previous delivered runs should be excluded.
        #[arg(long)]
        year: Option<i32>,
    },
    /// Draw a pairing and seal it to disk without revealing it.
    Pair {
        roster: PathBuf,
        /// Deterministic seed (testing only: anyone with the seed can recompute the pairing).
        #[arg(long)]
        seed: Option<u64>,
        /// Replace an existing sealed pairing (refused once any email has been sent).
        #[arg(long)]
        force: bool,
        /// Calendar year for this draw (defaults to the current UTC year).
        #[arg(long)]
        year: Option<i32>,
    },
    /// Replace a fully delivered pairing with a history-safe correction.
    Correct {
        roster: PathBuf,
        #[arg(long)]
        year: i32,
        /// Deterministic seed (testing only).
        #[arg(long)]
        seed: Option<u64>,
    },
    /// Simulate future draws without saving or revealing any pairing.
    Simulate {
        roster: PathBuf,
        #[arg(long, default_value_t = 5)]
        years: usize,
        #[arg(long)]
        start_year: Option<i32>,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Begin by simulating a replacement for the fully delivered active run.
        #[arg(long)]
        correction: bool,
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
    },
    /// Manage private delivered-assignment history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Re-check the sealed pairing against the roster and rules (prints no names).
    Verify { roster: PathBuf },
    /// Email every giver their receiver.
    Send {
        roster: PathBuf,
        /// Print redacted previews and write an HTML preview; send nothing.
        #[arg(long)]
        dry_run: bool,
        /// (Re)send only to this address.
        #[arg(long)]
        only: Option<String>,
        #[command(flatten)]
        mail: MailArgs,
    },
    /// Send one sample email (with a dummy receiver) to check SMTP and formatting.
    TestEmail {
        /// Recipient (defaults to EMAIL_HOST_USER, i.e. yourself).
        #[arg(long)]
        to: Option<String>,
        /// Preview the correction email: yeti copy and yeti banner.
        #[arg(long, conflicts_with = "template")]
        correction: bool,
        #[command(flatten)]
        mail: MailArgs,
    },
    /// Recovery only: show one giver's receiver. This spoils the surprise for you.
    Reveal {
        roster: PathBuf,
        #[arg(long)]
        giver: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommand {
    /// Import the original six-column pairs.csv format without printing rows.
    ImportLegacy {
        roster: PathBuf,
        pairs: PathBuf,
        #[arg(long)]
        year: i32,
    },
    /// Write the encrypted history backup (history/<list>.history.age) now.
    Backup { roster: PathBuf },
    /// Restore private history from its encrypted backup (e.g. after a wipe).
    Restore {
        roster: PathBuf,
        /// age secret key file (defaults to $SANTA_AGE_IDENTITY, then
        /// ~/.config/santa/age-key.txt).
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Only compare the backup with local history; write nothing.
        #[arg(long)]
        check: bool,
        /// Replace local history even if it differs from the backup.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Text,
    Json,
}

#[derive(Args, Debug, Clone)]
pub struct MailArgs {
    /// Email template file (first line `Subject: ...`, blank line, body).
    #[arg(long)]
    pub template: Option<PathBuf>,
    /// Year used in the template (defaults to the current year).
    #[arg(long)]
    pub year: Option<i32>,
    /// Banner image (PNG/JPEG/GIF) for the top of the email; defaults to the
    /// built-in art (the yeti for a correction, which refuses the normal art).
    #[arg(long)]
    pub banner: Option<PathBuf>,
    /// Sender address (defaults to EMAIL_HOST_USER).
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long, default_value = "smtp.gmail.com")]
    pub smtp_host: String,
    /// Defaults to 587 (starttls), 465 (tls) or 1025 (none).
    #[arg(long)]
    pub smtp_port: Option<u16>,
    #[arg(long, value_enum, default_value_t = Security::Starttls)]
    pub smtp_security: Security,
    /// Rehearsal: write .eml files to this directory instead of using SMTP.
    #[arg(long, hide = true)]
    pub outbox: Option<PathBuf>,
}

/// Everything effectful the commands may touch, injected by `main`.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub input: &'a mut dyn BufRead,
    pub now: SystemTime,
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// No pairing could be found (or ruled out within budget).
    #[error("{0}")]
    Solve(#[from] SolveError),
    #[error("{0}")]
    Verify(#[from] VerifyError),
    #[error("aborted: {0}")]
    Aborted(String),
    #[error("{0}")]
    Other(String),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Other(_) => 1,
            CliError::Solve(_) => 3,
            CliError::Verify(_) => 4,
            CliError::Aborted(_) => 5,
        }
    }
}

fn other(e: impl std::fmt::Display) -> CliError {
    CliError::Other(e.to_string())
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        other(e)
    }
}

pub fn run(cli: Cli, io: &mut Io) -> Result<(), CliError> {
    let state = cli.state_dir;
    match cli.command {
        Command::Check { roster, year } => check(&state, &roster, year, io),
        Command::Pair {
            roster,
            seed,
            force,
            year,
        } => pair(&state, &roster, seed, force, year, io),
        Command::Correct { roster, year, seed } => correct(&state, &roster, year, seed, io),
        Command::Simulate {
            roster,
            years,
            start_year,
            seed,
            correction,
            format,
        } => simulate(
            &state, &roster, years, start_year, seed, correction, format, io,
        ),
        Command::History { command } => match command {
            HistoryCommand::ImportLegacy {
                roster,
                pairs,
                year,
            } => import_legacy_history(&state, &roster, &pairs, year, io),
            HistoryCommand::Backup { roster } => backup_history(&state, &roster, io),
            HistoryCommand::Restore {
                roster,
                identity,
                check,
                force,
            } => restore_history(&state, &roster, identity, check, force, io),
        },
        Command::Verify { roster } => verify(&state, &roster, io),
        Command::Send {
            roster,
            dry_run,
            only,
            mail,
        } => send_cmd(&state, &roster, dry_run, only, &mail, io),
        Command::TestEmail {
            to,
            correction,
            mail,
        } => test_email(to, correction, &mail, io),
        Command::Reveal { roster, giver } => reveal(&state, &roster, &giver, io),
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Read a user-supplied input file, naming it in the error.
fn read_input(path: &Path, what: &str) -> Result<Vec<u8>, CliError> {
    fs::read(path).map_err(|e| other(format!("cannot read {what} {}: {e}", path.display())))
}

fn load_roster(path: &Path) -> Result<Roster, CliError> {
    roster::parse(read_input(path, "roster")?.as_slice()).map_err(other)
}

fn load_template(path: Option<&Path>) -> Result<Template, CliError> {
    let Some(path) = path else {
        return Ok(Template::default_template());
    };
    let text = String::from_utf8(read_input(path, "template")?).map_err(other)?;
    Template::parse(&text).map_err(other)
}

fn load_run_template(path: Option<&Path>, kind: RunKind) -> Result<Template, CliError> {
    match path {
        Some(path) => load_template(Some(path)),
        None if kind == RunKind::Correction => Ok(Template::correction_template()),
        None => Ok(Template::default_template()),
    }
}

/// The banner for a run of `kind`: the built-in art for that kind (trees, or
/// the yeti for a correction) unless `--banner` overrides it. A correction
/// never goes out with the normal artwork, so recipients can tell it apart.
fn banner_for(kind: RunKind, path: Option<&Path>) -> Result<Banner, CliError> {
    let banner = match path {
        None if kind == RunKind::Correction => return Ok(Banner::correction_art()),
        None => return Ok(Banner::default_art()),
        Some(path) => Banner::from_bytes(read_input(path, "banner")?).map_err(other)?,
    };
    if kind == RunKind::Correction && banner.is_default_art() {
        return Err(other(
            "refusing to send a correction with the normal artwork; omit --banner to use the built-in yeti",
        ));
    }
    Ok(banner)
}

/// Per-roster state directory, so different lists never share a pairing.
/// Encrypted history backups live in `history/` beside the state directory
/// (the repo root for the default `.santa`), which is safe to commit.
struct State {
    dir: PathBuf,
    name: String,
    backup_dir: PathBuf,
}

impl State {
    fn for_roster(state_dir: &Path, roster: &Path) -> Self {
        let name = roster
            .file_stem()
            .map_or_else(|| "roster".into(), |s| s.to_string_lossy().into_owned());
        State {
            dir: state_dir.join(&name),
            backup_dir: state_dir.parent().unwrap_or(Path::new("")).join("history"),
            name,
        }
    }

    fn recipients(&self) -> PathBuf {
        self.backup_dir.join("recipients.txt")
    }

    fn backup(&self) -> PathBuf {
        self.backup_dir.join(format!("{}.history.age", self.name))
    }

    fn pairing(&self) -> PathBuf {
        self.dir.join("pairing.json")
    }

    fn ledger(&self) -> PathBuf {
        self.dir.join("ledger.json")
    }

    fn history(&self) -> PathBuf {
        self.dir.join("history.json")
    }

    fn preview(&self) -> PathBuf {
        self.dir.join("preview")
    }

    fn load_sealed(&self) -> Result<Sealed, CliError> {
        let path = self.pairing();
        read_json(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => other(format!(
                "no sealed pairing at {}; run `santa pair` first",
                path.display()
            )),
            _ => other(format!("cannot read {}: {e}", path.display())),
        })
    }

    /// Load the seal and re-verify it against the roster.
    fn load_verified(&self, roster: &Roster) -> Result<(Sealed, Assignment), CliError> {
        let sealed = self.load_sealed()?;
        let assignment = seal::verify(
            &sealed,
            roster,
            &Policy::strict(),
            Budget::default().search_nodes,
        )?;
        if let Some(year) = sealed.year {
            let history = self.load_history()?;
            let forbidden =
                history.forbidden_for(roster, year, sealed.kind, Some(&sealed.pairing_id()));
            if assignment
                .pairs()
                .any(|(giver, receiver)| forbidden.contains(giver, receiver))
            {
                return Err(other(
                    "sealed pairing repeats a delivered assignment that this run must avoid",
                ));
            }
        }
        Ok((sealed, assignment))
    }

    fn load_history(&self) -> Result<History, CliError> {
        let history: History = read_optional_json(&self.history())
            .map_err(|e| other(format!("cannot read {}: {e}", self.history().display())))?
            .unwrap_or_default();
        history.validate().map_err(other)?;
        Ok(history)
    }

    /// Save private history and, when backups are enabled, its encrypted
    /// copy. Returns the backup path if one was written.
    fn save_history(&self, history: &History) -> Result<Option<PathBuf>, CliError> {
        history.validate().map_err(other)?;
        write_json(&self.history(), history)?;
        self.write_backup(history)
    }

    /// Encrypt `history` to `history/recipients.txt`. No recipients file
    /// means backups are off, which is not an error.
    fn write_backup(&self, history: &History) -> Result<Option<PathBuf>, CliError> {
        let text = match fs::read_to_string(self.recipients()) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(other(format!(
                    "cannot read {}: {e}",
                    self.recipients().display()
                )));
            }
        };
        let recipients = backup::parse_recipients(&text)
            .map_err(|e| other(format!("{}: {e}", self.recipients().display())))?;
        let plaintext = serde_json::to_vec_pretty(history).map_err(other)?;
        let armored = backup::encrypt(&recipients, &plaintext).map_err(other)?;
        write_private(&self.backup(), armored.as_bytes())?;
        Ok(Some(self.backup()))
    }

    fn load_ledger(&self) -> Result<Option<Ledger>, CliError> {
        read_optional_json(&self.ledger())
            .map_err(|e| other(format!("cannot read {}: {e}", self.ledger().display())))
    }

    /// The ledger for `sealed` (empty if none yet). A ledger that belongs to a
    /// different pairing is an error: its counts would be meaningless.
    fn ledger_for(&self, sealed: &Sealed) -> Result<Ledger, CliError> {
        let pairing_id = sealed.pairing_id();
        match self.load_ledger()? {
            None => Ok(Ledger::new(pairing_id)),
            Some(ledger) if ledger.pairing_id == pairing_id => Ok(ledger),
            Some(_) => Err(other(format!(
                "{} belongs to a different pairing; refusing to continue. Inspect it before deleting.",
                self.ledger().display()
            ))),
        }
    }
}

fn short(id: &str) -> &str {
    &id[..12.min(id.len())]
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn check(state_dir: &Path, path: &Path, year: Option<i32>, io: &mut Io) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let year = year.unwrap_or_else(|| year_of(io.now));
    let state = State::for_roster(state_dir, path);
    let history = state.load_history()?;
    let forbidden = history.forbidden_for(&roster, year, RunKind::Normal, None);
    let budget = Budget::default();
    let households = roster
        .household_sizes()
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{:?}×{k}", roster.household_label(HouseholdId(i))))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        io.out,
        "Roster OK: {} participants in {} households ({households})",
        roster.len(),
        roster.household_count()
    )?;
    writeln!(io.out, "Rules:")?;
    for r in ALL_RULES {
        writeln!(io.out, "  - {r}: {}", r.description())?;
    }

    // Sampling first: cheap for feasible rosters of any size. Nothing is saved.
    let mut rng: ChaCha20Rng = rand::make_rng();
    let pairing =
        accommodate::solve_with_accommodation(&roster, &Policy::strict(), &mut rng, budget)?;
    if let Some(accommodation) = pairing.accommodation() {
        write!(io.out, "{accommodation}")?;
    }
    let policy = &pairing.policy;
    solver::solve_avoiding(&roster, policy, &forbidden, &mut rng, budget)?;
    if forbidden.count() > 0 {
        writeln!(
            io.out,
            "History: avoiding {} delivered giver/receiver edges from {}.",
            forbidden.count(),
            year - 1
        )?;
    }

    if roster.len() > EXACT_COUNT_MAX_N {
        writeln!(io.out, "Feasible: valid pairings exist under: {policy}.")?;
        return Ok(());
    }
    let count = match solver::count_solutions_avoiding(
        &roster,
        policy,
        &forbidden,
        COUNT_LIMIT,
        budget.search_nodes,
    ) {
        Count::Exact(n) => format!("exactly {n}"),
        // A pairing was just found, so there is at least one.
        Count::AtLeast(n) | Count::Unknown { seen: n } => format!("at least {}", n.max(1)),
    };
    writeln!(
        io.out,
        "Feasible: {count} valid pairings exist under: {policy}."
    )?;
    Ok(())
}

fn pair(
    state_dir: &Path,
    path: &Path,
    seed: Option<u64>,
    force: bool,
    year: Option<i32>,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);
    let year = year.unwrap_or_else(|| year_of(io.now));
    let mut history = state.load_history()?;

    if state.pairing().exists() {
        let (sealed, assignment) = state.load_verified(&roster)?;
        let ledger = state.ledger_for(&sealed)?;
        if ledger.has_sent_any() {
            let can_roll_forward = ledger.sent.len() == roster.len()
                && sealed.year.is_some_and(|old_year| old_year < year);
            if !can_roll_forward {
                return Err(other(format!(
                    "{} already sent from the current pairing ({}). Re-pairing now would give people conflicting assignments; use `santa correct` for a disclosed run.",
                    plural(ledger.sent.len(), "email"),
                    state.ledger().display(),
                )));
            }
            let old_year = sealed.year.expect("checked above");
            if !history.has_pairing(&sealed.pairing_id()) {
                history
                    .append(HistoryRun::delivered(
                        old_year,
                        sealed.pairing_id(),
                        sealed.kind,
                        false,
                        &roster,
                        &assignment,
                    ))
                    .map_err(other)?;
                let backup = state.save_history(&history)?;
                note_backup(io.out, backup.as_deref(), &history)?;
            }
        } else if !force {
            return Err(other(format!(
                "a sealed pairing already exists at {}; use --force to replace it",
                state.pairing().display()
            )));
        }
    }

    let mut rng: ChaCha20Rng = match seed {
        Some(s) => {
            writeln!(
                io.err,
                "warning: --seed makes this pairing reproducible by anyone who knows the seed; use for testing only"
            )?;
            ChaCha20Rng::seed_from_u64(s)
        }
        None => rand::make_rng(),
    };
    let mut pairing = accommodate::solve_with_accommodation(
        &roster,
        &Policy::strict(),
        &mut rng,
        Budget::default(),
    )?;
    let forbidden = history.forbidden_for(&roster, year, RunKind::Normal, None);
    pairing.solution = solver::solve_avoiding(
        &roster,
        &pairing.policy,
        &forbidden,
        &mut rng,
        Budget::default(),
    )?;
    if let Some(accommodation) = pairing.accommodation() {
        write!(io.out, "{accommodation}")?;
    }

    let created = io.now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let sealed = Sealed::new(
        &roster,
        &pairing.solution,
        &pairing.policy,
        created,
        year,
        RunKind::Normal,
    );
    write_json(&state.pairing(), &sealed)?;
    // A fresh pairing starts a fresh (empty) ledger.
    write_json(&state.ledger(), &Ledger::new(sealed.pairing_id()))?;

    let method = match pairing.solution.method {
        Method::Sampled { attempts } => {
            format!(
                "uniform random draw ({})",
                plural(attempts as usize, "shuffle")
            )
        }
        Method::Searched => "randomized exhaustive search (valid, not guaranteed uniform)".into(),
    };
    writeln!(
        io.out,
        "🎄 Sealed a pairing for {} participants → {}\n   rules: {}\n   method: {method}\n   pairing id: {}\nNo names were shown. Next: `santa verify {}` then `santa send {} --dry-run`.",
        roster.len(),
        state.pairing().display(),
        pairing.policy,
        short(&sealed.pairing_id()),
        path.display(),
        path.display()
    )?;
    if forbidden.count() > 0 {
        writeln!(
            io.out,
            "   history: avoided {} delivered edges from {}",
            forbidden.count(),
            year - 1
        )?;
    }
    Ok(())
}

fn rng_from_seed(seed: Option<u64>, io: &mut Io) -> Result<ChaCha20Rng, CliError> {
    match seed {
        Some(seed) => {
            writeln!(
                io.err,
                "warning: --seed makes this pairing reproducible by anyone who knows the seed; use for testing only"
            )?;
            Ok(ChaCha20Rng::seed_from_u64(seed))
        }
        None => Ok(rand::make_rng()),
    }
}

fn correct(
    state_dir: &Path,
    path: &Path,
    year: i32,
    seed: Option<u64>,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);
    let (old_sealed, old_assignment) = state.load_verified(&roster)?;
    let ledger = state.ledger_for(&old_sealed)?;
    if ledger.sent.len() != roster.len() {
        return Err(other(format!(
            "correction requires a fully delivered pairing; only {}/{} emails were sent",
            ledger.sent.len(),
            roster.len()
        )));
    }
    if old_sealed
        .year
        .is_some_and(|sealed_year| sealed_year != year)
    {
        return Err(other("--year does not match the sealed pairing year"));
    }

    let mut history = state.load_history()?;
    if history.runs_in_year(year - 1) == 0 {
        return Err(other(format!(
            "no delivered history exists for {}; import the previous run before correcting",
            year - 1
        )));
    }
    let old_id = old_sealed.pairing_id();
    if !history.mark_superseded(&old_id) {
        history
            .append(HistoryRun::delivered(
                year,
                old_id.clone(),
                old_sealed.kind,
                true,
                &roster,
                &old_assignment,
            ))
            .map_err(other)?;
    }
    let forbidden = history.forbidden_for(&roster, year, RunKind::Correction, None);
    let candidates = match solver::collect_solutions_avoiding(
        &roster,
        &old_sealed.policy,
        &forbidden,
        COUNT_LIMIT as usize,
        Budget::default().search_nodes,
    ) {
        SolutionSet::Exact(candidates) => candidates,
        SolutionSet::AtLeast(count) => {
            return Err(other(format!(
                "correction has at least {count} candidates; raise the collection limit before drawing"
            )));
        }
        SolutionSet::Unknown(count) => {
            return Err(other(format!(
                "correction search budget ended after {count} candidates"
            )));
        }
    };
    if candidates.is_empty() {
        return Err(other(
            "no correction avoids both the previous year and the superseded draw",
        ));
    }

    let future_safe: Vec<Assignment> = candidates
        .iter()
        .filter(|candidate| {
            let mut next_year = ForbiddenEdges::none(roster.len());
            next_year.extend(&old_assignment);
            next_year.extend(candidate);
            solver::feasible_avoiding(
                &roster,
                &old_sealed.policy,
                &next_year,
                Budget::default().search_nodes,
            ) == Some(true)
        })
        .cloned()
        .collect();
    if future_safe.is_empty() {
        return Err(other(
            "correction candidates exist, but none leave a valid next-year draw that avoids both notices",
        ));
    }
    let mut rng = rng_from_seed(seed, io)?;
    let assignment = future_safe
        .choose(&mut rng)
        .expect("checked non-empty")
        .clone();
    let solution = Solution {
        assignment,
        method: Method::Sampled {
            attempts: future_safe.len() as u64,
        },
    };
    let created = io.now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let sealed = Sealed::new(
        &roster,
        &solution,
        &old_sealed.policy,
        created,
        year,
        RunKind::Correction,
    );
    let backup = state.save_history(&history)?;
    write_json(&state.pairing(), &sealed)?;
    write_json(&state.ledger(), &Ledger::new(sealed.pairing_id()))?;
    writeln!(
        io.out,
        "🏔️ Sealed a correction for {} participants.\n   {} valid corrections avoided both the previous year and the superseded draw\n   {} also preserve a next-year draw that avoids both {} notices\n   pairing id: {}\nNo names were shown and nothing was sent. Next: `santa verify {}` then `santa send {} --dry-run`.",
        roster.len(),
        candidates.len(),
        future_safe.len(),
        year,
        short(&sealed.pairing_id()),
        path.display(),
        path.display(),
    )?;
    note_backup(io.out, backup.as_deref(), &history)?;
    Ok(())
}

/// "3 runs (2025, 2026 superseded, 2026 correction)": years and kinds only.
fn runs_summary(history: &History) -> String {
    let runs = history
        .runs
        .iter()
        .map(|run| {
            let mut label = run.year.to_string();
            if run.superseded {
                label.push_str(" superseded");
            }
            if run.kind == RunKind::Correction {
                label.push_str(" correction");
            }
            label
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} ({runs})", plural(history.runs.len(), "run"))
}

fn note_backup(
    out: &mut dyn Write,
    backup: Option<&Path>,
    history: &History,
) -> Result<(), CliError> {
    if let Some(path) = backup {
        writeln!(
            out,
            "🔐 Encrypted history backup ({}) → {}; commit and push it so the history survives this laptop.",
            runs_summary(history),
            path.display()
        )?;
    }
    Ok(())
}

fn backup_history(state_dir: &Path, roster_path: &Path, io: &mut Io) -> Result<(), CliError> {
    let state = State::for_roster(state_dir, roster_path);
    let history = state.load_history()?;
    if history.runs.is_empty() {
        return Err(other(format!(
            "no delivered history at {} to back up",
            state.history().display()
        )));
    }
    let path = state.write_backup(&history)?.ok_or_else(|| {
        other(format!(
            "backups are off: {} is missing; add an age public key to it (see README)",
            state.recipients().display()
        ))
    })?;
    note_backup(io.out, Some(&path), &history)
}

fn restore_history(
    state_dir: &Path,
    roster_path: &Path,
    identity: Option<PathBuf>,
    check: bool,
    force: bool,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(roster_path)?;
    let state = State::for_roster(state_dir, roster_path);
    let identity_path = identity
        .or_else(|| (io.env)("SANTA_AGE_IDENTITY").map(PathBuf::from))
        .or_else(|| (io.env)("HOME").map(|home| PathBuf::from(home).join(DEFAULT_IDENTITY)))
        .ok_or_else(|| {
            other("pass --identity <age key file>, or set SANTA_AGE_IDENTITY to its path")
        })?;
    let identity_text = String::from_utf8(read_input(&identity_path, "identity")?)
        .map_err(|_| other("identity file is not text"))?;
    let identity = backup::parse_identity(&identity_text).map_err(other)?;
    let ciphertext = read_input(&state.backup(), "history backup")?;
    let plaintext = backup::decrypt(&identity, &ciphertext).map_err(other)?;
    let restored: History = serde_json::from_slice(&plaintext).map_err(|e| {
        other(format!(
            "history backup does not contain valid history: {e}"
        ))
    })?;
    restored.validate().map_err(other)?;
    let local: Option<History> = read_optional_json(&state.history())
        .map_err(|e| other(format!("cannot read {}: {e}", state.history().display())))?;
    let (matched, people) = restored.people_on_roster(&roster);

    if check {
        match &local {
            Some(local) if *local == restored => writeln!(
                io.out,
                "OK: backup matches local history: {}.",
                runs_summary(&restored)
            )?,
            Some(local) => writeln!(
                io.out,
                "backup differs from local history
  backup: {}
  local: {}
Run `santa history backup {}` to refresh the backup, or restore with --force to replace local history.",
                runs_summary(&restored),
                runs_summary(local),
                roster_path.display()
            )?,
            None => writeln!(
                io.out,
                "no local history; the backup holds {}.",
                runs_summary(&restored)
            )?,
        }
        writeln!(
            io.out,
            "  {matched}/{people} people in the backup are on this roster. Nothing was written."
        )?;
        return Ok(());
    }

    if let Some(local) = &local
        && *local != restored
        && !force
    {
        return Err(other(format!(
            "local history differs from the backup (local: {}; backup: {}); pass --force to replace it",
            runs_summary(local),
            runs_summary(&restored)
        )));
    }
    write_json(&state.history(), &restored)?;
    writeln!(
        io.out,
        "Restored {} into {}; {matched}/{people} people in it are on this roster. No names or pairings were shown.",
        runs_summary(&restored),
        state.history().display()
    )?;
    if matched < people {
        writeln!(
            io.out,
            "warning: {} in the history {} not on this roster (changed email address?); their past assignments no longer block repeats.",
            people - matched,
            if people - matched == 1 {
                "person is"
            } else {
                "people are"
            }
        )?;
    }
    Ok(())
}

fn import_legacy_history(
    state_dir: &Path,
    roster_path: &Path,
    pairs_path: &Path,
    year: i32,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(roster_path)?;
    let state = State::for_roster(state_dir, roster_path);
    let mut history = state.load_history()?;
    let bytes = read_input(pairs_path, "legacy pairing")?;
    let run = history
        .import_legacy(&bytes, &roster, year)
        .map_err(other)?;
    let count = run.assignment_count();
    let backup = state.save_history(&history)?;
    writeln!(
        io.out,
        "Imported {count} delivered assignments for {year} into {}. No names or pairings were shown.",
        state.history().display()
    )?;
    note_backup(io.out, backup.as_deref(), &history)?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct SimulationYear {
    year: i32,
    run_kind: &'static str,
    candidate_pairings: u64,
    candidate_count_exact: bool,
    previous_runs_avoided: usize,
    forbidden_edges: usize,
    future_safe_candidates: Option<usize>,
    previous_year_repeats: usize,
    rule_violations: usize,
}

#[derive(Debug, Serialize)]
struct SimulationReport {
    policy: &'static str,
    participant_count: usize,
    seed: u64,
    includes_correction: bool,
    simulated_years: usize,
    minimum_repeat_gap_years: Option<i32>,
    years: Vec<SimulationYear>,
}

#[allow(clippy::too_many_arguments)]
fn simulate(
    state_dir: &Path,
    path: &Path,
    years: usize,
    start_year: Option<i32>,
    seed: u64,
    correction: bool,
    format: ReportFormat,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);
    let mut history = state.load_history()?;
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let policy =
        accommodate::minimal_policy(&roster, &Policy::strict(), Budget::default().search_nodes)?;
    let mut report_years = Vec::new();
    let mut first_future_year = start_year
        .or_else(|| history.latest_year().map(|year| year + 1))
        .unwrap_or_else(|| year_of(io.now));

    if correction {
        let (active_sealed, active_assignment) = state.load_verified(&roster)?;
        let ledger = state.ledger_for(&active_sealed)?;
        let correction_year = active_sealed
            .year
            .or(start_year)
            .unwrap_or_else(|| year_of(io.now));
        if history.runs_in_year(correction_year - 1) == 0 {
            return Err(other(format!(
                "no delivered history exists for {}; import it before simulating a correction",
                correction_year - 1
            )));
        }
        if active_sealed.kind == RunKind::Normal {
            if ledger.sent.len() != roster.len() {
                return Err(other(
                    "correction simulation requires a fully delivered normal run or an already sealed correction",
                ));
            }
            let active_id = active_sealed.pairing_id();
            if !history.mark_superseded(&active_id) {
                history
                    .append(HistoryRun::delivered(
                        correction_year,
                        active_id,
                        active_sealed.kind,
                        true,
                        &roster,
                        &active_assignment,
                    ))
                    .map_err(other)?;
            }
        }
        let forbidden = history.forbidden_for(&roster, correction_year, RunKind::Correction, None);
        let candidates = match solver::collect_solutions_avoiding(
            &roster,
            &active_sealed.policy,
            &forbidden,
            COUNT_LIMIT as usize,
            Budget::default().search_nodes,
        ) {
            SolutionSet::Exact(candidates) => candidates,
            SolutionSet::AtLeast(count) | SolutionSet::Unknown(count) => {
                return Err(other(format!(
                    "correction simulation could not enumerate all candidates (saw {count})"
                )));
            }
        };
        let future_safe: Vec<Assignment> = candidates
            .iter()
            .filter(|candidate| {
                let mut next_year =
                    history.forbidden_for(&roster, correction_year + 1, RunKind::Normal, None);
                next_year.extend(candidate);
                solver::feasible_avoiding(
                    &roster,
                    &active_sealed.policy,
                    &next_year,
                    Budget::default().search_nodes,
                ) == Some(true)
            })
            .cloned()
            .collect();
        let selected = if active_sealed.kind == RunKind::Correction {
            future_safe
                .contains(&active_assignment)
                .then_some(active_assignment)
                .ok_or_else(|| other("the sealed correction is not future-safe"))?
        } else {
            future_safe
                .choose(&mut rng)
                .ok_or_else(|| other("no future-safe correction exists"))?
                .clone()
        };
        history
            .append(HistoryRun::delivered(
                correction_year,
                format!("simulation-correction-{correction_year}"),
                RunKind::Correction,
                false,
                &roster,
                &selected,
            ))
            .map_err(other)?;
        report_years.push(SimulationYear {
            year: correction_year,
            run_kind: "correction",
            candidate_pairings: candidates.len() as u64,
            candidate_count_exact: true,
            previous_runs_avoided: history.runs_in_year(correction_year - 1) + 1,
            forbidden_edges: forbidden.count(),
            future_safe_candidates: Some(future_safe.len()),
            previous_year_repeats: 0,
            rule_violations: 0,
        });
        first_future_year = correction_year + 1;
    }

    for offset in 0..years {
        let year = first_future_year + offset as i32;
        let forbidden = history.forbidden_for(&roster, year, RunKind::Normal, None);
        let count = solver::count_solutions_avoiding(
            &roster,
            &policy,
            &forbidden,
            COUNT_LIMIT,
            Budget::default().search_nodes,
        );
        let (candidate_pairings, candidate_count_exact) = match count {
            Count::Exact(count) => (count, true),
            Count::AtLeast(count) | Count::Unknown { seen: count } => (count, false),
        };
        let solution =
            solver::solve_avoiding(&roster, &policy, &forbidden, &mut rng, Budget::default())?;
        history
            .append(HistoryRun::delivered(
                year,
                format!("simulation-{year}"),
                RunKind::Normal,
                false,
                &roster,
                &solution.assignment,
            ))
            .map_err(other)?;
        report_years.push(SimulationYear {
            year,
            run_kind: "normal",
            candidate_pairings,
            candidate_count_exact,
            previous_runs_avoided: history.runs_in_year(year - 1),
            forbidden_edges: forbidden.count(),
            future_safe_candidates: None,
            previous_year_repeats: 0,
            rule_violations: 0,
        });
    }

    let report = SimulationReport {
        policy: "avoid-all-delivered-assignments-from-previous-year",
        participant_count: roster.len(),
        seed,
        includes_correction: correction,
        simulated_years: years,
        minimum_repeat_gap_years: history.minimum_repeat_gap(),
        years: report_years,
    };
    match format {
        ReportFormat::Json => {
            serde_json::to_writer_pretty(&mut *io.out, &report).map_err(other)?;
            writeln!(io.out)?;
        }
        ReportFormat::Text => print_simulation(&report, io)?,
    }
    Ok(())
}

fn print_simulation(report: &SimulationReport, io: &mut Io) -> Result<(), CliError> {
    writeln!(
        io.out,
        "Five-year history QC: {} participants, seed {}, policy: previous-year-only",
        report.participant_count, report.seed
    )?;
    for year in &report.years {
        let exact = if year.candidate_count_exact { "" } else { "+" };
        writeln!(
            io.out,
            "  {} {}: {}{exact} candidates, {} forbidden edges, 0 previous-year repeats, 0 rule violations{}",
            year.year,
            year.run_kind,
            year.candidate_pairings,
            year.forbidden_edges,
            year.future_safe_candidates
                .map_or_else(String::new, |count| format!(", {count} future-safe")),
        )?;
    }
    if let Some(gap) = report.minimum_repeat_gap_years {
        writeln!(io.out, "Minimum observed repeat gap: {gap} year(s).")?;
    }
    writeln!(
        io.out,
        "No assignments, names, email addresses, or receiver indices were shown or saved."
    )?;
    Ok(())
}

fn verify(state_dir: &Path, path: &Path, io: &mut Io) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);
    let (sealed, assignment) = state.load_verified(&roster)?;
    let ledger = state.ledger_for(&sealed)?;
    writeln!(
        io.out,
        "OK: pairing {} is a valid assignment for all {} participants.\n  everyone gives exactly once and receives exactly once\n  rules satisfied: {}\n  roster unchanged since sealing",
        short(&sealed.pairing_id()),
        roster.len(),
        sealed.policy
    )?;
    for (rule, used) in sealed.policy.exceptions_used(&roster, &assignment) {
        writeln!(
            io.out,
            "  accommodation: {} to {rule} (the minimum this roster needs)",
            plural(used, "exception")
        )?;
    }
    writeln!(
        io.out,
        "  emails sent so far: {}/{}",
        ledger.sent.len(),
        roster.len()
    )?;
    Ok(())
}

/// Sender, credentials and transport, from flags and the environment.
fn build_mailer(mail: &MailArgs, banner: Banner, io: &Io) -> Result<Box<dyn Mailer>, CliError> {
    let mailbox = |addr: &str| -> Result<Mailbox, CliError> {
        addr.parse()
            .map(|a| Mailbox::new(Some(FROM_NAME.to_owned()), a))
            .map_err(|e| other(format!("invalid sender address {addr:?}: {e}")))
    };
    let user = (io.env)("EMAIL_HOST_USER");

    if let Some(dir) = &mail.outbox {
        let from = mail.from.as_deref().unwrap_or("santa@outbox.invalid");
        return Ok(Box::new(OutboxMailer::new(
            dir.clone(),
            mailbox(from)?,
            banner,
        )));
    }

    // Credentials only ever travel over TLS.
    let credentials = match mail.smtp_security {
        Security::None => None,
        Security::Starttls | Security::Tls => {
            let missing = |var: &str| {
                other(format!(
                    "mail configuration error: {var} is not set; {CREDENTIALS_HINT}"
                ))
            };
            let user = user.clone().ok_or_else(|| missing("EMAIL_HOST_USER"))?;
            let pass =
                (io.env)("EMAIL_HOST_PASSWORD").ok_or_else(|| missing("EMAIL_HOST_PASSWORD"))?;
            Some((user, pass))
        }
    };
    let from = mail
        .from
        .clone()
        .or(user)
        .unwrap_or_else(|| "santa@localhost".into());
    let config = SmtpConfig {
        host: mail.smtp_host.clone(),
        port: mail
            .smtp_port
            .unwrap_or_else(|| mail.smtp_security.default_port()),
        security: mail.smtp_security,
        credentials,
    };
    let mailer = SmtpMailer::new(&config, mailbox(&from)?, banner).map_err(other)?;
    Ok(Box::new(mailer))
}

fn send_cmd(
    state_dir: &Path,
    path: &Path,
    dry_run: bool,
    only: Option<String>,
    mail: &MailArgs,
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);
    let (sealed, assignment) = state.load_verified(&roster)?;
    let ledger = state.ledger_for(&sealed)?;
    let banner = banner_for(sealed.kind, mail.banner.as_deref())?;
    let template = load_run_template(mail.template.as_deref(), sealed.kind)?;
    let year = match (sealed.year, mail.year) {
        (Some(sealed_year), Some(requested)) if sealed_year != requested => {
            return Err(other(format!(
                "--year {requested} does not match sealed pairing year {sealed_year}"
            )));
        }
        (Some(sealed_year), _) => sealed_year,
        (None, requested) => requested.unwrap_or_else(|| year_of(io.now)),
    };
    let selection = only.map_or(Selection::Pending, Selection::Only);

    let visibility = if dry_run {
        Visibility::Redacted
    } else {
        Visibility::Real
    };
    let outgoing = send::plan(&roster, &assignment, &template, year, visibility);
    let batch = send::select(&outgoing, &ledger, &selection).map_err(other)?;

    if dry_run {
        return print_dry_run(&batch, &ledger, &state, &banner, io);
    }
    if batch.is_empty() {
        writeln!(
            io.out,
            "Nothing to send: all {} participants have already been emailed for this pairing.",
            roster.len()
        )?;
        return Ok(());
    }
    confirm(&batch, io)?;

    let mailer = build_mailer(mail, banner, io)?;
    mailer.preflight().map_err(other)?;
    let ledger_path = state.ledger();
    let out = &mut *io.out;
    let delivery = send::deliver(
        &batch,
        mailer.as_ref(),
        ledger,
        &mut |l| write_json(&ledger_path, l).map_err(|e| e.to_string()),
        &mut |letter, i, n| {
            let _ = writeln!(out, "  sent {i}/{n} → {}", letter.to);
        },
    );
    let mut backup = None;
    if delivery.error.is_none() && delivery.ledger.sent.len() == roster.len() {
        let mut history = state.load_history()?;
        if !history.has_pairing(&sealed.pairing_id()) {
            history
                .append(HistoryRun::delivered(
                    year,
                    sealed.pairing_id(),
                    sealed.kind,
                    false,
                    &roster,
                    &assignment,
                ))
                .map_err(other)?;
            backup = state.save_history(&history)?.map(|path| (path, history));
        }
    }
    report_delivery(delivery, roster.len(), &state, io)?;
    if let Some((path, history)) = backup {
        note_backup(io.out, Some(&path), &history)?;
    }
    Ok(())
}

/// Redacted previews of every pending letter, plus an HTML preview file.
fn print_dry_run(
    batch: &[&Outgoing],
    ledger: &Ledger,
    state: &State,
    banner: &Banner,
    io: &mut Io,
) -> Result<(), CliError> {
    for Outgoing { letter, .. } in batch {
        writeln!(
            io.out,
            "----\nTo: {} <{}>\nSubject: {}\n\n{}",
            letter.to_name, letter.to, letter.subject, letter.text
        )?;
    }
    if let Some(first) = batch.first() {
        let page = write_html_preview(&state.preview(), &first.letter.html, banner)?;
        writeln!(
            io.out,
            "----\nHTML preview (receiver redacted): {}",
            page.display()
        )?;
    }
    writeln!(
        io.out,
        "DRY RUN: {} would be sent ({} already sent). Receivers are {}; nothing was sent.",
        plural(batch.len(), "message"),
        ledger.sent.len(),
        send::REDACTED
    )?;
    Ok(())
}

/// Write a browser-viewable preview (banner as a sibling file), owner-only.
fn write_html_preview(dir: &Path, html: &str, banner: &Banner) -> Result<PathBuf, CliError> {
    let banner_file = format!(
        "banner.{}",
        banner.content_type.trim_start_matches("image/")
    );
    write_private(&dir.join(&banner_file), &banner.bytes)?;
    let page = dir.join("email.html");
    let html = html.replace(&format!("cid:{BANNER_CID}"), &banner_file);
    write_private(&page, html.as_bytes())?;
    Ok(page)
}

/// List the recipients and require the organizer to type `send N`.
fn confirm(batch: &[&Outgoing], io: &mut Io) -> Result<(), CliError> {
    writeln!(io.out, "About to email {} giver(s):", batch.len())?;
    for Outgoing { letter, .. } in batch {
        writeln!(io.out, "  - {} <{}>", letter.to_name, letter.to)?;
    }
    let expected = format!("send {}", batch.len());
    write!(io.out, "Type `{expected}` to confirm: ")?;
    io.out.flush()?;
    let mut answer = String::new();
    io.input.read_line(&mut answer)?;
    if answer.trim() == expected {
        Ok(())
    } else {
        Err(CliError::Aborted(
            "confirmation did not match; nothing was sent".into(),
        ))
    }
}

fn report_delivery(
    delivery: Delivery,
    participants: usize,
    state: &State,
    io: &mut Io,
) -> Result<(), CliError> {
    match delivery.error {
        None => {
            writeln!(
                io.out,
                "🎁 Done: {} sent this run, {}/{participants} total for this pairing.",
                delivery.sent.len(),
                delivery.ledger.sent.len(),
            )?;
            Ok(())
        }
        Some(e) => Err(other(format!(
            "{e}\n{} sent before the failure and recorded in {}. Re-run `santa send` to continue; already-sent people will be skipped.",
            plural(delivery.sent.len(), "email"),
            state.ledger().display()
        ))),
    }
}

fn test_email(
    to: Option<String>,
    correction: bool,
    mail: &MailArgs,
    io: &mut Io,
) -> Result<(), CliError> {
    let to = to.or_else(|| (io.env)("EMAIL_HOST_USER")).ok_or_else(|| {
        other(format!(
            "pass --to, or set EMAIL_HOST_USER: {CREDENTIALS_HINT}"
        ))
    })?;
    let kind = if correction {
        RunKind::Correction
    } else {
        RunKind::Normal
    };
    let template = load_run_template(mail.template.as_deref(), kind)?;
    let banner = banner_for(kind, mail.banner.as_deref())?;
    let year = mail.year.unwrap_or_else(|| year_of(io.now));
    let me = Participant {
        name: "Organizer".into(),
        household: "test".into(),
        email: to
            .parse()
            .map_err(|e| other(format!("invalid recipient {to:?}: {e}")))?,
    };
    let mut letter = render(&template, &me, "Example Receiver (test)", year);
    letter.subject = format!("[TEST] {}", letter.subject);
    let mailer = build_mailer(mail, banner, io)?;
    mailer.preflight().map_err(other)?;
    mailer.deliver(&letter).map_err(other)?;
    writeln!(io.out, "✉️  Test email sent to {to}.")?;
    Ok(())
}

fn reveal(state_dir: &Path, path: &Path, giver: &str, io: &mut Io) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let (_, assignment) = State::for_roster(state_dir, path).load_verified(&roster)?;
    let g = roster
        .find_by_name(giver)
        .ok_or_else(|| other(format!("no participant named {giver:?}")))?;
    writeln!(
        io.err,
        "warning: revealing one assignment. Only do this to recover a lost email."
    )?;
    writeln!(
        io.out,
        "{} gives to {}",
        roster.participant(g).name,
        roster.participant(assignment.receiver_of(g)).name
    )?;
    Ok(())
}
