# summ.py <compare output>...: per thread count, |B/A-1| median (max) and 99% interval half-width median (max).
import sys, re, statistics as st
from collections import defaultdict
for path in sys.argv[1:]:
    e = defaultdict(list); h = defaultdict(list); n = 0; fl = 0
    for line in open(path):
        m = re.search(r'threads=(\d+).*?\s([\d.]+)\s+\[([\d.]+)\.\.([\d.]+)\]\s*(\w*)\s', line)
        if not m: continue
        t = int(m.group(1)); est, lo, hi = map(float, m.group(2, 3, 4))
        e[t].append(abs(est-1)*100); h[t].append((hi-lo)/2*100); n += 1; fl += m.group(5) in ('slower', 'faster')
    print(path.split('/')[-1], f"flagged {fl}/{n}:", '; '.join(f"t={t} |B/A-1| {st.median(e[t]):.1f} ({max(e[t]):.1f})% half-width {st.median(h[t]):.1f} ({max(h[t]):.1f})%" for t in sorted(e)))
