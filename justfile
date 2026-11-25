# santa — task runner. `just` lists recipes.

santa := "./target/release/santa"

default:
    @just --list

# Everything that must pass before any real send: fmt, clippy, full test suite.
check: fmt-check lint test

# Full test suite: unit, brute-force oracle, property, uniformity, CLI end-to-end, fake-SMTP.
test:
    cargo test

fmt-check:
    cargo fmt --check

lint:
    cargo clippy --all-targets -- -D warnings

bench:
    cargo bench --bench solve

build:
    cargo build --release

# Install `santa` onto PATH (~/.cargo/bin or $CARGO_HOME/bin). Re-run after code changes.
install:
    cargo install --path . --locked

# Re-render the built-in banner art (assets/banner.svg → assets/banner.png).
banner:
    nix run nixpkgs#resvg -- assets/banner.svg assets/banner.png

# Local test inbox: SMTP on 127.0.0.1:1025, web UI on http://127.0.0.1:8025
mailpit:
    nix run nixpkgs#mailpit -- --smtp 127.0.0.1:1025 --listen 127.0.0.1:8025

# Send one sample email to the local Mailpit inbox (run `just mailpit` first).
smoke-local: build
    {{santa}} test-email --to organizer@example.com --from elf@example.com --smtp-security none --smtp-host 127.0.0.1
    @echo "Open http://127.0.0.1:8025 to see it."

# Full rehearsal into Mailpit with a list (default: list_testing.csv). Uses a throwaway state dir.
rehearse-local list="list_testing.csv": build
    #!/usr/bin/env bash
    set -euo pipefail
    state=$(mktemp -d)
    {{santa}} --state-dir "$state" pair {{list}}
    n=$(tail -n +2 {{list}} | grep -c .)
    echo "send $n" | {{santa}} --state-dir "$state" send {{list}} --from elf@example.com --smtp-security none --smtp-host 127.0.0.1
    rm -rf "$state"
    echo "Open http://127.0.0.1:8025 to see all $n emails."

# Real Gmail: one sample email to yourself (EMAIL_HOST_USER), credentials from .envrc.
smoke-gmail: build
    direnv exec . {{santa}} test-email

# Simulate a correction plus five future years and log aggregate-only metrics
# to the private local MLflow store at .santa/mlflow.db.
simulate-history list="list_holtby.csv" years="5" seed="42" correction="false": build
    #!/usr/bin/env bash
    set -euo pipefail
    report=$(mktemp)
    trap 'rm -f "$report"' EXIT
    if [[ "{{correction}}" == "true" ]]; then
        {{santa}} simulate {{list}} --correction --years {{years}} --seed {{seed}} --format json > "$report"
    else
        {{santa}} simulate {{list}} --years {{years}} --seed {{seed}} --format json > "$report"
    fi
    uv run tools/log_simulation.py "$report"

# Export the latest aggregate MLflow run through the MLflow CLI and render a
# self-contained private HTML dashboard with inline SVG charts.
visualize-history correction="true":
    #!/usr/bin/env bash
    set -euo pipefail
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    export MLFLOW_TRACKING_URI="sqlite:///$(pwd)/.santa/mlflow.db"
    mlflow=(uv run --with mlflow==3.6.0 --with sqlalchemy==2.0.44 mlflow)
    "${mlflow[@]}" experiments csv --experiment-id 1 --filename "$work/runs.csv" >/dev/null
    flag=--no-correction
    if [[ "{{correction}}" == "true" ]]; then flag=--correction; fi
    run_id=$(python3 tools/render_simulation.py select-run "$work/runs.csv" "$flag")
    mkdir -p "$work/artifact"
    "${mlflow[@]}" artifacts download --run-id "$run_id" --artifact-path simulation.json --dst-path "$work/artifact" >/dev/null
    python3 tools/render_simulation.py render "$work/artifact/simulation.json" .santa/reports/history-qc.html
