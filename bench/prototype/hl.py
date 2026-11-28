# hl.py <compare.log>... : per configuration, per-round ratio of fastest samples B/A; Hodges-Lehmann
# estimate of the log ratio with its signed-rank CI (n=10: W_(4)..W_(52), alpha 0.0098).
# Prints flagged configurations and per-thread median CI half-width.
import sys, re, math, statistics as st
from collections import defaultdict
K = {10: 4, 20: 38}   # lower index (1-based) for alpha ~0.01 two-sided
for path in sys.argv[1:]:
    runs = defaultdict(lambda: defaultdict(list)); side = None
    for line in open(path):
        if line.startswith('#'):
            m = re.search(r"side=(\d)", line)
            if m: side = int(m.group(1))
            continue
        m = dict(re.findall(r'(\w+)=(\S+)', line))
        runs[(m['loop'], int(m['n']), int(m['threads']), m['budget'])][side].append([int(x) for x in m['ns'].split(',')])
    flags = []; hw = defaultdict(list)
    for key, s in runs.items():
        d = [math.log(min(b)/min(a)) for a, b in zip(s[0], s[1])]
        w = sorted((d[i]+d[j])/2 for i in range(len(d)) for j in range(i, len(d)))
        k = K[len(d)]; lo, hi = math.exp(w[k-1]), math.exp(w[len(w)-k]); est = math.exp(st.median(w))
        hw[key[2]].append((hi-lo)/2*100)
        if lo > 1 or hi < 1: flags.append(f"{key[0]} {key[1]} t={key[2]} {key[3]} {est:.3f} [{lo:.3f}..{hi:.3f}]")
    print(path.split('/')[-1], f"flagged {len(flags)}/{len(runs)};", ' '.join(f"t={t}: CI half-width med {st.median(v):.1f}% max {max(v):.1f}%" for t, v in sorted(hw.items())))
    for f in flags: print('   ', f)
