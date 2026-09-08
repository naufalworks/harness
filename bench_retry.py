#!/usr/bin/env python3
import json, sys
sys.path.insert(0, ".")
from bench import bench
import concurrent.futures as cf

try:
    with open("benchmark_report.json") as f:
        out = json.load(f)
except FileNotFoundError:
    out = {}

targets = [m for m in ["gpt-5.5","claude-sonnet-5","hy3","qwen3.8-max-preview","claude-sonnet-4.6","claude-opus-4.7","kimi-k3","qwen3.7-max","kimi-k2.6"] if m not in out]
print("running:", targets, flush=True)

def save():
    with open("benchmark_report.json", "w") as fh:
        json.dump(out, fh, indent=2)

with cf.ThreadPoolExecutor(max_workers=3) as ex:
    futs = {ex.submit(bench, m): m for m in targets}
    for f in cf.as_completed(futs):
        m = futs[f]
        try:
            out[m] = f.result()
            s = sum(t["score"] for t in out[m]["tests"].values())
            print(f"done {m}: {s:.0f}/7", flush=True)
        except Exception as e:
            print(f"FAIL {m}: {e}", flush=True)
        save()  # incremental save after each model

print("saved all")
