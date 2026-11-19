#!/usr/bin/env python3
"""Compare raw Rust stdout to a key saved by the original Corten Mac GUI.

Prints only equality, lengths and timing. Neither key is written or displayed.
"""
import argparse
import pathlib
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('reference', type=pathlib.Path, help='Original GUI Save to File output')
parser.add_argument('--binary', type=pathlib.Path, default=pathlib.Path('target/release/pebble-extract-key'))
parser.add_argument('--runs', type=int, default=3)
args = parser.parse_args()
if not 1 <= args.runs <= 100:
    parser.error('--runs must be between 1 and 100')
reference = args.reference.read_bytes()
if not reference:
    raise SystemExit('Reference file is empty')
for number in range(1, args.runs + 1):
    started = time.monotonic()
    result = subprocess.run([str(args.binary.resolve())], capture_output=True, timeout=30)
    elapsed = (time.monotonic() - started) * 1000
    if result.returncode != 0:
        raise SystemExit(f'Run {number}: extractor failed (exit {result.returncode}); output withheld')
    if result.stdout != reference:
        first = next((i for i, (a, b) in enumerate(zip(result.stdout, reference)) if a != b), min(len(result.stdout), len(reference)))
        raise SystemExit(f'Run {number}: MISMATCH at byte {first}; Rust {len(result.stdout)} bytes, reference {len(reference)} bytes. Values withheld.')
    print(f'Run {number}: EXACT MATCH, {len(reference)} bytes, {elapsed:.1f} ms')
