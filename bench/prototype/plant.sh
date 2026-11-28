#!/bin/bash
# plant.sh: for each planted spin S (0 = identical source), compare in config mode then pair mode.
# The spin runs in every leaf of fold_tree; the other loops are controls.
P="$(cd "$(dirname "$0")" && pwd)"
cd "$P/../.."
for S in 0 100 300; do
  git show HEAD:src/parallel/split.rs > src/parallel/split.rs
  if [ $S != 0 ]; then python3 - $S <<'PY'
import sys; S=sys.argv[1]; p='src/parallel/split.rs'; s=open(p).read()
old='''    if n <= 1 {
        return (fold.leaf)(input.into_item());'''
assert old in s
s=s.replace(old, '''    if n <= 1 {
        for i in 0..%s_u32 { std::hint::black_box(i); }
        return (fold.leaf)(input.into_item());''' % S)
open(p,'w').write(s)
PY
  fi
  for m in config sample; do
    echo "# S=$S mode=$m start $(date +%T) load $(uptime | sed 's/.*averages: //')" >> bench/target/plant3.progress
    (cd bench && ./target/release/compare interleave=$m log=target/p3-$m-$S.log > target/p3-$m-$S.txt 2>> target/plant3.err)
    echo "# S=$S mode=$m done $(date +%T)" >> bench/target/plant3.progress
  done
done
git show HEAD:src/parallel/split.rs > src/parallel/split.rs
echo "# all done" >> bench/target/plant3.progress
