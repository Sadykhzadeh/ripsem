#!/usr/bin/env python3
"""Retrieval-quality benchmark for ripsem. See eval/README.md.

  bench.py fetch      clone the pinned corpora
  bench.py prepare    cut the doc comments out of a copy, write query set 1
  bench.py index      build an index per corpus, backend and query set
  bench.py run        run every system over every query
  bench.py report     print the tables
  bench.py failures   classify the misses of one system

Python standard library only.
"""

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import extract  # noqa: E402

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
CORPORA = json.loads((HERE / "corpora.json").read_text())
SEED = 20261114
CAP = 300
DEPTH = 10
BACKENDS = ["hash", "fastembed"]
# name -> (kind, backend, arm)
SYSTEMS = {
    "hash": ("cli", "hash", None),
    "fastembed": ("cli", "fastembed", None),
    "hash-lexical": ("arms", "hash", "lexical"),
    "hash-dense": ("arms", "hash", "dense"),
    "fastembed-lexical": ("arms", "fastembed", "lexical"),
    "fastembed-dense": ("arms", "fastembed", "dense"),
    "ripgrep": ("rg", "hash", None),
}
SETS = {"auto": "strip", "curated": "orig"}


# --- paths and processes ---------------------------------------------------

class Ctx:
    def __init__(self, args):
        self.work = Path(args.work).expanduser().resolve()
        exe = ".exe" if os.name == "nt" else ""
        self.ripsem = args.ripsem or shutil.which("ripsem") or str(REPO / "target" / "release" / f"ripsem{exe}")
        self.arms = args.arms or str(HERE / "arms" / "target" / "release" / f"ripsem-eval-arms{exe}")
        self.rg = args.rg or shutil.which("rg") or "rg"
        self.repos = [c for c in CORPORA if not args.repo or c["name"] in args.repo]
        self.args = args

    def corpus(self, name):
        return self.work / "corpora" / name

    def base(self, name, variant):
        return self.work / "trees" / name / variant

    def tree(self, name, variant, backend):
        return self.work / "trees" / name / f"{variant}-{backend}"

    def queries(self, name, qset):
        if qset == "curated":
            return HERE / "queries" / f"{name}.jsonl"
        return self.work / "queries" / f"auto-{name}.jsonl"

    def raw(self, system, qset, name):
        return self.work / "results" / "raw" / f"{system}__{qset}__{name}.json"


def peak_rss_tools():
    """Returns (spawn, ...) that runs a command and reports its peak memory."""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        class Counters(ctypes.Structure):
            _fields_ = [
                ("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t),
            ]

        def run(cmd, **kw):
            t0 = time.perf_counter()
            p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, **kw)
            out = p.stdout.read()
            p.wait()
            dt = time.perf_counter() - t0
            c = Counters()
            c.cb = ctypes.sizeof(c)
            ctypes.windll.psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
            ok = ctypes.windll.psapi.GetProcessMemoryInfo(int(p._handle), ctypes.byref(c), c.cb)
            return p.returncode, out, dt, (c.PeakWorkingSetSize if ok else 0)

        return run

    def run(cmd, **kw):
        t0 = time.perf_counter()
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, **kw)
        out = p.stdout.read()
        _, status, usage = os.wait4(p.pid, 0)
        dt = time.perf_counter() - t0
        p.returncode = os.waitstatus_to_exitcode(status)
        scale = 1 if sys.platform == "darwin" else 1024
        return p.returncode, out, dt, usage.ru_maxrss * scale

    return run


timed = peak_rss_tools()


def sh(cmd, cwd=None, check=True):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if check and r.returncode:
        sys.exit(f"command failed: {' '.join(map(str, cmd))}\n{r.stderr}")
    return r


def read_jsonl(path):
    rows = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            rows.append(json.loads(line))
    return rows


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=1, sort_keys=True), encoding="utf-8")


# --- fetch ----------------------------------------------------------------

