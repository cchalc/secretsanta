//! Shared harness for the binary-level tests: a temp directory with a
//! six-person, three-household roster and helpers to run `santa` in it.
#![allow(dead_code)] // each test crate uses a different subset

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use santa::seal::Sealed;
use tempfile::TempDir;

/// Participants in `family.csv`: households (Alice, Bob), (Carol, Dave), (Erin, Frank).
pub const NAMES: [&str; 6] = ["Alice", "Bob", "Carol", "Dave", "Erin", "Frank"];

pub fn address(name: &str) -> String {
    format!("{}@example.com", name.to_lowercase())
}

pub fn roster_csv() -> String {
    let rows: String = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| format!("2026,{n},{},{}\n", i / 2 + 1, address(n)))
        .collect();
    format!("Timestamp,Full Name,Room number,Email address\n{rows}")
}

pub struct Env {
    dir: TempDir,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("family.csv"), roster_csv()).unwrap();
        Env { dir }
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// `santa` in the temp dir, with any real credentials removed.
    pub fn santa(&self) -> Command {
        let mut c = Command::cargo_bin("santa").unwrap();
        c.current_dir(self.dir.path())
            .env_remove("EMAIL_HOST_USER")
            .env_remove("EMAIL_HOST_PASSWORD");
        c
    }

    pub fn pair(&self) {
        self.santa().args(["pair", "family.csv"]).assert().success();
    }

    /// The sealed `receivers[giver]` for `family.csv`.
    pub fn receivers(&self) -> Vec<usize> {
        santa::files::read_json::<Sealed>(&self.path(".santa/family/pairing.json"))
            .unwrap()
            .receivers
    }

    pub fn outbox_files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(self.path("outbox"))
            .map(|rd| rd.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

pub fn stdout_of(c: &mut Command) -> String {
    String::from_utf8(c.output().unwrap().stdout).unwrap()
}
