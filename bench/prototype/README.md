# Prototype measurements (#37)

Scripts behind the answers on #37, as run on 12 cores (8P+4E), never merged. Each writes into
`bench/target/`; the planted slowdown is a spin in every leaf of `fold_tree`, the other loops are
controls.

- `plant.sh`: identical source, then spins of 100 and 300 iterations; `config` and `sample` modes alternated.
- `calls.sh`: 1M items on 4 and 12 threads, 1, 10 and 30 calls per sample.
- `divan.sh` with `divan/`: both versions as divan benchmarks against the paired harness, process by process.
- `summ.py` summarizes `compare`'s output; `dv.py` the divan/pair rounds; `floor.py` and `hl.py` the first logs.