def cmd_fetch(ctx):
    for c in ctx.repos:
        dest = ctx.corpus(c["name"])
        if not (dest / ".git").is_dir():
            dest.mkdir(parents=True, exist_ok=True)
            git = ["git", "-c", "core.autocrlf=false", "-c", "core.symlinks=false"]
            sh(git + ["init", "-q"], cwd=dest)
            sh(git + ["remote", "add", "origin", c["url"]], cwd=dest)
            print(f"fetching {c['name']} {c['tag']}", flush=True)
            sh(git + ["fetch", "-q", "--depth", "1", "origin", c["commit"]], cwd=dest)
            sh(git + ["-c", "advice.detachedHead=false", "checkout", "-q", "FETCH_HEAD"], cwd=dest)
        head = sh(["git", "rev-parse", "HEAD"], cwd=dest).stdout.strip()
        if head != c["commit"]:
            sys.exit(f"{c['name']}: checked out {head}, expected {c['commit']}")
        print(f"{c['name']}: {c['tag']} {head}")


# --- prepare: query set 1 and the doc-free copy --------------------------

def copy_tree(src, dst):
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(src, dst, ignore=shutil.ignore_patterns(".git", ".ripsem"))


def source_files(root, exts):
    out = []
    for dirpath, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d != ".git")
        for f in sorted(files):
            if any(f.endswith(e) for e in exts):
                out.append(Path(dirpath, f))
    return out


def cmd_prepare(ctx):
    stats = {}
    for c in ctx.repos:
        name = c["name"]
        src = ctx.corpus(name)
        orig, strip = ctx.base(name, "orig"), ctx.base(name, "strip")
        copy_tree(src, orig)
        found = []
        for f in source_files(orig, c["extensions"]):
            rel = f.relative_to(orig).as_posix()
            if not extract.is_target_file(rel):
                continue
            text = f.read_bytes().decode("utf-8", errors="replace")
            found += extract.candidates(c["language"], rel, text)
        pool = len(found)
        counts = {}
        for item in found:
            counts[item["query"].lower()] = counts.get(item["query"].lower(), 0) + 1
        found = [x for x in found if counts[x["query"].lower()] == 1]
        found.sort(key=lambda x: (x["path"], x["start"]))
        rng = random.Random(f"{SEED}:{name}")
        picked = sorted(rng.sample(found, min(CAP, len(found))), key=lambda x: (x["path"], x["start"]))

        copy_tree(orig, strip)
        by_file = {}
        for x in picked:
            by_file.setdefault(x["path"], []).append(x)
        rows = []
        for path, items in sorted(by_file.items()):
            file = strip / path
            text = file.read_bytes().decode("utf-8", errors="replace")
            new_text, renumber = extract.cut_lines(text, [x["cut"] for x in items])
            file.write_bytes(new_text.encode("utf-8"))
            for x in items:
                x["new_start"], x["new_end"] = renumber(x["start"]), renumber(x["end"])
        for n, x in enumerate(sorted(picked, key=lambda x: (x["path"], x["start"])), 1):
            rows.append({
                "id": f"{name}-a{n:03d}",
                "query": x["query"],
                "targets": [{"path": x["path"], "start": x["new_start"], "end": x["new_end"]}],
                "name": x["name"],
                "leaks_name": x["leaks_name"],
            })
        out = ctx.queries(name, "auto")
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in rows), encoding="utf-8")
        digest = hashlib.sha256(out.read_bytes()).hexdigest()[:16]
        stats[name] = {"pool": pool, "unique": len(found), "queries": len(rows),
                       "leaking": sum(r["leaks_name"] for r in rows), "sha256": digest,
                       "source_files": len(source_files(orig, c["extensions"]))}
        print(f"{name}: {pool} documented functions, {len(found)} usable, {len(rows)} queries, "
              f"{stats[name]['leaking']} name their answer, sha256 {digest}")
    write_json(ctx.work / "queries" / "stats.json", stats)


# --- validate the curated file against the pinned tree -------------------

