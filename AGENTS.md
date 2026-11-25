# AGENTS.md — santa

Guide for anyone (human or agent) working in this repo. `README.md` covers
*using* santa; this file covers *developing* it.

`santa` is a Rust CLI that pairs a Secret Santa roster and emails each giver.
It is a pure functional core (`domain`, `roster`, `rules`, `solver`,
`accommodate`, `seal`, `template`, `html`, `message`, `send`) inside a thin
imperative shell (`cli`, `main`, `files`, `mailer`). See the README's
Architecture section for the module map.

## Ground rules (non-negotiable)

1. **Never reveal or commit a pairing.** Sealed pairings, ledgers, previews and
   rehearsal emails live in `.santa/` and `outbox/`, both gitignored. Never
   print, paste, log or `git add` them. The only pairing data that may be
   committed is the age-**encrypted** history backup `history/*.history.age`
   (plus the public keys in `history/recipients.txt`). Never commit plaintext
   history, and never read, print or commit the secret key
   (`~/.config/santa/age-key.txt`). Don't run `santa reveal` unless the
   organizer explicitly asks. Violations and diagnostics carry participant
   *indices*, never names. Keep it that way.
2. **Never send real email without an explicit go-ahead** for that specific
   send. Rehearse against Mailpit first (`just rehearse-local`).
3. **Secrets stay in `.envrc`** (gitignored). Never read, echo or commit
   `EMAIL_HOST_PASSWORD`. To check one, test its *shape* (length, has spaces)
   without printing it.
4. **The roster CSVs contain real family names and emails**, and the GitHub repo
   is public. Don't add more real data, and don't paste rows into issues, PRs or
   logs. Tests use synthetic people (`domain::synthetic_roster`,
   `tests/common`).
5. **No Homebrew.** Tools come from nix (`nix run nixpkgs#<tool>`), cargo or
   just.

## Environment

| Tool | How it's used here |
|---|---|
| **direnv** | Loads `.envrc` (`EMAIL_HOST_USER`, `EMAIL_HOST_PASSWORD`). Start from `.envrc.example`, then `direnv allow`. Non-interactive shells and agents: `direnv exec . <cmd>`. |
| **devenv** | Not used. The project is small and the nix-provided Rust toolchain (cargo/rustc 1.95) is enough. If the toolchain ever needs pinning, add a `devenv.nix` and load it from `.envrc` with `use devenv`. |
| **nix** | Rust toolchain; one-off tools via `nix run`: `mailpit` (local inbox), `resvg` (banner art). |
| **just** | Task runner; see below. |

## Just recipes

| Recipe | What it does |
|---|---|
| `just check` | `fmt --check` + clippy with `-D warnings` + full test suite. **Run before every commit.** |
| `just test` | `cargo test`: unit, brute-force oracle, property, uniformity, CLI end-to-end, fake-SMTP |
| `just lint` / `just fmt-check` | clippy / rustfmt alone |
| `just bench` | criterion benchmarks (`benches/solve.rs`); update the README table if numbers move |
| `just build` / `just install` | release build / install `santa` onto PATH (re-run after code changes) |
| `just banner` | re-render `assets/banner.svg` → `banner.png` and `assets/yeti.svg` → `yeti.png` |
| `just mailpit` | local SMTP inbox on :1025, web UI at http://127.0.0.1:8025 |
| `just smoke-local` | one sample email into Mailpit |
| `just rehearse-local [list]` | full pair + send of a list into Mailpit (throwaway state dir) |
| `just smoke-gmail` | one real `[TEST]` email to `EMAIL_HOST_USER` via Gmail |
| `just rehearse-correction` | synthetic correction run (temp state, example.com) with its 6 emails into Mailpit |
| `just smoke-gmail-correction` | one real `[TEST]` correction email (yeti copy + banner) to `EMAIL_HOST_USER` |

## Version control: jj (colocated with git)

The repo is a **colocated jj repo**: `.jj/` and `.git/` share history, so jj
and git both work, and GitHub only ever sees git.

```sh
jj st                                   # what changed
jj log                                  # graph; trunk() = master@origin
jj describe -m "feat(send): ..."        # describe the working-copy change
jj new                                  # start the next change on top
jj commit -m "fix(html): ..."           # describe @ and start a new change in one step
jj squash                               # fold @ into its parent
jj bookmark set <branch> -r @-          # move a branch to the last finished change
jj git push -b <branch>                 # push (git remote: origin)
jj git fetch && jj rebase -d 'trunk()'  # update onto master
```

- **Commit messages** follow the repo's commit-msg hook:
  - header `type(scope): lowercase imperative description`, at most 50 chars;
  - `type` is one of `feat fix docs style refactor perf test chore`;
  - blank line, then a body with `Problem:` / `Solution:` paragraphs, wrapped at 72.
