#!/usr/bin/env python3
"""Read chunks-rs JSON-lines from stdin, recompute each (path, mode) with the
Python py_chunks engine via get_chunks, and deep-compare. Reports per-extension
pass/fail and the first few mismatches."""
import json
import sys
from collections import defaultdict

sys.path.insert(0, ".")  # run from py_chunks/ dir
from py_chunks import get_chunks  # noqa: E402

passed = defaultdict(int)
failed = defaultdict(int)
errors = defaultdict(int)
mismatch_samples = []

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    rec = json.loads(raw)
    path, ext, mode = rec["path"], rec["ext"], rec["mode"]
    key = ext

    try:
        py_chunks = get_chunks(path, mode=mode, window_size=3, overlap=1,
                               sentences_per_chunk=3, paragraphs_per_page=15)
    except Exception as e:  # noqa: BLE001
        if not rec["ok"]:
            passed[key] += 1  # both sides errored → agree
        else:
            errors[key] += 1
            if len(mismatch_samples) < 25:
                mismatch_samples.append(f"[{ext} {mode}] PY errored but RS ok: {path}: {e}")
        continue

    if not rec["ok"]:
        errors[key] += 1
        if len(mismatch_samples) < 25:
            mismatch_samples.append(f"[{ext} {mode}] RS errored but PY ok: {path}: {rec.get('error')}")
        continue

    rs_chunks = rec["chunks"]
    # Normalize via json round-trip so int/float & key-order don't cause false diffs.
    rs_norm = json.loads(json.dumps(rs_chunks, sort_keys=True))
    py_norm = json.loads(json.dumps(py_chunks, sort_keys=True))

    if rs_norm == py_norm:
        passed[key] += 1
    else:
        failed[key] += 1
        if len(mismatch_samples) < 25:
            reason = f"count RS={len(rs_chunks)} PY={len(py_chunks)}"
            if len(rs_chunks) == len(py_chunks):
                for i, (a, b) in enumerate(zip(rs_norm, py_norm)):
                    if a != b:
                        ac, bc = a.get("content", ""), b.get("content", "")
                        if ac != bc:
                            reason = f"chunk {i} content differs RS[:60]={ac[:60]!r} PY[:60]={bc[:60]!r}"
                        else:
                            reason = f"chunk {i} metadata/type differs RS={a.get('metadata')} PY={b.get('metadata')}"
                        break
            mismatch_samples.append(f"[{ext} {mode}] {path.split('/')[-1]}: {reason}")

exts = sorted(set(list(passed) + list(failed) + list(errors)))
print(f"{'ext':8} {'pass':>6} {'fail':>6} {'err':>6}")
print("-" * 30)
tp = tf = te = 0
for e in exts:
    print(f"{e:8} {passed[e]:>6} {failed[e]:>6} {errors[e]:>6}")
    tp += passed[e]; tf += failed[e]; te += errors[e]
print("-" * 30)
print(f"{'TOTAL':8} {tp:>6} {tf:>6} {te:>6}")

if mismatch_samples:
    print("\n=== first mismatches ===")
    for s in mismatch_samples:
        print(" -", s)
