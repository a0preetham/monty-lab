#!/usr/bin/env bash
# Experiment B — every stage is a separate OS process. State crosses only as bytes.
set -u
BIN=target/release/snapshot-cross-process
D=/tmp/monty-b-idle.dump
S=/tmp/monty-b-suspended.dump
cargo build --release 2>&1 | grep -E '^(error|warning: unused)' && exit 1
rm -f "$D" "$S"
fail=0
step() { echo; echo "### $1"; shift; "$@" || { echo "  ^ STAGE FAILED"; fail=1; }; }

step "stage1 (pid A) — build agent state, dump Idle"                 $BIN stage1 "$D"
step "stage2 (pid B, fresh process) — load and keep feeding"          $BIN stage2 "$D"
step "stage3 (pid C) — suspend mid-tool-call, dump, exit unresolved"  $BIN stage3 "$S"
step "stage4 (pid D, fresh process) — resume with a DIFFERENT impl"   $BIN stage4 "$S" 250
step "stage4 again (pid E) — same bytes, another impl, another answer" $BIN stage4 "$S" 999
step "stage5 — dump sizes for realistic agent state"                  $BIN stage5
step "stage6 — version mismatch and garbage handling"                 $BIN stage6 "$D"

echo
if [ $fail -eq 0 ]; then echo "ALL STAGES PASSED"; else echo "SOME STAGES FAILED"; fi
exit $fail
