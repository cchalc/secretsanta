//! Imperative shell: argument parsing, files, terminal, environment. Each
//! subcommand is a short pipeline of pure core functions joined with `?`.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use lettre::message::Mailbox;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::accommodate;
use crate::domain::{Assignment, HouseholdId, Participant, Roster};
use crate::files::{read_json, read_optional_json, write_json, write_private};
use crate::html::BANNER_CID;
use crate::mailer::{Mailer, OutboxMailer, Security, SmtpConfig, SmtpMailer};
use crate::message::{Banner, Template, render, year_of};
use crate::roster;
use crate::rules::{ALL_RULES, Policy};
use crate::seal::{self, Sealed, VerifyError};
use crate::send::{self, Delivery, Ledger, Outgoing, Selection, Visibility};
use crate::solver::{self, Budget, Count, Method, SolveError};

const FROM_NAME: &str = "Santa's Elf";
const EXACT_COUNT_MAX_N: usize = 12;
const COUNT_LIMIT: u64 = 1_000_000;
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
    Check { roster: PathBuf },
    /// Draw a pairing and seal it to disk without revealing it.
    Pair {
        roster: PathBuf,
        /// Deterministic seed (testing only: anyone with the seed can recompute the pairing).
        #[arg(long)]
        seed: Option<u64>,
        /// Replace an existing sealed pairing (refused once any email has been sent).
        #[arg(long)]
        force: bool,
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

#[derive(Args, Debug, Clone)]
pub struct MailArgs {
    /// Email template file (first line `Subject: ...`, blank line, body).
    #[arg(long)]
    pub template: Option<PathBuf>,
    /// Year used in the template (defaults to the current year).
    #[arg(long)]
    pub year: Option<i32>,
    /// Banner image (PNG/JPEG/GIF) for the top of the email; defaults to the built-in art.
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
        Command::Check { roster } => check(&roster, io),
        Command::Pair {
            roster,
            seed,
            force,
        } => pair(&state, &roster, seed, force, io),
        Command::Verify { roster } => verify(&state, &roster, io),
        Command::Send {
            roster,
            dry_run,
            only,
            mail,
        } => send_cmd(&state, &roster, dry_run, only, &mail, io),
        Command::TestEmail { to, mail } => test_email(to, &mail, io),
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

fn load_banner(path: Option<&Path>) -> Result<Banner, CliError> {
    match path {
        None => Ok(Banner::default_art()),
        Some(path) => Banner::from_bytes(read_input(path, "banner")?).map_err(other),
    }
}

/// Per-roster state directory, so different lists never share a pairing.
struct State {
    dir: PathBuf,
}

impl State {
    fn for_roster(state_dir: &Path, roster: &Path) -> Self {
        let stem = roster
            .file_stem()
            .map_or_else(|| "roster".into(), |s| s.to_string_lossy().into_owned());
        State {
            dir: state_dir.join(stem),
        }
    }

    fn pairing(&self) -> PathBuf {
        self.dir.join("pairing.json")
    }

    fn ledger(&self) -> PathBuf {
        self.dir.join("ledger.json")
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
        Ok((sealed, assignment))
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

fn check(path: &Path, io: &mut Io) -> Result<(), CliError> {
    let roster = load_roster(path)?;
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

    if roster.len() > EXACT_COUNT_MAX_N {
        writeln!(io.out, "Feasible: valid pairings exist under: {policy}.")?;
        return Ok(());
    }
    let count = match solver::count_solutions(&roster, policy, COUNT_LIMIT, budget.search_nodes) {
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
    io: &mut Io,
) -> Result<(), CliError> {
    let roster = load_roster(path)?;
    let state = State::for_roster(state_dir, path);

    if let Some(ledger) = state.load_ledger()?.filter(Ledger::has_sent_any) {
        return Err(other(format!(
            "{} already sent from the current pairing ({}). Re-pairing now would give people conflicting assignments.\n\
             If you really mean to start over, tell everyone, then delete {} manually.",
            plural(ledger.sent.len(), "email"),
            state.ledger().display(),
            state.ledger().display()
        )));
    }
    if state.pairing().exists() && !force {
        return Err(other(format!(
            "a sealed pairing already exists at {}; use --force to replace it",
            state.pairing().display()
        )));
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
    let pairing = accommodate::solve_with_accommodation(
        &roster,
        &Policy::strict(),
        &mut rng,
        Budget::default(),
    )?;
    if let Some(accommodation) = pairing.accommodation() {
        write!(io.out, "{accommodation}")?;
    }

    let created = io.now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let sealed = Sealed::new(&roster, &pairing.solution, &pairing.policy, created);
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
fn build_mailer(mail: &MailArgs, io: &Io) -> Result<Box<dyn Mailer>, CliError> {
    let banner = load_banner(mail.banner.as_deref())?;
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
    let template = load_template(mail.template.as_deref())?;
    let year = mail.year.unwrap_or_else(|| year_of(io.now));
    let selection = only.map_or(Selection::Pending, Selection::Only);

    let visibility = if dry_run {
        Visibility::Redacted
    } else {
        Visibility::Real
    };
    let outgoing = send::plan(&roster, &assignment, &template, year, visibility);
    let batch = send::select(&outgoing, &ledger, &selection).map_err(other)?;

    if dry_run {
        return print_dry_run(&batch, &ledger, &state, mail, io);
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

    let mailer = build_mailer(mail, io)?;
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
    report_delivery(delivery, roster.len(), &state, io)
}

/// Redacted previews of every pending letter, plus an HTML preview file.
fn print_dry_run(
    batch: &[&Outgoing],
    ledger: &Ledger,
    state: &State,
    mail: &MailArgs,
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
        let banner = load_banner(mail.banner.as_deref())?;
        let page = write_html_preview(&state.preview(), &first.letter.html, &banner)?;
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

fn test_email(to: Option<String>, mail: &MailArgs, io: &mut Io) -> Result<(), CliError> {
    let to = to.or_else(|| (io.env)("EMAIL_HOST_USER")).ok_or_else(|| {
        other(format!(
            "pass --to, or set EMAIL_HOST_USER: {CREDENTIALS_HINT}"
        ))
    })?;
    let template = load_template(mail.template.as_deref())?;
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
    let mailer = build_mailer(mail, io)?;
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
