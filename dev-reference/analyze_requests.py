#!/usr/bin/env python3
"""Analyze request-recording JSONL for cache viability.

Usage: python analyze_requests.py <path/to/requests.jsonl> [--top N]

Prints (over outcome=="ok" rows only — only cacheable responses count):
  - exact-duplicate rate   (hash_full)  -> full-response cache hit-rate ceiling
  - content-duplicate rate (hash_messages)
  - top-N most-repeated keys
  - retry vs concurrent-dup split (same hash_full within 2s = retry)
"""
import argparse
import collections
import json


def load(path):
    rows = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def dup_rate(rows, key):
    counts = collections.Counter(r[key] for r in rows)
    total = sum(counts.values())
    unique = len(counts)
    hits = total - unique  # every repeat beyond the first is a would-be cache hit
    return unique, total, hits, counts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--top", type=int, default=10)
    args = ap.parse_args()

    rows = load(args.file)
    ok = [r for r in rows if r.get("outcome") == "ok"]
    print(f"total records: {len(rows)} | cacheable (outcome==ok): {len(ok)}")
    if not ok:
        print("no cacheable rows; nothing to analyze.")
        return

    for label, key in [("exact (hash_full)", "hash_full"), ("content (hash_messages)", "hash_messages")]:
        unique, total, hits, counts = dup_rate(ok, key)
        rate = hits / total if total else 0.0
        print(f"\n{label}: {unique} unique / {total} total -> dup rate {rate:.2%} ({hits} repeat hits)")
        print(f"  top {args.top}:")
        for h, c in counts.most_common(args.top):
            if c > 1:
                print(f"    {c}x  {h}")

    # retry vs concurrent-dup split (exact), by hash_full
    by_key = collections.defaultdict(list)
    for r in ok:
        by_key[r["hash_full"]].append(r.get("ts", ""))
    retries, concurrent = 0, 0
    for _, ts_list in by_key.items():
        if len(ts_list) < 2:
            continue
        # crude: if any two within 2s, call the group retries
        ts_sorted = sorted(ts_list)
        paired = False
        for a, b in zip(ts_sorted, ts_sorted[1:]):
            if (parse_ts(b) - parse_ts(a)) <= 2.0:
                paired = True
                break
        if paired:
            retries += len(ts_list) - 1
        else:
            concurrent += len(ts_list) - 1
    print(f"\nrepeats split: ~{retries} retry-like | ~{concurrent} concurrent-like (exact hash_full)")


def parse_ts(s):
    # ISO 8601 local; fall back to 0 on parse error so ordering still works
    try:
        from datetime import datetime
        return datetime.fromisoformat(s).timestamp()
    except Exception:
        return 0.0


if __name__ == "__main__":
    main()
