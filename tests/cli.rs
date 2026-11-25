//! End-to-end tests of the `santa` binary. Real SMTP is never used: sends go
//! to the hidden `--outbox` rehearsal mailer.

use std::fs;
use std::path::PathBuf;

use mail_parser::MimeHeaders;
use predicates::prelude::*;

mod common;
use common::{Env, NAMES, address, roster_csv, stdout_of};

/// In output made of `----`-separated previews, each block may only mention
/// its own giver (the `To:` line): never anyone else.
fn assert_previews_only_mention_their_giver(out: &str) {
    for block in out.split("----\n").filter(|b| b.contains("To: ")) {
        let giver = block
            .split("To: ")
            .nth(1)
            .unwrap()
            .split(" <")
            .next()
            .unwrap();
        for n in NAMES.iter().filter(|n| **n != giver) {
            assert!(
                !block.contains(n),
                "preview for {giver} leaks {n}:\n{block}"
            );
        }
    }
}

/// A delivered email, decoded with a real MIME parser.
struct Email {
    subject: String,
    text: String,
    html: String,
    inline_images: usize,
    /// Bytes of the inline `cid:banner` image (empty if none).
    banner: Vec<u8>,
}

/// Built-in artwork, read from the same files the binary embeds.
const TREES_ART: &[u8] = include_bytes!("../assets/banner.png");
const YETI_ART: &[u8] = include_bytes!("../assets/yeti.png");

fn parse_eml(path: &std::path::Path) -> Email {
    let bytes = fs::read(path).unwrap();
    let msg = mail_parser::MessageParser::default()
        .parse(&bytes)
        .expect("valid MIME message");
    Email {
        subject: msg.subject().unwrap_or_default().to_owned(),
        text: msg.body_text(0).expect("text/plain part").into_owned(),
        html: msg.body_html(0).expect("text/html part").into_owned(),
        inline_images: msg
            .attachments()
            .filter(|a| a.content_id() == Some("banner"))
            .count(),
        banner: msg
            .attachments()
            .find(|a| a.content_id() == Some("banner"))
            .map(|a| a.contents().to_vec())
            .unwrap_or_default(),
    }
}

/// Deliver a 2025 and a 2026 run, then seal a 2026 correction for `family.csv`.
fn seal_correction(e: &Env) {
    for (year, seed) in [("2025", "21"), ("2026", "22")] {
        e.santa()
            .args(["pair", "family.csv", "--year", year, "--seed", seed])
            .assert()
            .success();
        e.santa()
            .args(["send", "family.csv", "--outbox", "outbox"])
            .write_stdin("send 6\n")
            .assert()
            .success();
    }
    e.santa()
        .args(["correct", "family.csv", "--year", "2026", "--seed", "23"])
        .assert()
        .success();
}

const CHALCRAFT_SHAPE: &str = "Timestamp,Full Name,Room number,Email address\n1,Annabel,1,annabel@x.com\n1,Barnaby,1,barnaby@x.com\n1,Cosima,2,cosima@x.com\n1,Desmond,3,desmond@x.com\n1,Evangeline,3,evangeline@x.com\n";

// ------------------------------------------------------------------ check ---

