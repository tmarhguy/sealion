#!/usr/bin/env python3
"""Deterministic scale-corpus generator (§85).

Writes N .txt files (30-60 vocab words + filename-stem title) for
`sealion bench scale`. Titles are unique per file (filename stems), so the
dictionary gains ~N singleton title terms — realistic title/URL
uniqueness, noted in docs/performance.md.

Usage: python3 evaluation/generate_scale.py <out-dir> [docs=100000] [seed=20261001]
"""

import os
import random
import sys

out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/sl-100k/corp"
n = int(sys.argv[2]) if len(sys.argv) > 2 else 100000
seed = int(sys.argv[3]) if len(sys.argv) > 3 else 20261001

random.seed(seed)
vocab = [f"w{i:03d}" for i in range(500)]
common = ["compiler", "database", "systems", "distributed",
          "optimization", "index", "query", "shard"]
os.makedirs(out, exist_ok=True)
for i in range(n):
    count = 30 + (i * 37) % 31
    words = [vocab[(i * 13 + j * 7) % 500] for j in range(count)]
    if i % 97 == 0:
        words += random.sample(common, 2)
    title = " ".join(random.sample(vocab, 3))
    with open(f"{out}/doc{i:06d}.txt", "w") as f:
        f.write(title + "\n" + " ".join(words) + "\n")
    if (i + 1) % 25000 == 0:
        print(f"{i + 1}/{n}", flush=True)
print("corpus done")
