# dv.py <dir>: divan vs pair, per configuration the per-process ratio of fastest samples, HL estimate
# and 99% signed-rank interval over the 10 processes; summary per thread count and the flags.
import sys, re, math, glob, statistics as st
from collections import defaultdict
U = {'ns': 1, 'µs': 1e3, 'ms': 1e6, 's': 1e9}
def divan(path):
    side = None; out = {}
    for line in open(path):
        if re.search(r'[├╰]─ base', line): side = 0; continue
        if re.search(r'[├╰]─ candidate', line): side = 1; continue
        m = re.search(r'[├╰]─ (\S+)\s+([\d.]+) (ns|µs|ms|s)\s', line)
        if m and side is not None:
            lp, n, t, b = m.group(1).split('/')
            out.setdefault((lp, int(n), int(t), b), [0, 0])[side] = float(m.group(2)) * U[m.group(3)]
    return out
def pair(path):
    out = {}
    for line in open(path):
        f = dict(x.split('=', 1) for x in line.split()[1:])
        a = min(map(int, f['base_ns'].split(','))); b = min(map(int, f['candidate_ns'].split(',')))
        out[(f['loop'], int(f['n']), int(f['threads']), f['budget'])] = [a, b]
    return out
def hl(d):
    n = len(d); w = sorted((d[i]+d[j])/2 for i in range(n) for j in range(i, n)); k = 4  # n=10, 99%
    return st.median(w), w[k-1], w[len(w)-k]
for name, parse in [('divan', divan), ('pair', pair)]:
    rounds = [parse(p) for p in sorted(glob.glob(f"{sys.argv[1]}/{name}-*.txt"))]
    e = defaultdict(list); h = defaultdict(list); flags = []
    for key in rounds[0]:
        d = [math.log(r[key][1] / r[key][0]) for r in rounds]
        est, lo, hi = map(math.exp, hl(d))
        e[key[2]].append(abs(est-1)*100); h[key[2]].append((hi-lo)/2*100)
        if lo > 1 or hi < 1: flags.append(f"{key[0]} {key[1]} t={key[2]} {key[3]} {est:.3f} [{lo:.3f}..{hi:.3f}]")
    print(f"{name:5} ({len(rounds)} rounds) flagged {len(flags)}/{len(rounds[0])}:", '; '.join(f"t={t} |B/A-1| {st.median(e[t]):.1f} ({max(e[t]):.1f})% half-width {st.median(h[t]):.1f} ({max(h[t]):.1f})%" for t in sorted(e)))
    for f in flags: print('     ', f)
