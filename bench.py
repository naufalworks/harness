#!/usr/bin/env python3
"""Benchmark intelligence of satuakses.top relay models.

7 auto-graded tests + identity probe. Parallel across models.
"""
import json, re, time, sys, concurrent.futures as cf
import urllib.request

import os
BASE = "https://api.satuakses.top/v1"
KEY = os.environ.get("SATUAKSES_API_KEY", "")

MODELS = [
    "claude-opus-4.7", "kimi-k3", "qwen3.7-max", "claude-sonnet-5", "hy3",
    "gpt-5.5", "qwen3.8-max-preview", "claude-sonnet-4.6",
    "deepseek-v4-pro-jailbreak", "glm-5.2", "claude-opus-4.6", "kimi-k2.6",
]

# Each test: (name, prompt, grade(answer) -> score 0-1)
TESTS = [
    ("math_multi",
     "A bat and a ball cost $1.10 in total. The bat costs $1.00 more than the ball. How much does the ball cost? Answer with just the dollar amount.",
     lambda a: 1.0 if re.search(r"\$?0\.05\b|\b5\s*cents\b", a) else 0.0),
    ("logic_seq",
     "What is the next number: 2, 3, 5, 9, 17, ...? Answer with just the number.",
     lambda a: 1.0 if re.search(r"\b33\b", a) else 0.0),
    ("instruct_fmt",
     "Reply with exactly the word: banana. Nothing else.",
     lambda a: 1.0 if a.strip().lower().rstrip(".") == "banana" else 0.0),
    ("code_gen",
     "Write a Python one-liner that reverses the string 'abc' using slicing. Output ONLY the code, no backticks, no explanation.",
     lambda a: 1.0 if re.search(r"['\"]abc['\"]\s*\[\s*:\s*:\s*-\s*1\s*\]", a.replace("s", "s")) else 0.0),
    ("reading",
     "Read this and answer in one word: 'The tall man who lived next door, a retired firefighter, adopted three beagles.' What breed are the dogs?",
     lambda a: 1.0 if "beagle" in a.lower() else 0.0),
    ("theory_mind",
     "Sarah puts her chocolate in the drawer and leaves. Her brother moves it to the cabinet while she's out. When Sarah returns, where will she FIRST look for her chocolate? One word.",
     lambda a: 1.0 if "drawer" in a.lower() else 0.0),
    ("self_limit",
     "Count the number of times the letter 'r' appears in: strawberry radiator rover. Answer with just the number.",
     lambda a: 1.0 if re.search(r"\b7\b", a) else 0.0),
]

IDENTITY = "What model are you, actually? State your underlying model name and version if you know it. If you are Claude, say which Claude model exactly."

def chat(model, prompt, timeout=120):
    body = json.dumps({"model": model, "messages": [{"role": "user", "content": prompt}]}).encode()
    req = urllib.request.Request(
        f"{BASE}/chat/completions", data=body,
        headers={"content-type": "application/json", "authorization": f"Bearer {KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            v = json.loads(r.read())
            return v["choices"][0]["message"]["content"].strip()
    except Exception as e:
        return f"__ERR__ {e}"

def bench(model):
    res = {"model": model, "tests": {}, "identity": "", "times": {}}
    for name, prompt, grade in TESTS:
        t0 = time.time()
        a = chat(model, prompt)
        dt = time.time() - t0
        res["times"][name] = round(dt, 1)
        err = a.startswith("__ERR__")
        res["tests"][name] = {"score": 0.0, "answer": a[:120], "error": err}
        if not err:
            res["tests"][name]["score"] = grade(a)
    t0 = time.time()
    ident = chat(model, IDENTITY)
    res["identity"] = ident[:400]
    res["times"]["identity"] = round(time.time() - t0, 1)
    return res

def main():
    out = {}
    with cf.ThreadPoolExecutor(max_workers=4) as ex:
        futs = {ex.submit(bench, m): m for m in MODELS}
        for f in cf.as_completed(futs):
            m = futs[f]
            try:
                out[m] = f.result()
                s = sum(t["score"] for t in out[m]["tests"].values())
                print(f"done {m}: {s:.0f}/7", flush=True)
            except Exception as e:
                print(f"FAIL {m}: {e}", flush=True)
    with open("benchmark_report.json", "w") as fh:
        json.dump(out, fh, indent=2)
    print("saved benchmark_report.json")

if __name__ == "__main__":
    main()
