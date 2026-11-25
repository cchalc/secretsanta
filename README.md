# santa 🎄

A Secret Santa pairing and emailing CLI in Rust. It replaces the original Python scripts, which are kept on the `python-legacy` branch.

Pairings are **sealed**: the organizer never sees who gives to whom unless they ask for it with `santa reveal`. Every giver gets a festive HTML email with a holiday banner and a plain-text fallback.

## Rules

1. **NoSelf**: nobody gives to themselves. This rule is *never* relaxed.
2. **NoSameHousehold**: nobody gives to someone with the same room number (partner, child, parent).
3. **NoReciprocal**: if A gives to B, then B does not give to A.
4. **HouseholdSpread**: two members of the same household never give into the same household (if A1 → B1, then not A2 → B2).

Everyone gives exactly once and receives exactly once.

### When a list can't satisfy every rule

Some rosters can't satisfy every rule. In `list_chalcraft.csv`, households of 2, 1 and 2 people mean both two-person households would have to give into the one-person household. santa then:

1. **Explains why**, in plain language and without naming anyone. For example: *households "1", "3" each have 2 members, so under the strict rules each would have to give one gift into every other household. That would mean household "2" (1 member(s)) receiving 2 gifts, which is impossible because everyone receives exactly one*.
2. **Accommodates automatically with the smallest possible change.** It allows the fewest exceptions, minimised in priority order: first NoSameHousehold, then NoReciprocal, then HouseholdSpread. For `list_chalcraft.csv` that is exactly **one HouseholdSpread exception**, and everything else stays strict.
3. **Keeps one gift per person.** Everyone still gives exactly one gift and receives exactly one gift. Only the household-spread rule bends.
4. **Still draws uniformly.** The draw is random among all pairings that use that minimal accommodation.
5. **Records the accommodation in the seal.** `verify` re-derives the minimum and rejects a seal that claims more exceptions than the roster needs.

`santa check` shows the notice and what would happen; `santa pair` applies it and prints the same notice.

## Roster format

A CSV file with a header row. Columns are matched by name, in any order. Other columns, such as `Timestamp`, are ignored.

```csv
Timestamp,Full Name,Room number,Email address
2025,Nick,1,nick@example.com
2025,Angela,1,angela@example.com
```

Whitespace is trimmed and emails are validated. Duplicate names and emails are rejected. All problems are reported together.

## Usage

```sh
just install                     # puts `santa` on your PATH (re-run after code changes)

santa check   list_holtby.csv    # validate, explain any accommodation, count valid pairings
santa history import-legacy list_holtby.csv pairs.csv --year 2025
santa pair    list_holtby.csv --year 2026    # excludes every delivered 2025 assignment
santa verify  list_holtby.csv    # re-check the seal (prints no names)
santa send    list_holtby.csv --dry-run   # every email with the receiver redacted, plus an HTML preview file
santa test-email                 # one sample email to yourself (EMAIL_HOST_USER)
santa send    list_holtby.csv    # asks you to type `send N`, then emails everyone
santa reveal  list_holtby.csv --giver Nick   # recovery only: spoils one pairing
```

### History, corrections and simulation

Completed deliveries are recorded in owner-only
`.santa/<list>/history.json`. A new annual draw excludes every giver/receiver
edge delivered in the previous calendar year. If more than one result was
delivered during a correction year, the following year avoids both notices.
Roster rows may be reordered because history matches participants by normalized
email address.

Use `santa correct <list> --year <year>` only after a disclosed run. It archives
the old result, draws a replacement in which every participant changes receiver,
and keeps only corrections that leave a feasible next-year draw. Correction
email automatically uses the one-off yeti explanation; normal sends keep the
normal template. A real correction send also requires
`--banner <yeti-image.png>` and refuses the normal artwork.

`santa simulate <list> --correction --years 5 --format json` performs the
correction and future draws in memory. Its text and JSON contain aggregate
candidate counts and QC metrics only—never assignments, names, emails or
receiver indices. `just simulate-history` logs an ordinary aggregate forecast,
and `just simulate-history correction=true` includes an active correction.
Both log through
MLflow 3.6 to the private local `.santa/mlflow.db` store. It does not contact
Databricks or use a CLI profile, REST API or credentials.

