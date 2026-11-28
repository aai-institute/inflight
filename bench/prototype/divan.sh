#!/bin/bash
# divan.sh: identical source then fold_tree S=100; 10 rounds, each a divan process then a pair process.
P="$(cd "$(dirname "$0")" && pwd)"
T="$P/../target"
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
  (cd $P/divan && cargo build --release --quiet)
  CARGO_TARGET_DIR=$T/pair-target cargo build --release --quiet --manifest-path $T/pair/Cargo.toml
  mkdir -p $T/dv-$S
  for r in $(seq 1 10); do
    echo "# S=$S round $r $(date +%T) load $(uptime | sed 's/.*averages: //')" >> $T/divan.progress
    if [ $((r % 2)) = 1 ]; then
      $P/divan/target/release/inflight-pair-divan --bench > $T/dv-$S/divan-$r.txt 2>&1
      $T/pair-target/release/inflight-pair > $T/dv-$S/pair-$r.txt 2>/dev/null
    else
      $T/pair-target/release/inflight-pair > $T/dv-$S/pair-$r.txt 2>/dev/null
      $P/divan/target/release/inflight-pair-divan --bench > $T/dv-$S/divan-$r.txt 2>&1
    fi
  done
done
git show HEAD:src/parallel/split.rs > src/parallel/split.rs