def cmd_validate(ctx):
    bad = 0
    for c in ctx.repos:
        rows = read_jsonl(ctx.queries(c["name"], "curated"))
        for r in rows:
            for t in r["targets"]:
                f = ctx.base(c["name"], "orig") / t["path"]
                if not f.is_file():
                    print(f"{r['id']}: no file {t['path']}")
                    bad += 1
                    continue
                n = len(f.read_bytes().split(b"\n"))
                if not (1 <= t["start"] <= t["end"] <= n):
                    print(f"{r['id']}: lines {t['start']}-{t['end']} outside {t['path']} ({n} lines)")
                    bad += 1
            words = len(r["query"].split())
            if not 3 <= words <= 20:
                print(f"{r['id']}: {words} words")
        print(f"{c['name']}: {len(rows)} curated queries")
    sys.exit(1 if bad else 0)


# --- index ----------------------------------------------------------------

def prepare_index_tree(ctx, name, variant, backend):
    tree = ctx.tree(name, variant, backend)
    copy_tree(ctx.base(name, variant), tree)
    sh([ctx.ripsem, "init", "--path", str(tree)])
    sh([ctx.ripsem, "model", "--path", str(tree), "backend", backend])
    return tree


def indexed_files(ctx, tree):
    r = sh([ctx.ripsem, "index", "--path", str(tree), "--dry-run"])
    files = [l.strip() for l in r.stdout.splitlines() if l.strip() and not re.match(r"^\d+ files? would", l.strip())]
    return files


def cmd_index(ctx):
    results = {}
    results_path = ctx.work / "results" / "index.json"
    if results_path.exists():
        results = json.loads(results_path.read_text())
    repeats = ctx.args.repeats
    for c in ctx.repos:
        name = c["name"]
        for backend in ctx.args.backends:
            for variant in ("orig", "strip"):
                tree = prepare_index_tree(ctx, name, variant, backend)
                index_file = tree / ".ripsem" / "index.bin"
                times, rss = [], []
                runs = repeats if variant == "orig" else 1
                for _ in range(runs):
                    if index_file.exists():
                        index_file.unlink()
                    code, out, dt, peak = timed([ctx.ripsem, "index", "--path", str(tree)])
                    if code:
                        sys.exit(f"index failed in {tree}")
                    times.append(dt)
                    rss.append(peak)
                status = json.loads(sh([ctx.ripsem, "status", "--json", "--path", str(tree)]).stdout)
                key = f"{name}/{variant}/{backend}"
                results[key] = {
                    "seconds": times, "peak_rss": rss, "bytes": index_file.stat().st_size,
                    "files": len(indexed_files(ctx, tree)), "status": status,
                }
                print(f"{key}: median {statistics.median(times):.2f}s over {runs}, "
                      f"{index_file.stat().st_size / 1e6:.1f} MB, peak {statistics.median(rss) / 1e6:.0f} MB", flush=True)
                write_json(results_path, results)


# --- running systems -------------------------------------------------------

def overlaps(hit, target):
    return hit[0] == target["path"] and hit[1] <= target["end"] and hit[2] >= target["start"]


def cli_search(ctx, tree, query, k):
    code, out, dt, peak = timed([ctx.ripsem, "search", "--json", "-k", str(k), "--path", str(tree), "--", query])
    if code:
        return None, dt, peak
    rows = json.loads(out.decode("utf-8"))
    return [[r["path"], r["start_line"], r["end_line"]] for r in rows], dt, peak


def terms_of(query):
    seen, out = set(), []
    for w in re.findall(r"[a-z0-9_]+", query.lower()):
        if w in extract.STOPWORDS or len(w) < 2 or w in seen:
            continue
        seen.add(w)
        out.append(w)
    return out


def rg_search(ctx, tree, files, query):
    words = terms_of(query)
    if not words:
        return [], 0.0, 0
    cmd = [ctx.rg, "-i", "-n", "--no-heading", "--color", "never", "--no-messages",
           "--no-require-git", "--max-columns", "10"]
    for w in words:
        cmd += ["-e", w]
    cmd.append(".")
    code, out, dt, peak = timed(cmd, cwd=str(tree))
    lines_by_file = {}
    for line in out.decode("utf-8", errors="replace").splitlines():
        m = re.match(r"^(.*?):(\d+):", line)
        if m:
            path = m.group(1).replace("\\", "/")
            path = path[2:] if path.startswith("./") else path
            if path in files:
                lines_by_file.setdefault(path, set()).add(int(m.group(2)))
    ranked = sorted(lines_by_file.items(), key=lambda kv: (-len(kv[1]), kv[0]))[:DEPTH]
    return [[p, min(ls), max(ls), sorted(ls)] for p, ls in ranked], dt, peak


