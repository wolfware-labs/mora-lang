#!/usr/bin/env bash
# Runs cargo-mutants for a CI job and decides the job's result.
#
# Usage: .github/scripts/mutants.sh <cargo mutants arguments...>
# The report is written to "$RUNNER_TEMP/mutants.out".
#
# --in-place builds in the checkout, so the cached target directory is reused
# instead of copying the tree for every shard.
#
# Each test process may allocate 2 GiB: a mutant that turns a loop that
# allocates into an endless one then aborts its tests, which counts as caught,
# instead of filling the runner's memory until the runner dies.
#
# Exit status 3 means some mutants timed out: they made the tests hang, which a
# test run would also notice, so they count as caught. cargo-mutants also exits
# with 3 when mutants were missed besides, so missed.txt decides instead of the
# status: a missed mutant, status 2 and every other status fail the job.
set -euo pipefail

export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="prlimit --data=2147483648 --"

status=0
cargo mutants --in-place --no-shuffle --output "$RUNNER_TEMP" "$@" || status=$?
if [ -s "$RUNNER_TEMP/mutants.out/missed.txt" ]; then
  exit 2
fi
if [ "$status" -ne 0 ] && [ "$status" -ne 3 ]; then
  exit "$status"
fi
