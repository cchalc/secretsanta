#!/usr/bin/env python3
"""Select an aggregate MLflow run and render its simulation artifact as HTML."""

from __future__ import annotations

import argparse
import csv
import html
import json
import os
from pathlib import Path
from typing import Any


def select_run(csv_path: Path, correction: bool) -> None:
    with csv_path.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    wanted = str(correction).lower()
    matches = [
        row
        for row in rows
        if row.get("status") == "FINISHED"
        and row.get("params.includes_correction", "").lower() == wanted
    ]
    if not matches:
        raise SystemExit("no finished MLflow run matches the requested correction mode")
    latest = max(matches, key=lambda row: row.get("start_time", ""))
    run_id = latest.get("run_id", "")
    if len(run_id) != 32 or any(char not in "0123456789abcdef" for char in run_id):
        raise SystemExit("MLflow CSV returned an invalid run id")
    print(run_id)


def svg_bars(years: list[dict[str, Any]], key: str, color: str, label: str) -> str:
    width, height = 760, 260
    left, right, top, bottom = 54, 20, 26, 46
    inner_w, inner_h = width - left - right, height - top - bottom
    values = [int(year[key]) for year in years]
    maximum = max(values + [1])
    slot = inner_w / max(len(years), 1)
    bar_w = min(70, slot * 0.58)
    bars = []
    for index, (year, value) in enumerate(zip(years, values)):
        bar_h = inner_h * value / maximum
        x = left + index * slot + (slot - bar_w) / 2
        y = top + inner_h - bar_h
        bars.append(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{bar_w:.1f}" '
            f'height="{bar_h:.1f}" rx="6" fill="{color}"><title>{value}</title></rect>'
            f'<text x="{x + bar_w / 2:.1f}" y="{y - 7:.1f}" text-anchor="middle" '
            f'class="value">{value}</text>'
            f'<text x="{x + bar_w / 2:.1f}" y="{height - 17}" text-anchor="middle" '
            f'class="axis">{int(year["year"])}</text>'
        )
    return (
        f'<svg viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(label)}">'
        f'<line x1="{left}" y1="{top + inner_h}" x2="{width - right}" '
        f'y2="{top + inner_h}" stroke="#cbd5e1" />'
        + "".join(bars)
        + "</svg>"
    )


def validate_report(report: dict[str, Any]) -> list[dict[str, Any]]:
    years = report.get("years")
    if not isinstance(years, list) or not years:
        raise SystemExit("simulation artifact has no yearly aggregate metrics")
    forbidden = {"assignment", "assignments", "giver", "receiver", "email", "name"}
    serialized = json.dumps(report).lower()
    if any(f'"{field}"' in serialized for field in forbidden):
        raise SystemExit("refusing artifact with a pairing or participant field")
    return years


def render(report_path: Path, output: Path) -> None:
    report = json.loads(report_path.read_text(encoding="utf-8"))
    years = validate_report(report)
    zero_repeats = all(int(year["previous_year_repeats"]) == 0 for year in years)
    zero_violations = all(int(year["rule_violations"]) == 0 for year in years)
    rows = "".join(
        "<tr>"
        f'<td>{int(year["year"])}</td>'
        f'<td>{html.escape(str(year["run_kind"]))}</td>'
        f'<td>{int(year["candidate_pairings"])}</td>'
        f'<td>{int(year["forbidden_edges"])}</td>'
        f'<td>{"—" if year["future_safe_candidates"] is None else int(year["future_safe_candidates"])}</td>'
        f'<td>{int(year["previous_year_repeats"])}</td>'
        f'<td>{int(year["rule_violations"])}</td>'
        "</tr>"
        for year in years
    )
    document = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Secret Santa history QC</title>
<style>
:root{{--ink:#172033;--muted:#64748b;--paper:#f8fafc;--card:#fff;--green:#15803d;--red:#b91c1c;}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--paper);color:var(--ink);font:15px/1.5 system-ui,sans-serif}}
main{{max-width:980px;margin:0 auto;padding:40px 24px 60px}} h1{{margin:0;font-size:32px}} h2{{margin:0 0 12px;font-size:19px}}
.lede{{color:var(--muted);margin:6px 0 26px}} .grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px}}
.card,.panel{{background:var(--card);border:1px solid #e2e8f0;border-radius:14px;box-shadow:0 8px 24px #0f172a0a}}
.card{{padding:16px}} .metric{{font-size:26px;font-weight:750}} .label{{color:var(--muted);font-size:13px}}
.panel{{margin-top:16px;padding:20px;overflow:auto}} .ok{{color:var(--green)}} .bad{{color:var(--red)}}
svg{{width:100%;min-width:520px;height:auto}} svg text{{font-family:system-ui,sans-serif}} .axis{{fill:var(--muted);font-size:13px}} .value{{fill:var(--ink);font-weight:700;font-size:13px}}
table{{width:100%;border-collapse:collapse;min-width:700px}} th,td{{padding:10px;border-bottom:1px solid #e2e8f0;text-align:right}} th:first-child,td:first-child,th:nth-child(2),td:nth-child(2){{text-align:left}} th{{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.04em}}
footer{{margin-top:18px;color:var(--muted);font-size:12px}}
</style></head><body><main>
<h1>Secret Santa history QC</h1><p class="lede">Aggregate-only MLflow simulation report. No pairings or participant data are included.</p>
<section class="grid">
<div class="card"><div class="metric">{int(report["participant_count"])}</div><div class="label">Participants</div></div>
<div class="card"><div class="metric">{int(report["simulated_years"])}</div><div class="label">Future years simulated</div></div>
<div class="card"><div class="metric">{html.escape(str(report.get("minimum_repeat_gap_years", "—")))}</div><div class="label">Minimum official repeat gap</div></div>
<div class="card"><div class="metric {'ok' if zero_repeats else 'bad'}">{'PASS' if zero_repeats else 'FAIL'}</div><div class="label">Previous-year repeats</div></div>
<div class="card"><div class="metric {'ok' if zero_violations else 'bad'}">{'PASS' if zero_violations else 'FAIL'}</div><div class="label">Pairing rules</div></div>
</section>
<section class="panel"><h2>Valid candidate pairings by year</h2>{svg_bars(years, "candidate_pairings", "#2563eb", "Candidate pairings by year")}</section>
<section class="panel"><h2>Forbidden historical edges by year</h2>{svg_bars(years, "forbidden_edges", "#7c3aed", "Forbidden historical edges by year")}</section>
<section class="panel"><h2>Yearly QC details</h2><table><thead><tr><th>Year</th><th>Run</th><th>Candidates</th><th>Forbidden edges</th><th>Future-safe</th><th>Repeats</th><th>Violations</th></tr></thead><tbody>{rows}</tbody></table></section>
<footer>Policy: {html.escape(str(report["policy"]))} · Seed: {int(report["seed"])} · Correction included: {str(bool(report["includes_correction"])).lower()}</footer>
</main></body></html>"""
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    output.write_text(document, encoding="utf-8")
    os.chmod(output, 0o600)
    print(output)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    select = commands.add_parser("select-run")
    select.add_argument("csv", type=Path)
    select.add_argument("--correction", action=argparse.BooleanOptionalAction, default=True)
    draw = commands.add_parser("render")
    draw.add_argument("report", type=Path)
    draw.add_argument("output", type=Path)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "select-run":
        select_run(args.csv, args.correction)
    else:
        render(args.report, args.output)


if __name__ == "__main__":
    main()