def arms_records(ctx, tree):
    out = sh([ctx.arms, str(tree), "records"]).stdout
    return json.loads(out)


def arms_rank(ctx, tree, queries, depth):
    path = ctx.work / "results" / "arms-queries.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps({"id": q["id"], "query": q["query"]}) + "\n" for q in queries), encoding="utf-8")
    out = sh([ctx.arms, str(tree), "rank", str(path), str(depth)]).stdout
    return {row["id"]: row for row in map(json.loads, out.splitlines())}


def places(records, numbers):
    """Record numbers as distinct places, best first."""
    seen, out = set(), []
    for n in numbers:
        p, s, e = records[n][0], records[n][1], records[n][2]
        if (p, s, e) not in seen:
            seen.add((p, s, e))
            out.append([p, s, e])
    return out


def run_one(ctx, system, qset, name):
    kind, backend, arm = SYSTEMS[system]
    variant = SETS[qset]
    tree = ctx.tree(name, variant, backend)
    queries = read_jsonl(ctx.queries(name, qset))
    rows = []
    if kind == "cli":
        cli_search(ctx, tree, queries[0]["query"], DEPTH)
        for q in queries:
            hits, dt, peak = cli_search(ctx, tree, q["query"], DEPTH)
            rows.append({"id": q["id"], "hits": hits, "seconds": dt, "peak_rss": peak})
    elif kind == "rg":
        files = set(indexed_files(ctx, tree))
        rg_search(ctx, tree, files, queries[0]["query"])
        for q in queries:
            hits, dt, peak = rg_search(ctx, tree, files, q["query"])
            rows.append({"id": q["id"], "hits": hits, "seconds": dt, "peak_rss": peak})
    else:
        records = arms_records(ctx, tree)
        ranked = arms_rank(ctx, tree, queries, 200)
        for q in queries:
            rows.append({"id": q["id"], "hits": places(records, ranked[q["id"]][arm])[:50]})
    write_json(ctx.raw(system, qset, name), rows)
    return rows


def cmd_run(ctx):
    for system in ctx.args.systems:
        for qset in ctx.args.sets:
            for c in ctx.repos:
                t0 = time.perf_counter()
                run_one(ctx, system, qset, c["name"])
                print(f"{system} {qset} {c['name']}: {time.perf_counter() - t0:.0f}s", flush=True)


# --- metrics --------------------------------------------------------------

def rank_of(hits, targets, level, depth=DEPTH):
    for n, h in enumerate(hits[:depth], 1):
        for t in targets:
            if level == "file":
                if h[0] == t["path"]:
                    return n
            elif level == "line" and len(h) > 3:
                if h[0] == t["path"] and any(t["start"] <= l <= t["end"] for l in h[3]):
                    return n
            elif overlaps(h, t):
                return n
    return None


def metrics(ranks):
    n = len(ranks) or 1
    return {
        "n": len(ranks),
        "mrr": sum(1 / r for r in ranks if r) / n,
        "r1": sum(1 for r in ranks if r and r <= 1) / n,
        "r5": sum(1 for r in ranks if r and r <= 5) / n,
        "r10": sum(1 for r in ranks if r and r <= 10) / n,
    }


def load_ranks(ctx, system, qset, name):
    path = ctx.raw(system, qset, name)
    if not path.exists():
        return None
    queries = {q["id"]: q for q in read_jsonl(ctx.queries(name, qset))}
    kind = SYSTEMS[system][0]
    out = []
    for row in json.loads(path.read_text()):
        q = queries[row["id"]]
        hits = row["hits"] or []
        region = "line" if kind == "rg" else "region"
        out.append({
            "id": row["id"], "leaks": q.get("leaks_name", False),
            "file": rank_of(hits, q["targets"], "file"),
            "region": rank_of(hits, q["targets"], region),
            "seconds": row.get("seconds"), "peak_rss": row.get("peak_rss"),
        })
    return out


