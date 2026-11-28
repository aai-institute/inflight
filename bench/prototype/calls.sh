#!/bin/bash
# calls.sh: 1M, 4 and 12 threads, pair mode, calls=1 vs 10 vs 30, identical source then fold_tree S=100,
# two repeats, alternating calls settings.
P="$(cd "$(dirname "$0")" && pwd)"
cd "$P/../.."
for S in 0 100; do
  git show HEAD:src/parallel/split.rs > src/parallel/split.rs
  if [ $S != 0 ]; then python3 - $S <<'PY'
import sys; S=sys.argv[1]; p='src/parallel/split.rs'; s=open(p).read()
old='''    if n <= 1 {
        return (fold.leaf)(input.into_item());'''
s=s.replace(old, '''    if n <= 1 {
        for i in 0..%s_u32 { std::hint::black_box(i); }
        return (fold.leaf)(input.into_item());''' % S)
open(p,'w').write(s)
PY
  fi
  for rep in a b; do for c in 1 10 30; do
    echo "# S=$S calls=$c rep=$rep $(date +%T) load $(uptime | sed 's/.*averages: //')" >> bench/target/calls5.progress
    (cd bench && ./target/release/compare sizes=1048576 threads=4,12 calls=$c log=target/c5-$S-$c-$rep.log > target/c5-$S-$c-$rep.txt 2>/dev/null)
  done; done
done
git show HEAD:src/parallel/split.rs > src/parallel/split.rs
