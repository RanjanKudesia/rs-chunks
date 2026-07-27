#!/usr/bin/env python3
"""Compare chunks-rs chunk_with_images (stdin JSON lines) against py-chunks
get_chunks(list_images=True). Reports per-ext pass/fail on: chunk count, image
count, image names, and image-chunk contents."""
import json
import sys
from collections import defaultdict

sys.path.insert(0, ".")
from py_chunks import get_chunks  # noqa: E402

passed = defaultdict(int)
failed = defaultdict(int)
samples = []

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    rec = json.loads(raw)
    path, ext, mode = rec["path"], rec["ext"], rec["mode"]
    try:
        res = get_chunks(path, mode=mode, window_size=3, overlap=1,
                         sentences_per_chunk=3, paragraphs_per_page=15, list_images=True)
    except Exception as e:  # noqa: BLE001
        if not rec["ok"]:
            passed[ext] += 1
        else:
            failed[ext] += 1
            if len(samples) < 20:
                samples.append(f"[{ext} {mode}] PY errored, RS ok: {path.split('/')[-1]}: {e}")
        continue
    if not rec["ok"]:
        failed[ext] += 1
        continue

    py_chunks = res.chunks
    py_images = res.images
    py_img_names = sorted(py_images.keys())
    py_img_contents = [c["content"] for c in py_chunks if c["content_type"] == "image"]

    ok = (
        rec["n_chunks"] == len(py_chunks)
        and rec["n_images"] == len(py_images)
        and rec["image_names"] == py_img_names
        and rec["image_chunk_contents"] == py_img_contents
    )
    if ok:
        passed[ext] += 1
    else:
        failed[ext] += 1
        if len(samples) < 20:
            samples.append(
                f"[{ext} {mode}] {path.split('/')[-1]}: "
                f"chunks RS={rec['n_chunks']}/PY={len(py_chunks)} "
                f"images RS={rec['n_images']}/PY={len(py_images)} "
                f"names_eq={rec['image_names']==py_img_names} "
                f"imgchunks_eq={rec['image_chunk_contents']==py_img_contents}"
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