def bootstrap_ci(values, seed=7, rounds=2000):
    rng = random.Random(seed)
    n = len(values)
    means = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(rounds))
    return means[int(0.025 * rounds)], means[int(0.975 * rounds)]


def pct(values, p):
    s = sorted(values)
    return s[min(len(s) - 1, int(round(p * (len(s) - 1))))]


def fmt(m):
    return f"{m['mrr']:.3f} {m['r1']:.3f} {m['r5']:.3f} {m['r10']:.3f}"


def cmd_report(ctx):
    systems = [s for s in ctx.args.systems if any(ctx.raw(s, q, c["name"]).exists() for q in SETS for c in ctx.repos)]
    digest = {}
    for qset in ctx.args.sets:
        for level in ("region", "file"):
            print(f"\n### Query set `{qset}`, {level} level (MRR@10, R@1, R@5, R@10)\n")
            print("| system | " + " | ".join(c["name"] for c in ctx.repos) + " | all (mean of repos) |")
            print("|---|" + "---|" * (len(ctx.repos) + 1))
            for s in systems:
                cells, per = [], []
                for c in ctx.repos:
                    ranks = load_ranks(ctx, s, qset, c["name"])
                    if ranks is None:
                        cells.append("-")
                        continue
                    m = metrics([r[level] for r in ranks])
                    per.append(m)
                    cells.append(fmt(m))
                    digest[f"{s}/{qset}/{level}/{c['name']}"] = {k: round(v, 6) for k, v in m.items()}
                if per:
                    avg = {k: sum(m[k] for m in per) / len(per) for k in ("mrr", "r1", "r5", "r10")}
                    avg["n"] = sum(m["n"] for m in per)
                    cells.append(fmt(avg))
                print(f"| {s} | " + " | ".join(cells) + " |")
        if qset == "auto":
            print("\n### Query set `auto`, region level, split by whether the query contains the answer's own name\n")
            print("| system | queries naming the answer (n, MRR@10, R@5) | queries not naming it (n, MRR@10, R@5) |")
            print("|---|---|---|")
            for s in systems:
                allr = []
                for c in ctx.repos:
                    allr += load_ranks(ctx, s, qset, c["name"]) or []
                cells = []
                for flag in (True, False):
                    sub = [r["region"] for r in allr if r["leaks"] == flag]
                    m = metrics(sub)
                    cells.append(f"{m['n']}, {m['mrr']:.3f}, {m['r5']:.3f}")
                print(f"| {s} | " + " | ".join(cells) + " |")

        print(f"\n### Query set `{qset}`: uncertainty of region-level MRR@10 (95% bootstrap interval, all queries pooled)\n")
        print("| system | MRR@10 | interval |")
        print("|---|---|---|")
        for s in systems:
            pooled = []
            for c in ctx.repos:
                pooled += [1 / r["region"] if r["region"] else 0.0 for r in (load_ranks(ctx, s, qset, c["name"]) or [])]
            if pooled:
                lo, hi = bootstrap_ci(pooled)
                print(f"| {s} | {sum(pooled) / len(pooled):.3f} | {lo:.3f} to {hi:.3f} |")

    print("\n### Latency and memory of a search process (process start included), per system\n")
    print("| system | queries | p50 ms | p95 ms | median peak RSS MB | max peak RSS MB |")
    print("|---|---|---|---|---|---|")
    for s in systems:
        if SYSTEMS[s][0] == "arms":
            continue
        secs, rss = [], []
        for qset in SETS:
            for c in ctx.repos:
                for r in load_ranks(ctx, s, qset, c["name"]) or []:
                    if r["seconds"] is not None:
                        secs.append(r["seconds"] * 1000)
                        rss.append(r["peak_rss"] / 1e6)
        if secs:
            print(f"| {s} | {len(secs)} | {pct(secs, .5):.0f} | {pct(secs, .95):.0f} | "
                  f"{statistics.median(rss):.0f} | {max(rss):.0f} |")
    index_path = ctx.work / "results" / "index.json"
    if index_path.exists():
        idx = json.loads(index_path.read_text())
        print("\n### Index build (median of the runs, original tree)\n")
        print("| corpus | backend | files | chunks | build s | runs | peak RSS MB | index MB |")
        print("|---|---|---|---|---|---|---|---|")
        for key in sorted(idx):
            name, variant, backend = key.split("/")
            if variant != "orig" or name not in [c["name"] for c in ctx.repos]:
                continue
            v = idx[key]
            chunks = v["status"].get("index", {}).get("chunks", v["status"].get("index", {}).get("records", "?"))
            print(f"| {name} | {backend} | {v['files']} | {chunks} | {statistics.median(v['seconds']):.1f} | "
                  f"{len(v['seconds'])} | {statistics.median(v['peak_rss']) / 1e6:.0f} | {v['bytes'] / 1e6:.1f} |")
    blob = json.dumps(digest, sort_keys=True).encode()
    print(f"\nquality digest (timings excluded): {hashlib.sha256(blob).hexdigest()[:16]}")