- **Sign everything.** Git signs with SSH (`commit.gpgsign=true`). jj is set
  repo-locally to the same key (`signing.behavior = "own"`, `backend = "ssh"`).
- **jj does not run git hooks.** The commit-msg linter and the secret scan only
  run on `git commit`. When using jj, follow the message format yourself, run
  `just check`, and make sure nothing secret is in the change (`jj diff`)
  before pushing. For changes near `.envrc`, `.gitignore` or roster files,
  prefer a plain `git commit` so the secret scan runs.
- **The commit-msg hook reads `/dev/tty`.** From a shell without a terminal (an
  agent), run `script -q /dev/null git commit -S -F <msgfile>` so the hooks run.
  Never skip them with `--no-verify`.
- **Branches:**
  - `master`: default.
  - `rust-cli`: the Rust rewrite.
  - `python-legacy`: frozen reference copy of the original Python scripts. Don't change it.

## Parallel work: worktrunk (`wt`)

worktrunk manages **git worktrees**, one directory per branch, for running
several agents or experiments side by side. jj stays in the primary checkout;
inside a `wt` worktree use git (or `jj workspace add` if you want jj-native
parallel checkouts instead).

```sh
wt switch --create <branch>     # new worktree + branch (sibling dir secretsanta.<branch>)
wt switch <branch>              # jump to an existing worktree
wt list                         # all worktrees and their status
wt merge                        # from inside a worktree: merge into master (runs pre-merge hooks)
wt remove <branch>              # delete the worktree once merged
```

Project hooks live in `.config/wt.toml`:
- `pre-start` symlinks the primary checkout's `.envrc` into the new worktree and
  runs `direnv allow`.
- `pre-merge` runs `just check`.

The organizer must approve the hooks once with `wt config approvals add`.
Agents must not approve hooks on the organizer's behalf (no `--yes`).
Agents creating worktrees from a non-interactive session should use
`wt switch --create <branch> --no-cd`.

## Code style

- **Functional core, imperative shell.** Core functions are pure: they take
  data and return `Result`. Randomness (`&mut impl Rng`), the clock and
  delivery (`&dyn Mailer`) are passed in. All I/O goes in `cli.rs`,
  `files.rs` and `mailer.rs`.
- **Readability beats cleverness.** Use `?`, `map`/`and_then`, iterator
  pipelines and `collect::<Result<_, _>>()` where they read naturally. A plain
  `for` loop or early `return` is better than a contorted fold. If a combinator
  chain needs a comment to decode, rewrite it.
- **Parse, don't validate.** `Roster`, `Assignment`, `Template` and `Policy`
  can only be built in a valid state. Add invariants to the constructor, not to
  callers.
- **Fix things at the right depth.** One source of truth per concept:
  - `Rule::is_relaxable`;
  - `domain::email_key` / `name_key`;
  - template placeholders in `template.rs`;
  - private writes in `files.rs`.
- **Errors:** `thiserror` enums in the core and `CliError` in the shell, with
  exit codes 1/3/4/5. User data appears only where the user typed it (roster
  line numbers, file paths), never receiver names.
- **Owner-only files:** anything that could reveal a pairing goes through
  `files::write_private` / `write_json` (0600 files, 0700 directories).
- **Performance:** the hot paths are `solver::sample` (rejection sampling) and
  the backtracking `Search`. Measure with `just bench` before and after
  changing them.

## Testing expectations

- **Red/green:** write the failing test first, especially for bugs.
- **Oracles stay independent.** `tests/oracle.rs` brute-forces every
  permutation with code that shares nothing with `santa::rules`. Don't make it
  call the code it checks.
- **Leak checks:** any new output path (CLI text, email part, preview, error)
  gets a test asserting it contains no receiver names.
- **Snapshots** (insta) live in `src/snapshots/`. After an intentional change,
  review the diff, then accept with `INSTA_UPDATE=always cargo test`.
- **`tests/oracle.proptest-regressions`** stores seeds of past failures;
  commit it.
- **Integration tests** share `tests/common/mod.rs` (`Env`, `NAMES`,
  `roster_csv`). Real SMTP is never used in tests: use `--outbox` or the
  in-process fake server in `tests/smtp.rs`.
- **Mutation check before risky merges:** plant the bug, confirm a test fails,
  restore. Every safety property should have a test that bites.
- **Before any real send:**
  1. `just check`;
  2. `santa check`, `pair`, `verify`, `send --dry-run`;
  3. `just rehearse-local` (for a correction: `just rehearse-correction`);
  4. `just smoke-gmail` (for a correction: `just smoke-gmail-correction`);
  5. then the real `santa send`, typing `send N` at the prompt.
