# floor.py <compare.log>: noise floor per estimator. For each configuration, per-round ratio B/A by
# estimator (median of samples, min, 25th pct); then the pooled ratio = median over rounds, and
# |pooled-1|. Summarised by thread count: median and max over configurations.
import sys, re, statistics as st
from collections import defaultdict
runs = defaultdict(lambda: defaultdict(list))
side = None
for line in open(sys.argv[1]):
    if line.startswith('#'):
        m = re.search(r"side=(\d)", line)
        if m: side = int(m.group(1))
        continue
    m = dict(re.findall(r'(\w+)=(\S+)', line))
    key = (m['loop'], int(m['n']), int(m['threads']), m['budget'])
    runs[key][side].append([int(x) for x in m['ns'].split(',')])
def q(xs, p): xs = sorted(xs); return xs[round((len(xs)-1)*p)]
ests = {'median': lambda s: q(s, .5), 'p25': lambda s: q(s, .25), 'min': min}
for name, f in ests.items():
    by_t = defaultdict(list); worst = defaultdict(list)
    for key, sides in runs.items():
        r = [f(b)/f(a) for a, b in zip(sides[0], sides[1])]
        by_t[key[2]].append(abs(st.median(r)-1)*100)
        worst[key[2]].append(st.median([abs(x-1)*100 for x in r]))
    print(name, ' '.join(f"t={t}: pooled |B/A-1| med {st.median(v):.1f}% max {max(v):.1f}%, one-round med {st.median(worst[t]):.1f}%" for t, v in sorted(by_t.items())))
if len(sys.argv) > 2:
    f = ests[sys.argv[2]]
    for key, sides in sorted(runs.items(), key=lambda kv: (kv[0][2], kv[0][1], kv[0][3], kv[0][0])):
        r = [f(b)/f(a) for a, b in zip(sides[0], sides[1])]
        print(f"  {key[0]:15} {key[1]:>9} t={key[2]:>2} {key[3]:6} base_min_us={st.median([min(a) for a in sides[0]])/1e3:9.1f} pooled={abs(st.median(r)-1)*100:5.1f}% one-round={st.median([abs(x-1)*100 for x in r]):5.1f}%")