#[test]
fn check_reports_exact_count() {
    let e = Env::new();
    e.santa()
        .args(["check", "family.csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains("6 participants in 3 households"))
        .stdout(predicate::str::contains("exactly 32 valid pairings"));
}

#[test]
fn infeasible_roster_is_explained_and_accommodated() {
    let e = Env::new();
    fs::write(e.path("tight.csv"), CHALCRAFT_SHAPE).unwrap();
    e.santa()
        .args(["check", "tight.csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cannot satisfy every rule"))
        .stdout(predicate::str::contains(
            "receiving 2 gifts, which is impossible",
        ))
        .stdout(predicate::str::contains(
            "Everyone still gives exactly one gift and receives exactly one gift",
        ))
        .stdout(predicate::str::contains(
            "HouseholdSpread: 1 exception allowed",
        ))
        .stdout(predicate::str::contains("Every other rule stays strict"))
        .stdout(predicate::str::contains("HouseholdSpread (≤1 exception)"));

    let out = stdout_of(e.santa().args(["pair", "tight.csv"]));
    assert!(
        out.contains("HouseholdSpread: 1 exception allowed"),
        "{out}"
    );
    assert!(out.contains("Sealed a pairing for 5 participants"));
    for n in ["Annabel", "Barnaby", "Cosima", "Desmond", "Evangeline"] {
        assert!(!out.contains(n), "pair output must not contain names");
    }
    e.santa()
        .args(["verify", "tight.csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "accommodation: 1 exception to HouseholdSpread",
        ));
}

#[test]
fn verify_rejects_a_seal_that_grants_itself_extra_exceptions() {
    let e = Env::new();
    fs::write(e.path("tight.csv"), CHALCRAFT_SHAPE).unwrap();
    e.santa().args(["pair", "tight.csv"]).assert().success();
    let p = e.path(".santa/tight/pairing.json");
    let mut sealed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    sealed["policy"]["limits"]["NoReciprocal"] = serde_json::json!(3);
    fs::write(&p, sealed.to_string()).unwrap();
    e.santa()
        .args(["verify", "tight.csv"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("smallest one this roster needs"));
}

#[test]
fn check_reports_all_roster_errors() {
    let e = Env::new();
    fs::write(
        e.path("typos.csv"),
        "Full Name,Room number,Email address\nA,1,nope\nA,2,b@x.com\n,3,c@x.com\n",
    )
    .unwrap();
    e.santa()
        .args(["check", "typos.csv"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("invalid email address \"nope\""))
        .stderr(predicate::str::contains("line 4: name is empty"));
}

// ------------------------------------------------------- pair and verify ----

#[test]
fn pair_seals_without_revealing_and_verify_passes() {
    let e = Env::new();
    let out = stdout_of(e.santa().args(["pair", "family.csv"]));
    assert!(out.contains("Sealed a pairing for 6 participants"));
    assert!(
        NAMES.iter().all(|n| !out.contains(n)),
        "pair must not print names:\n{out}"
    );

    let sealed = e.path(".santa/family/pairing.json");
    assert!(sealed.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&sealed).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let v = stdout_of(e.santa().args(["verify", "family.csv"]));
    assert!(v.contains("OK: pairing"));
    assert!(v.contains("emails sent so far: 0/6"));
    assert!(
        NAMES.iter().all(|n| !v.contains(n)),
        "verify must not print names:\n{v}"
    );
}

#[test]
fn pair_refuses_to_overwrite_without_force() {
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["pair", "family.csv"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("already exists"));
    e.santa()
        .args(["pair", "family.csv", "--force"])
        .assert()
        .success();
}

#[test]
fn verify_detects_roster_changes_and_tampering() {
    let e = Env::new();
    e.pair();
    // Change one email → fingerprint mismatch.
    fs::write(
        e.path("family.csv"),
        roster_csv().replace("bob@", "robert@"),
    )
    .unwrap();
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("roster has changed"));

    // Restore roster, tamper with the assignment.
    fs::write(e.path("family.csv"), roster_csv()).unwrap();
    let p = e.path(".santa/family/pairing.json");
    let mut sealed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    sealed["receivers"] = serde_json::json!([0, 2, 3, 4, 5, 1]); // Alice → Alice
    fs::write(&p, sealed.to_string()).unwrap();
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("NoSelf"));
    e.santa()
        .args(["send", "family.csv", "--dry-run"])
        .assert()
        .code(4);
}

#[test]
fn a_ledger_from_another_pairing_is_refused() {
    let e = Env::new();
    e.pair();
    fs::write(
        e.path(".santa/family/ledger.json"),
        r#"{"pairing_id":"someone-else","sent":["alice@example.com"]}"#,
    )
    .unwrap();
    for args in [
        vec!["verify", "family.csv"],
        vec!["send", "family.csv", "--dry-run"],
    ] {
        e.santa()
            .args(args)
            .assert()
            .code(1)
            .stderr(predicate::str::contains("belongs to a different pairing"));
    }
}

#[test]
fn verify_without_pairing_is_an_error() {
    let e = Env::new();
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("run `santa pair` first"));
}

// ------------------------------------------------------------------- send ---

#[test]
fn dry_run_previews_everyone_redacted_and_sends_nothing() {
    let e = Env::new();
    e.pair();
    let out = stdout_of(e.santa().args([
        "send",
        "family.csv",
        "--dry-run",
        "--year",
        "2026",
        "--outbox",
        "outbox",
    ]));
    assert_eq!(out.matches("\nTo: ").count(), 6);
    assert_eq!(out.matches("🎁 [redacted] 🎁").count(), 6);
    assert!(out.contains("Subject: 🎄 Your 2026 Family Secret Santa assignment"));
    assert!(out.contains("DRY RUN: 6 messages would be sent"));
    assert_previews_only_mention_their_giver(&out);
    assert!(e.outbox_files().is_empty());

    // HTML preview: redacted, with the banner beside it.
    let page = e.path(".santa/family/preview/email.html");
    let html = fs::read_to_string(&page).unwrap();
    assert!(out.contains("HTML preview (receiver redacted)"));
    assert!(html.contains("[redacted]"));
    assert!(html.contains("src=\"banner.png\""));
    assert!(e.path(".santa/family/preview/banner.png").exists());
    let giver = NAMES
        .iter()
        .find(|n| html.contains(&format!("Ho ho ho, {n}!")))
        .unwrap();
    assert!(
        NAMES
            .iter()
            .filter(|n| *n != giver)
            .all(|n| !html.contains(n))
    );
}

#[test]
fn full_send_delivers_the_sealed_pairing_exactly_once() {
    let e = Env::new();
    e.pair();
    let out = stdout_of(
        e.santa()
            .args(["send", "family.csv", "--outbox", "outbox", "--year", "2026"])
            .write_stdin("send 6\n"),
    );
    assert!(out.contains("Done: 6 sent this run, 6/6 total"), "{out}");
    for n in NAMES {
        assert!(
            !out.contains(&format!("🎁 {n} 🎁")),
            "send output leaks {n}"
        );
    }

    // Each giver got one well-formed email naming exactly the sealed receiver.
    let receivers = e.receivers();
    let files = e.outbox_files();
    assert_eq!(files.len(), 6);
    for (giver, name) in NAMES.iter().enumerate() {
        let addr = address(name);
        let mine: Vec<&PathBuf> = files
            .iter()
            .filter(|f| f.to_string_lossy().ends_with(&format!("{addr}.eml")))
            .collect();
        assert_eq!(mine.len(), 1, "{addr} should get exactly one email");
        let email = parse_eml(mine[0]);
        let expected = NAMES[receivers[giver]];
        assert_eq!(email.subject, "🎄 Your 2026 Family Secret Santa assignment");
        assert!(
            email.text.contains(&format!("🎁 {expected} 🎁")),
            "{}",
            email.text
        );
        assert!(email.text.contains(&format!("Ho ho ho, {name}!")));
        assert!(
            email.html.contains(&format!(">{expected}</span>")),
            "receiver highlighted in HTML"
        );
        assert!(email.html.contains("cid:banner"));
        assert_eq!(email.inline_images, 1, "banner attached inline");
        for (i, other) in NAMES.iter().enumerate() {
            if i != giver && i != receivers[giver] {
                assert!(
                    !email.text.contains(other) && !email.html.contains(other),
                    "{addr}'s email mentions {other}"
                );
            }
        }
    }

    // Re-running sends nothing.
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to send"));
    assert_eq!(e.outbox_files().len(), 6);

    // Verify reflects progress; re-pairing is now refused even with --force.
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains("emails sent so far: 6/6"));
    e.santa()
        .args(["pair", "family.csv", "--force"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("conflicting assignments"));
}

#[test]
fn custom_banner_is_used_and_validated() {
    let e = Env::new();
    e.pair();
    fs::write(e.path("not-an-image.txt"), "hello").unwrap();
    e.santa()
        .args([
            "send",
            "family.csv",
            "--dry-run",
            "--banner",
            "not-an-image.txt",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("PNG, JPEG or GIF"));
    fs::write(e.path("photo.jpg"), [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]).unwrap();
    e.santa()
        .args([
            "test-email",
            "--to",
            "me@example.com",
            "--outbox",
            "outbox",
            "--banner",
            "photo.jpg",
        ])
        .assert()
        .success();
    let eml = fs::read_to_string(&e.outbox_files()[0]).unwrap();
    assert!(eml.contains("Content-Type: image/jpeg"));
}

#[cfg(unix)]
#[test]
fn files_that_reveal_pairings_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--dry-run"])
        .assert()
        .success();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .write_stdin("send 6\n")
        .assert()
        .success();
    assert_eq!(mode(&e.path("outbox")), 0o700);
    for eml in e.outbox_files() {
        assert_eq!(mode(&eml), 0o600, "{}", eml.display());
    }
    for f in [
        ".santa/family/pairing.json",
        ".santa/family/ledger.json",
        ".santa/family/history.json",
        ".santa/family/preview/email.html",
    ] {
        assert_eq!(mode(&e.path(f)), 0o600, "{f}");
    }
}

#[test]
fn wrong_confirmation_aborts_without_sending() {
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .write_stdin("yes\n")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("nothing was sent"));
    assert!(e.outbox_files().is_empty());
}

