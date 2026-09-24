#!/usr/bin/env bash
# Experiment I — every stage is a separate OS process, so the host-side store is
# genuinely empty on the far side of the dump unless a stage refills it.
set -u
BIN=target/release/host-objects-across-dump
D=/tmp/monty-i-handle.dump
cargo build --release 2>&1 | grep -E '^(error|warning: unused)' && exit 1
rm -f "$D"
fail=0
step() { echo; echo "### $1"; shift; "$@" || { echo "  ^ STAGE FAILED"; fail=1; }; }

step "stage1 (pid A) — hand the guest a host object, use it, dump"        $BIN stage1 "$D"
step "stage2 (pid B, fresh process) — empty store, as the handoff warns"  $BIN stage2 "$D"
step "stage3 (pid C, fresh process) — rebind the same id to a new object" $BIN stage3 "$D" 999
step "stage3 again (pid D) — same bytes, another object, another answer"  $BIN stage3 "$D" 4200
step "stage4 (pid E) — a store keyed by the wrong id"                     $BIN stage4 "$D"

echo
if [ $fail -eq 0 ]; then echo "ALL STAGES PASSED"; else echo "SOME STAGES FAILED"; fi
exit $fail
