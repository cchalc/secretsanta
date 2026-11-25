# /// script
# requires-python = ">=3.11"
# dependencies = ["mlflow==3.6.0", "sqlalchemy==2.0.44"]
# ///
"""Log an aggregate santa simulation to a local MLflow file store.

The Rust CLI owns simulation and secrecy. This adapter rejects likely PII or
pairing fields before logging the already-aggregate JSON report.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any

import mlflow


FORBIDDEN_KEYS = {
    "assignment",
    "assignments",
    "giver",
    "givers",
    "receiver",
    "receivers",
    "email",
    "emails",
    "name",
    "names",
    "pairing_id",
}
ROOT_KEYS = {
    "policy",
    "participant_count",
    "seed",
    "includes_correction",
    "simulated_years",
    "minimum_repeat_gap_years",
    "years",
}
YEAR_KEYS = {
    "year",
    "run_kind",
    "candidate_pairings",
    "candidate_count_exact",
    "previous_runs_avoided",
    "forbidden_edges",
    "future_safe_candidates",
    "previous_year_repeats",
    "rule_violations",
}


def reject_sensitive(value: Any, path: str = "report") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            lowered = key.lower()
            if lowered in FORBIDDEN_KEYS:
                raise ValueError(f"refusing sensitive field at {path}.{key}")
            reject_sensitive(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            reject_sensitive(child, f"{path}[{index}]")
    elif isinstance(value, str) and "@" in value:
        raise ValueError(f"refusing possible email address at {path}")


def validate_schema(report: dict[str, Any]) -> None:
    if set(report) != ROOT_KEYS:
        raise ValueError("refusing report with an unexpected root schema")
    if not isinstance(report["years"], list):
        raise ValueError("refusing report whose years field is not a list")
    if any(not isinstance(year, dict) or set(year) != YEAR_KEYS for year in report["years"]):
        raise ValueError("refusing report with an unexpected yearly schema")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    parser.add_argument("--tracking-db", type=Path, default=Path(".santa/mlflow.db"))
    parser.add_argument(
        "--artifact-dir",
        type=Path,
        default=Path("/private/tmp/santa-mlflow-artifacts"),
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    report = json.loads(args.report.read_text(encoding="utf-8"))
    validate_schema(report)
    reject_sensitive(report)

    tracking_db = args.tracking_db.resolve()
    artifact_dir = args.artifact_dir.resolve()
    tracking_db.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    artifact_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    mlflow.set_tracking_uri(f"sqlite:///{tracking_db}")
    # MLflow creates a default experiment during database initialization. Run
    # that initialization from a generic path so it cannot capture the OS
    # username from this checkout's absolute path.
    os.chdir("/private/tmp")
    client = mlflow.MlflowClient()
    experiment = client.get_experiment_by_name("santa-history-qc")
    experiment_id = (
        experiment.experiment_id
        if experiment is not None
        else client.create_experiment(
            "santa-history-qc", artifact_location=artifact_dir.as_uri()
        )
    )
    with mlflow.start_run(
        experiment_id=experiment_id,
        run_name="five-year-simulation",
        tags={
            "mlflow.user": "santa-local",
            "mlflow.source.name": "santa-history-qc",
        },
    ):
        mlflow.log_params(
            {
                "policy": report["policy"],
                "participant_count": report["participant_count"],
                "seed": report["seed"],
                "includes_correction": report["includes_correction"],
                "simulated_years": report["simulated_years"],
            }
        )
        gap = report.get("minimum_repeat_gap_years")
        if gap is not None:
            mlflow.log_metric("minimum_repeat_gap_years", gap)
        for step, year in enumerate(report["years"]):
            mlflow.log_metric("candidate_pairings", year["candidate_pairings"], step=step)
            mlflow.log_metric("forbidden_edges", year["forbidden_edges"], step=step)
            mlflow.log_metric(
                "previous_year_repeats", year["previous_year_repeats"], step=step
            )
            mlflow.log_metric("rule_violations", year["rule_violations"], step=step)
            if year["future_safe_candidates"] is not None:
                mlflow.log_metric(
                    "future_safe_candidates", year["future_safe_candidates"], step=step
                )
        mlflow.log_dict(report, "simulation.json")
        tracking_db.chmod(0o600)
        print(
            "Logged aggregate QC to .santa/mlflow.db; "
            "no pairing data was accepted."
        )


if __name__ == "__main__":
    main()
