#!/usr/bin/env python3
"""Compare chunks-rs to_markdown_with_images (stdin) vs py-chunks
get_markdown(list_images=True): markdown string + image names + image count."""
import json
import sys
from collections import defaultdict

sys.path.insert(0, ".")
from py_chunks import get_markdown  # noqa: E402

passed = defaultdict(int)
failed = defaultdict(int)
samples = []

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    rec = json.loads(raw)
    path, ext = rec["path"], rec["ext"]
    try:
        res = get_markdown(path, list_images=True)
    except Exception as e:  # noqa: BLE001
        if not rec["ok"]:
            passed[ext] += 1
        else:
            failed[ext] += 1
            if len(samples) < 20:
                samples.append(f"[{ext}] PY errored, RS ok: {path.split('/')[-1]}: {e}")
        continue
    if not rec["ok"]:
        failed[ext] += 1
        continue

    py_md = res.markdown
    py_names = sorted(res.images.keys())
    ok = rec["markdown"] == py_md and rec["image_names"] == py_names and rec["n_images"] == len(res.images)
    if ok:
        passed[ext] += 1
    else:
        failed[ext] += 1
        if len(samples) < 20:
            md_eq = rec["markdown"] == py_md
            samples.append(
                f"[{ext}] {path.split('/')[-1]}: md_eq={md_eq} "
                f"(RS_len={len(rec['markdown'])} PY_len={len(py_md)}) "
                f"images RS={rec['n_images']}/PY={len(res.images)} names_eq={rec['image_names']==py_names}"
            )

exts = sorted(set(list(passed) + list(failed)))
print(f"{'ext':8} {'pass':>6} {'fail':>6}")
print("-" * 22)
tp = tf = 0
for e in exts:
    print(f"{e:8} {passed[e]:>6} {failed[e]:>6}")
    tp += passed[e]; tf += failed[e]
print("-" * 22)
print(f"{'TOTAL':8} {tp:>6} {tf:>6}")
if samples:
    print("\n=== mismatches ===")
    for s in samples:
        print(" -", s)
