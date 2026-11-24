//! Exercises the real SMTP code path (`SmtpMailer` over TCP) against a tiny
//! in-process SMTP server. TLS is not covered here (that is what the Mailpit
//! and Gmail smoke tests are for); everything else is: envelope, MIME body,
//! pooling, server rejections, resume, and "no credentials over plaintext".

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use mail_parser::MimeHeaders;
use predicates::prelude::*;

mod common;
use common::{Env, NAMES, address};

#[derive(Clone, Debug, Default)]
struct Received {
    rcpt: Vec<String>,
    data: String,
}

#[derive(Default)]
struct ServerLog {
    messages: Vec<Received>,
    commands: Vec<String>,
}

/// Start a fake SMTP server. `reject_message` makes the server refuse the
/// k-th message (1-based, counted across connections) at RCPT time.
fn spawn_smtp(reject_message: Option<usize>) -> (u16, Arc<Mutex<ServerLog>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(ServerLog::default()));
    let shared = log.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = shared.clone();
            thread::spawn(move || handle(stream, &log, reject_message));
        }
    });
    (port, log)
}

fn handle(stream: TcpStream, log: &Mutex<ServerLog>, reject_message: Option<usize>) {
    let mut out = stream.try_clone().unwrap();
    let mut input = BufReader::new(stream);
    let mut reply = |s: &str| out.write_all(s.as_bytes()).unwrap();
    reply("220 fake.test ESMTP\r\n");
    let mut current = Received::default();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let cmd = line.trim_end().to_owned();
        log.lock().unwrap().commands.push(cmd.clone());
        let upper = cmd.to_uppercase();
        if upper.starts_with("EHLO") {
            reply("250-fake.test\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n");
        } else if upper.starts_with("RCPT TO:") {
            let n = log.lock().unwrap().messages.len() + 1;
            if reject_message == Some(n) {
                reply("550 5.1.1 mailbox unavailable\r\n");
            } else {
                current
                    .rcpt
                    .push(cmd[8..].trim().trim_matches(['<', '>']).to_owned());
                reply("250 OK\r\n");
            }
        } else if upper == "DATA" {
            reply("354 end with .\r\n");
            let mut data = String::new();
            loop {
                line.clear();
                input.read_line(&mut line).unwrap();
                if line == ".\r\n" {
                    break;
                }
                data.push_str(
                    line.strip_prefix('.')
                        .filter(|_| line.starts_with(".."))
                        .unwrap_or(&line),
                );
            }
            current.data = data;
            log.lock()
                .unwrap()
                .messages
                .push(std::mem::take(&mut current));
            reply("250 queued\r\n");
        } else if upper == "QUIT" {
            reply("221 bye\r\n");
            return;
        } else if upper.starts_with("RSET") {
            current = Received::default();
            reply("250 OK\r\n");
        } else {
            reply("250 OK\r\n");
        }
    }
}

fn smtp_args(port: u16) -> Vec<String> {
    let port = port.to_string();
    [
        "--smtp-security",
        "none",
        "--smtp-host",
        "127.0.0.1",
        "--smtp-port",
        &port,
    ]
    .map(String::from)
    .to_vec()
}

#[test]
fn test_email_is_delivered_over_smtp_as_valid_mime() {
    let (port, log) = spawn_smtp(None);
    let e = Env::new();
    e.santa()
        .args([
            "test-email",
            "--to",
            "me@example.com",
            "--from",
            "elf@example.com",
        ])
        .args(smtp_args(port))
        .assert()
        .success();

    let log = log.lock().unwrap();
    assert_eq!(log.messages.len(), 1);
    let m = &log.messages[0];
    assert_eq!(m.rcpt, vec!["me@example.com"]);
    let parsed = mail_parser::MessageParser::default()
        .parse(m.data.as_bytes())
        .unwrap();
    assert!(parsed.subject().unwrap().starts_with("[TEST] 🎄"));
    assert!(
        parsed
            .body_text(0)
            .unwrap()
            .contains("Example Receiver (test)")
    );
    assert!(parsed.body_html(0).unwrap().contains("cid:banner"));
    assert_eq!(
        parsed
            .attachments()
            .filter(|a| a.content_id() == Some("banner"))
            .count(),
        1
    );
    assert!(m.data.contains("From: =?utf-8?b?") && m.data.contains("<elf@example.com>"));
}

#[test]
fn full_send_over_smtp_matches_the_seal_one_recipient_per_message() {
    let (port, log) = spawn_smtp(None);
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--from", "elf@example.com"])
        .args(smtp_args(port))
        .write_stdin("send 6\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Done: 6 sent this run"));

    let receivers = e.receivers();
    let log = log.lock().unwrap();
    assert_eq!(log.messages.len(), 6);
    for (giver, name) in NAMES.iter().enumerate() {
        let addr = address(name);
        let mine: Vec<&Received> = log
            .messages
            .iter()
            .filter(|m| m.rcpt == vec![addr.clone()])
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "{addr}: exactly one message, addressed only to them"
        );
        let parsed = mail_parser::MessageParser::default()
            .parse(mine[0].data.as_bytes())
            .unwrap();
        let text = parsed.body_text(0).unwrap();
        assert!(text.contains(&format!("🎁 {} 🎁", NAMES[receivers[giver]])));
    }
}

#[test]
fn credentials_are_never_sent_without_tls() {
    let (port, log) = spawn_smtp(None);
    let e = Env::new();
    e.santa()
        .env("EMAIL_HOST_USER", "organizer@example.com")
        .env("EMAIL_HOST_PASSWORD", "super-secret")
        .args(["test-email"])
        .args(smtp_args(port))
        .assert()
        .success();
    let log = log.lock().unwrap();
    assert!(
        log.commands
            .iter()
            .all(|c| !c.to_uppercase().starts_with("AUTH"))
    );
    assert!(log.commands.iter().all(|c| !c.contains("super-secret")));
    assert_eq!(log.messages[0].rcpt, vec!["organizer@example.com"]);
}

#[test]
fn server_rejection_mid_run_is_resumable_without_duplicates() {
    let (bad_port, bad_log) = spawn_smtp(Some(3));
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--from", "elf@example.com"])
        .args(smtp_args(bad_port))
        .write_stdin("send 6\n")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("2 emails sent before the failure"));
    let first: Vec<String> = bad_log
        .lock()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| m.rcpt.clone())
        .collect();
    assert_eq!(first.len(), 2);

    let (good_port, good_log) = spawn_smtp(None);
    e.santa()
        .args(["send", "family.csv", "--from", "elf@example.com"])
        .args(smtp_args(good_port))
        .write_stdin("send 4\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("6/6 total"));
    let second: Vec<String> = good_log
        .lock()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| m.rcpt.clone())
        .collect();
    assert_eq!(second.len(), 4);
    let mut all: Vec<String> = first.into_iter().chain(second).collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 6, "everyone exactly once across both runs");
}

#[test]
fn unreachable_server_fails_before_anything_is_sent() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port(); // now closed
    let e = Env::new();
    e.pair();
    e.santa()
        .args(["send", "family.csv", "--from", "elf@example.com"])
        .args(smtp_args(port))
        .write_stdin("send 6\n")
        .assert()
        .code(1);
    e.santa()
        .args(["verify", "family.csv"])
        .assert()
        .stdout(predicate::str::contains("emails sent so far: 0/6"));
}