# --- failures --------------------------------------------------------------

TESTISH = re.compile(r"(^|/)(tests?|__tests__|testdata|integration|e2e|examples?|samples?|vendor|node_modules|spec)/|_test\.go$|\.spec\.|\.test\.|(^|/)test_")


def classify(f):
    """The first class that fits, in the order of how directly it explains the miss."""
    top = f["top"][0][0] if f["top"] else ""
    rank = f["cli_rank"]
    if f["n_records"] == 0:
        return "answer not in the index"
    if f["n_records"] >= 2 and f["best_cover"] < 0.6:
        return "answer split across chunks"
    if f["tightest_record"] >= 3 * f["span"] and f["tightest_record"] >= 30:
        return "chunk much longer than the answer (diluted)"
    if TESTISH.search(top):
        return "test or example code ranked first"
    if rank and f["file_rank"] is not None:
        return "ranked 11 to 50, a sibling in the same file won"
    if rank:
        return "ranked 11 to 50, another file won"
    deep = [r for r in (f["dense_rank"], f["lexical_rank"]) if r]
    if deep and min(deep) <= 50:
        return "ranked below 50 in the merge, in the top 50 of one arm"
    if f["file_rank"] is not None:
        return "right file, wrong function, answer beyond rank 50"
    if not f["query_words_in_body"]:
        return "no shared words with the answer, far in meaning"
    return "words shared, but beyond rank 50 in both arms"


