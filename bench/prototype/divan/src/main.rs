// Both versions as divan benchmarks: each configuration an argument, each version a bench.
use std::sync::{Mutex, OnceLock};

use inflight_bench::{Data, BUDGETS, LOOPS, SIZES, THREADS};

mod base {
    use ::inflight_base as krate;
    include!("../../../src/work.rs");
}

mod candidate {
    use ::inflight as krate;
    include!("../../../src/work.rs");
}

fn configs() -> Vec<String> {
    let mut v = Vec::new();
    for n in SIZES {
        for t in THREADS {
            for b in BUDGETS {
                for lp in LOOPS {
                    v.push(format!("{lp}/{n}/{t}/{b}"));
                }
            }
        }
    }
    v
}

fn pool(t: usize) -> &'static rayon::ThreadPool {
    static POOLS: OnceLock<Vec<rayon::ThreadPool>> = OnceLock::new();
    let pools = POOLS.get_or_init(|| {
        THREADS
            .iter()
            .map(|&t| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(t)
                    .build()
                    .unwrap()
            })
            .collect()
    });
    &pools[THREADS.iter().position(|&x| x == t).unwrap()]
}

fn data(n: usize) -> &'static Mutex<Data> {
    static DATA: OnceLock<Vec<Mutex<Data>>> = OnceLock::new();
    let all = DATA.get_or_init(|| SIZES.iter().map(|&n| Mutex::new(Data::new(n))).collect());
    &all[SIZES.iter().position(|&x| x == n).unwrap()]
}

fn bench(bencher: divan::Bencher, config: &str, run: inflight_bench::pair::Run) {
    let parts: Vec<&str> = config.split('/').collect();
    let (lp, n, t, b) = (
        parts[0],
        parts[1].parse().unwrap(),
        parts[2].parse().unwrap(),
        parts[3],
    );
    let mut guard = data(n).lock().unwrap();
    let d: &mut Data = &mut guard;
    let pool = pool(t);
    bencher.bench_local(|| pool.install(|| run(lp, b, d)));
}

#[divan::bench(args = configs(), sample_count = 20)]
fn base(bencher: divan::Bencher, config: &str) {
    bench(bencher, config, base::run);
}

#[divan::bench(args = configs(), sample_count = 20)]
fn candidate(bencher: divan::Bencher, config: &str) {
    bench(bencher, config, candidate::run);
}

fn main() {
    divan::main();
}