#[test]
fn only_resends_to_a_single_person() {
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .write_stdin("send 6\n")
        .assert()
        .success();
    e.santa()
        .args([
            "send",
            "family.csv",
            "--outbox",
            "outbox",
            "--only",
            "CAROL@example.com",
        ])
        .write_stdin("send 1\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("About to email 1 giver(s)"));
    assert_eq!(e.outbox_files().len(), 7);
    e.santa()
        .args([
            "send",
            "family.csv",
            "--outbox",
            "outbox",
            "--only",
            "zed@example.com",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not a participant"));
}

#[test]
fn smtp_without_credentials_fails_before_sending() {
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv"])
        .write_stdin("send 6\n")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("EMAIL_HOST_USER is not set"));
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .stdout(predicate::str::contains("emails sent so far: 0/6"));
}

#[test]
fn custom_template_is_validated() {
    let e = Env::new();
    e.santa()
        .args(["pair", "family.csv", "--year", "2027"])
        .assert()
        .success();
    fs::write(
        e.path("bad.txt"),
        "Subject: Gift for {receiver}\n\nHi {giver}, {receiver}\n",
    )
    .unwrap();
    e.santa()
        .args(["send", "family.csv", "--dry-run", "--template", "bad.txt"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "subject must not contain {receiver}",
        ));
    fs::write(
        e.path("ok.txt"),
        "Subject: Office swap {year}\n\nHi {giver}, you have {receiver}.\n",
    )
    .unwrap();
    e.santa()
        .args([
            "send",
            "family.csv",
            "--dry-run",
            "--template",
            "ok.txt",
            "--year",
            "2027",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Subject: Office swap 2027"))
        .stdout(predicate::str::contains("you have [redacted]."));
    let html = fs::read_to_string(e.path(".santa/family/preview/email.html")).unwrap();
    assert!(html.contains("Secret Santa 2027"));
}

#[test]
fn completed_year_is_archived_and_next_year_never_repeats() {
    let e = Env::new();
    e.santa()
        .args(["pair", "family.csv", "--year", "2025", "--seed", "11"])
        .assert()
        .success();
    let previous = e.receivers();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox", "--year", "2025"])
        .write_stdin("send 6\n")
        .assert()
        .success();
    e.santa()
        .args(["pair", "family.csv", "--year", "2026", "--seed", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("avoided 6 delivered edges"));
    let current = e.receivers();
    assert!(
        previous
            .iter()
            .zip(current)
            .all(|(previous, current)| *previous != current)
    );
    assert!(e.path(".santa/family/history.json").exists());
}

#[test]
fn correction_changes_every_assignment_and_uses_correction_copy() {
    let e = Env::new();
    e.santa()
        .args(["pair", "family.csv", "--year", "2025", "--seed", "21"])
        .assert()
        .success();
    let first = e.receivers();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .write_stdin("send 6\n")
        .assert()
        .success();
    e.santa()
        .args(["pair", "family.csv", "--year", "2026", "--seed", "22"])
        .assert()
        .success();
    let superseded = e.receivers();
    e.santa()
        .args(["send", "family.csv", "--outbox", "outbox"])
        .write_stdin("send 6\n")
        .assert()
        .success();

    let simulation = stdout_of(e.santa().args([
        "simulate",
        "family.csv",
        "--correction",
        "--years",
        "5",
        "--seed",
        "23",
        "--format",
        "json",
    ]));
    assert!(simulation.contains("\"includes_correction\": true"));
    assert!(simulation.contains("\"previous_year_repeats\": 0"));
    assert!(NAMES.iter().all(|name| !simulation.contains(name)));
    assert!(!simulation.contains("@example.com"));

    e.santa()
        .args(["correct", "family.csv", "--year", "2026", "--seed", "23"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No names were shown and nothing was sent",
        ));
    let corrected = e.receivers();
    assert!(
        corrected
            .iter()
            .enumerate()
            .all(|(giver, receiver)| *receiver != first[giver] && *receiver != superseded[giver])
    );
    e.santa()
        .args([
            "simulate",
            "family.csv",
            "--correction",
            "--years",
            "5",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"includes_correction\": true"));
    e.santa()
        .args(["send", "family.csv", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("horrible yeti"))
        .stdout(predicate::str::contains("pieces of paper in a hat"));

    // The real send uses the built-in yeti art without any --banner flag.
    e.santa()
        .args(["send", "family.csv", "--outbox", "corrected"])
        .write_stdin("send 6\n")
        .assert()
        .success();
    let mut files: Vec<PathBuf> = fs::read_dir(e.path("corrected"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 6);
    for (giver, name) in NAMES.iter().enumerate() {
        let addr = address(name);
        let file = files
            .iter()
            .find(|f| f.to_string_lossy().ends_with(&format!("{addr}.eml")))
            .unwrap();
        let email = parse_eml(file);
        assert_eq!(
            email.subject,
            "🏔️ Corrected 2026 Family Secret Santa assignment"
        );
        assert!(email.text.contains("horrible yeti"));
        assert!(email.html.contains("horrible yeti"));
        assert_eq!(email.inline_images, 1);
        assert!(email.banner == YETI_ART, "correction uses the yeti banner");
        let expected = NAMES[corrected[giver]];
        assert!(email.text.contains(&format!("🎁 {expected} 🎁")));
        for (i, other) in NAMES.iter().enumerate() {
            if i != giver && i != corrected[giver] {
                assert!(
                    !email.text.contains(other) && !email.html.contains(other),
                    "{addr}'s correction email mentions {other}"
                );
            }
        }
    }
}

#[test]
fn correction_refuses_the_normal_artwork() {
    let e = Env::new();
    seal_correction(&e);
    fs::write(e.path("trees.png"), TREES_ART).unwrap();
    for args in [
        &[
            "send",
            "family.csv",
            "--outbox",
            "corrected",
            "--banner",
            "trees.png",
        ][..],
        &["send", "family.csv", "--dry-run", "--banner", "trees.png"][..],
    ] {
        e.santa()
            .args(args)
            .write_stdin("send 6\n")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("normal artwork"));
    }
    assert!(!e.path("corrected").exists(), "nothing was sent");

    // A different custom image is still allowed for a correction.
    fs::write(e.path("photo.jpg"), [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]).unwrap();
    e.santa()
        .args(["send", "family.csv", "--dry-run", "--banner", "photo.jpg"])
        .assert()
        .success();
}

#[test]
fn normal_send_still_accepts_the_tree_artwork() {
    let e = Env::new();
    e.pair();
    fs::write(e.path("trees.png"), TREES_ART).unwrap();
    e.santa()
        .args([
            "send",
            "family.csv",
            "--outbox",
            "outbox",
            "--banner",
            "trees.png",
        ])
        .write_stdin("send 6\n")
        .assert()
        .success();
    assert!(parse_eml(&e.outbox_files()[0]).banner == TREES_ART);
}

#[test]
fn correction_dry_run_previews_the_yeti() {
    let e = Env::new();
    seal_correction(&e);
    e.santa()
        .args(["send", "family.csv", "--dry-run"])
        .assert()
        .success();
    let preview = fs::read(e.path(".santa/family/preview/banner.png")).unwrap();
    assert!(preview == YETI_ART, "dry-run preview shows the yeti banner");
}

#[test]
fn legacy_history_import_reports_only_counts() {
    let e = Env::new();
    let receivers = [2, 4, 5, 1, 3, 0];
    let legacy = receivers
        .iter()
        .enumerate()
        .map(|(giver, receiver)| {
            format!(
                "\"{},H{},{}@example.com,{},H{},{}@example.com\"\n",
                NAMES[giver],
                giver / 2,
                NAMES[giver].to_lowercase(),
                NAMES[*receiver],
                receiver / 2,
                NAMES[*receiver].to_lowercase(),
            )
        })
        .collect::<String>();
    fs::write(e.path("legacy.csv"), legacy).unwrap();
    let output = stdout_of(e.santa().args([
        "history",
        "import-legacy",
        "family.csv",
        "legacy.csv",
        "--year",
        "2025",
    ]));
    assert!(output.contains("Imported 6 delivered assignments for 2025"));
    assert!(NAMES.iter().all(|name| !output.contains(name)));
}

// ------------------------------------------------------ test-email, reveal ---

#[test]
fn test_email_goes_only_to_the_organizer() {
    let e = Env::new();
    e.santa()
        .args(["test-email", "--to", "me@example.com", "--outbox", "outbox"])
        .assert()
        .success();
    let files = e.outbox_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].to_string_lossy().ends_with("me@example.com.eml"));
    let email = parse_eml(&files[0]);
    assert!(email.subject.starts_with("[TEST] "));
    assert!(email.text.contains("Example Receiver (test)"));
    assert_eq!(email.inline_images, 1);
}

#[test]
fn correction_test_email_shows_the_yeti_copy_and_art() {
    let e = Env::new();
    e.santa()
        .args([
            "test-email",
            "--correction",
            "--year",
            "2026",
            "--to",
            "me@example.com",
            "--outbox",
            "outbox",
        ])
        .assert()
        .success();
    let files = e.outbox_files();
    assert_eq!(files.len(), 1);
    let email = parse_eml(&files[0]);
    assert_eq!(
        email.subject,
        "[TEST] 🏔️ Corrected 2026 Family Secret Santa assignment"
    );
    assert!(email.text.contains("horrible yeti"));
    assert!(email.text.contains("Example Receiver (test)"));
    assert!(
        email.banner == YETI_ART,
        "correction test uses the yeti banner"
    );

    fs::write(e.path("t.txt"), "Subject: Hi\n\n{receiver}\n").unwrap();
    e.santa()
        .args([
            "test-email",
            "--correction",
            "--template",
            "t.txt",
            "--to",
            "me@example.com",
            "--outbox",
            "outbox",
        ])
        .assert()
        .code(2);
}

#[test]
fn test_email_defaults_to_the_sender_address() {
    let e = Env::new();
    e.santa()
        .env("EMAIL_HOST_USER", "organizer@example.com")
        .args(["test-email", "--outbox", "outbox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("organizer@example.com"));
    e.santa()
        .args(["test-email", "--outbox", "outbox"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("direnv allow"));
}

#[test]
fn reveal_shows_one_assignment_consistent_with_seal() {
    let e = Env::new();
    e.pair();
    let out = stdout_of(e.santa().args(["reveal", "family.csv", "--giver", "erin"]));
    assert!(out.starts_with("Erin gives to "));
    let receiver = out.trim().trim_start_matches("Erin gives to ");
    assert!(NAMES.contains(&receiver) && receiver != "Erin" && receiver != "Frank");
}

#[test]
fn state_is_per_roster() {
    let e = Env::new();
    fs::copy(e.path("family.csv"), e.path("office.csv")).unwrap();
    e.pair();
    e.santa().args(["pair", "office.csv"]).assert().success();
    assert!(e.path(".santa/family/pairing.json").exists());
    assert!(e.path(".santa/office/pairing.json").exists());
}