def cmd_failures(ctx):
    system, qset = ctx.args.system, ctx.args.set
    kind, backend, _ = SYSTEMS[system]
    out = []
    for c in ctx.repos:
        name = c["name"]
        tree = ctx.tree(name, SETS[qset], backend)
        queries = {q["id"]: q for q in read_jsonl(ctx.queries(name, qset))}
        ranks = load_ranks(ctx, system, qset, name)
        raw = {r["id"]: r for r in json.loads(ctx.raw(system, qset, name).read_text())}
        missed = [r for r in ranks if not r["region"]]
        if qset == "auto" and ctx.args.sample and len(missed) > ctx.args.sample:
            missed = random.Random(f"{SEED}:sample:{name}").sample(missed, ctx.args.sample)
            missed.sort(key=lambda r: r["id"])
        if not missed:
            continue
        records = arms_records(ctx, tree)
        subset = [queries[r["id"]] for r in missed]
        deep = arms_rank(ctx, tree, subset, 200)
        for r in missed:
            q = queries[r["id"]]
            t = q["targets"][0]
            row = deep[r["id"]]
            mine = [n for n, rec in enumerate(records) if rec[0] == t["path"] and rec[1] <= t["end"] and rec[2] >= t["start"]]
            span = t["end"] - t["start"] + 1

            def where(order):
                for pos, n in enumerate(order, 1):
                    if n in mine:
                        return pos
                return None

            hits10 = raw[r["id"]]["hits"] or []
            cover = [min(records[n][2], t["end"]) - max(records[n][1], t["start"]) + 1 for n in mine]
            deeper, _, _ = cli_search(ctx, tree, q["query"], 50) if kind == "cli" else (hits10, 0, 0)
            cli_rank = rank_of(deeper or [], q["targets"], "region", 50)
            tight = [records[n][2] - records[n][1] + 1 for n in mine if min(records[n][2], t["end"]) - max(records[n][1], t["start"]) + 1 >= 0.8 * span]
            file_text = (tree / t["path"]).read_bytes().decode("utf-8", "replace").split("\n")
            body = file_text[t["start"] - 1 : t["end"]]
            comment = sum(1 for l in body if l.strip().startswith(("//", "#", "*", "/*")))
            out.append({
                "id": r["id"], "repo": name, "query": q["query"], "target": t,
                "name": q.get("name"), "leaks": q.get("leaks_name"),
                "file_rank": r["file"],
                "records_over_target": [[records[n][0], records[n][1], records[n][2], records[n][3]] for n in mine][:6],
                "n_records": len(mine),
                "best_cover": max(cover) / span if cover else 0,
                "biggest_record": max((records[n][2] - records[n][1] + 1 for n in mine), default=0),
                "span": span, "cli_rank": cli_rank if cli_rank and cli_rank > DEPTH else None,
                "tightest_record": min(tight) if tight else 0,
                "fused_rank": where(row["fused"]), "dense_rank": where(row["dense"]),
                "lexical_rank": where(row["lexical"]),
                "top": [[h[0], h[1], h[2]] for h in hits10[:3]],
                "tests_in_top10": sum(1 for h in hits10 if TESTISH.search(h[0])),
                "comment_share": comment / max(1, len(body)),
                "body_words": len(re.findall(r"[A-Za-z]{3,}", "\n".join(body))),
                "query_words_in_body": sorted(set(extract.content_words(q["query"])) & set(re.findall(r"[a-z0-9]+", "\n".join(file_text[t["start"] - 1 : t["end"]]).lower()))),
            })
    counts = {}
    for f in out:
        f["class"] = classify(f)
        f["bucket"] = "11-50" if f["cli_rank"] else "beyond 50"
        key = (f["class"], f["bucket"])
        counts[key] = counts.get(key, 0) + 1
    for (k, b), v in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"{v:4d}  {k}  [{b}]")
    path = ctx.work / "results" / f"failures-{system}-{qset}.json"
    write_json(path, out)
    print(f"{len(out)} misses written to {path}")


# --- main ---------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["fetch", "prepare", "validate", "index", "run", "report", "failures"])
    ap.add_argument("--work", default=os.environ.get("RIPSEM_EVAL_DIR", str(Path.home() / "ripsem-eval")),
                    help="directory for corpora, indexes and results (default: ~/ripsem-eval)")
    ap.add_argument("--ripsem", help="the ripsem binary (default: ripsem on PATH, else target/release)")
    ap.add_argument("--arms", help="the ripsem-eval-arms binary built from eval/arms")
    ap.add_argument("--rg", help="ripgrep (default: rg on PATH)")
    ap.add_argument("--repo", action="append", help="limit to a corpus (repeatable)")
    ap.add_argument("--systems", type=lambda s: s.split(","), default=list(SYSTEMS))
    ap.add_argument("--sets", type=lambda s: s.split(","), default=list(SETS))
    ap.add_argument("--backends", type=lambda s: s.split(","), default=BACKENDS)
    ap.add_argument("--repeats", type=int, default=5, help="index builds to time per corpus and backend")
    ap.add_argument("--system", default="fastembed", help="failures: the system to analyse")
    ap.add_argument("--set", default="curated", help="failures: the query set")
    ap.add_argument("--sample", type=int, default=0, help="failures: sample this many misses per corpus")
    args = ap.parse_args()
    ctx = Ctx(args)
    globals()["cmd_" + args.command](ctx)


if __name__ == "__main__":
    main()