`just visualize-history` uses the MLflow CLI to export the latest correction
run and download its aggregate `simulation.json` artifact, then writes a
self-contained dashboard with inline SVG charts to
`.santa/reports/history-qc.html`. Use `correction=false` for the latest ordinary
forecast. The report is owner-only and contains no participant or pairing data.

Email options:
- `--template file.txt`: first line `Subject: ...`, then a blank line, then paragraphs separated by blank lines. Placeholders are `{giver}`, `{receiver}` and `{year}`. `{receiver}` is required in the body and forbidden in the subject. The paragraph that mentions `{receiver}` becomes the highlighted card.
- `--banner photo.jpg`: your own PNG, JPEG or GIF (up to 1 MB) instead of the built-in art.
- `--year`: the year shown in the email.
- `--from`: the sender address.
- SMTP: `--smtp-host`, `--smtp-port`, and `--smtp-security starttls|tls|none`. `none` is for a local test inbox only, and never sends credentials.

Exit codes: `0` ok, `1` error, `3` no pairing found within the search budget, `4` the sealed pairing failed verification, `5` aborted at confirmation.

### Credentials (direnv)

```sh
cp .envrc.example .envrc   # gitignored; fill in your Gmail + an App Password (no spaces)
direnv allow
```

Gmail requires an [App Password](https://support.google.com/accounts/answer/185833) when two-factor authentication is on. santa reads `EMAIL_HOST_USER` and `EMAIL_HOST_PASSWORD` from the environment and never prints them.

### Safe-send checklist

1. `just check`: fmt, clippy and the full test suite pass.
2. `santa check <list>`: read any accommodation notice.
3. `santa pair <list>`, then `santa verify <list>` reports OK.
4. `santa send <list> --dry-run`: read the previews and open the HTML preview.
5. `just mailpit` in another terminal, then `just rehearse-local`: view every test email at http://127.0.0.1:8025.
6. `just smoke-gmail`: sends one real `[TEST]` email to yourself through Gmail.
7. `santa send <list>`. If it's interrupted, run it again: people already emailed are skipped, using `.santa/<list>/ledger.json`.

Once any email has been sent, `santa pair --force` refuses to re-pair. Re-pairing then would give people conflicting assignments.

## Architecture

**Functional core, imperative shell.** All logic is pure functions over immutable data that return `Result`. Files, clock, terminal and SMTP access sit in `cli.rs`, `main.rs`, `files.rs` and `mailer.rs`. The core gets its effects as parameters: an `&mut impl Rng` and a `&dyn Mailer`.

```
roster.csv ─parse (accumulate errors)─▶ Roster ─solve_with_accommodation(Policy, rng)─▶ Pairing ─seal─▶ .santa/<list>/pairing.json
                                                                                                          │
   send: load seal ─▶ verify ─▶ plan (Template → Letter: text + HTML) ─▶ select (ledger) ─▶ deliver(&dyn Mailer)
                                                                                                          │
                                                                                 .santa/<list>/ledger.json (resumable)
```

| Module | Role |
|---|---|
| `domain` | `Roster` (validated, households interned to integers, SHA-256 fingerprint), `Assignment` (the constructor guarantees a permutation), shared email/name keys |
| `roster` | CSV parsing that accumulates every error instead of stopping at the first (the "Validated" pattern) |
| `rules` | `Rule` values with lazy violation iterators and incremental costs. `Policy` holds the rules plus an exception limit per rule, and refuses to relax NoSelf |
| `solver` | Plain-language prechecks, then uniform rejection sampling, then backtracking that finds a solution or proves there is none. Also counting and diagnosis |
| `accommodate` | Finds the lexicographically smallest relaxation using a galloping/binary search over exception limits. `Accommodation` displays as a notice with no names |
| `seal` | Sealed pairing bound to the roster fingerprint and policy. `verify` rejects tampering and non-minimal accommodations |
| `template` | Parses a template once into `Text \| Giver \| Receiver \| Year` segments and renders in a single pass, so substituted values are never re-expanded |
| `html` | Table-based layout with inline styles, an inline banner (`cid:banner`), a hidden preview line and the reveal card. Everything is HTML-escaped |
| `history` | Owner-only delivered-run log and previous-year giver/receiver exclusions, keyed by normalized email so roster reordering is safe |
| `message` | `Letter` (text + HTML) and the MIME build: `multipart/alternative[text, related[html, banner]]` |
| `mailer` | `Mailer` trait: `SmtpMailer` (pooled, with a preflight connection test) and `OutboxMailer` (rehearsal `.eml` files) |
| `send` | `plan` → `select` → `deliver`. The ledger is saved after every success, and delivery stops at the first failure |
| `files` | Owner-only (0600/0700) atomic writes for every file that could reveal a pairing |
| `cli` | clap subcommands; each is a short `?` pipeline over the core |

**Monads in Rust.** Rust has no do-notation, so `Result` and `Option` play that role: `?` is bind, alongside `and_then`, `map` and `ok_or`. Traverse is `collect::<Result<Vec<_>, _>>`. Error accumulation uses `partition`, and pipelines are iterator chains. Mutable state appears only where it is clearer, such as the backtracking `Partial` behind a pure interface and the delivery loop. It is never shared.

**Uniformity.** A Fisher–Yates shuffle is uniform over all n! permutations. Keeping only the shuffles that satisfy the policy gives a uniform draw over the valid pairings. With an accommodation, the limits are exact minima, so the valid set is exactly the optimal pairings. Both cases are checked statistically.

## Email design

![Built-in banner art](assets/banner.png)

- **Banner:** original artwork in `assets/banner.svg` (trees, presents, baubles and snow), rendered to PNG with `just banner`.
- **Layout:** 600 px fluid layout that fits phones, with an Outlook-only fixed-width wrapper. In Mailpit's client-compatibility check across 186 tests: **91.7% fully supported, 1% unsupported**. Rounded corners square off in Outlook, and the hidden preview line may show in a few clients.
- **Secrecy:** the receiver appears only in the reveal card. The subject, the inbox preview line and the HTML `<title>` never contain it.

## Testing

`just check` runs fmt, clippy (warnings as errors) and the suite. `just test` runs the suite alone: **109 tests**.

- **Brute-force oracles** (`tests/oracle.rs`), independent code that checks every permutation:
  - pairing counts on the real lists (holtby 32, testing 2, chalcraft 0);
  - the minimal accommodation (lexicographic minimum of exception counts) on 200 random rosters;
  - uniformity: χ² for strict draws, and ±15% per optimal pairing for accommodated draws.
- **Property tests:** sampling and search both return oracle-valid pairings; `Infeasible` is reported exactly when no pairing exists; prechecks never reject a feasible roster.
- **Templates and HTML:** snapshot tests; checks that escaping is applied per segment, that values are never re-expanded, and that the receiver never appears in the subject, preview line or title.
- **Send pipeline:**
  - one email per giver, naming only their receiver, in both text and HTML;
  - after a simulated outage, resuming sends no duplicates;
  - a failure to save the ledger stops the run.
- **CLI end-to-end** (`tests/cli.rs`, outbox mailer), with emails decoded by a real MIME parser:
  - accommodation;
  - detection of tampered seals and changed rosters;
  - confirmation, resume and `--only`;
  - template and banner validation;
  - owner-only file permissions.
- **SMTP** (`tests/smtp.rs`): the real `SmtpMailer` against an in-process fake SMTP server. Covers envelope recipients, MIME, a mid-run rejection followed by a resume, an unreachable server, and credentials never sent without TLS.
- **Live:**
  - `just rehearse-local` sends to a local Mailpit inbox;
  - `just smoke-gmail` sends one real email to yourself.
- **Mutation check:** 16 bugs were planted one at a time, including the original Python one, the old placeholder re-expansion, and world-readable outbox files. The suite caught every one.

## Performance (criterion, Apple Silicon, release)

| Case | Time |
|---|---|
| holtby shape (6 people, 3 households) | 1.4 µs |
| family of 10 | 2.4 µs |
| 50 / 200 / 1000 people (household sizes 1–4) | 60 µs / 161 µs / 0.77 ms |
| exhaustive search, 50 people | 0.33 ms |
| prove chalcraft infeasible / find its accommodation | 3.2 µs / 3.4 µs |
| count all pairings, family of 10 | 22 ms |
