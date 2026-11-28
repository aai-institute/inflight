//! Times each loop on the grid, one line per configuration with every sample, flushed:
//! `loops [label=…] [samples=20] [warmup=3] [calls=1] [sizes=…] [threads=…] [budgets=…] [loops=…]`,
//! each list comma-separated and the whole grid by default.
//! A sample times `calls` calls in a row and reports the time per call.
use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use inflight_bench::lib::run;
use inflight_bench::{Data, BUDGETS, LOOPS, SIZES, THREADS};

fn main() {
    let args: HashMap<String, String> = std::env::args()
        .skip(1)
        .map(|a| {
            let (k, v) = a.split_once('=').expect("arguments as key=value");
            (k.to_owned(), v.to_owned())
        })
        .collect();
    let get = |k: &str, default: usize| args.get(k).map_or(default, |v| v.parse().unwrap());
    let label = args.get("label").map_or("loops", String::as_str);
    let (samples, warmup, calls) = (get("samples", 20), get("warmup", 3), get("calls", 1));
    let list = |k: &str, all: &[&str]| -> Vec<String> {
        args.get(k)
            .map_or(all.iter().map(|s| (*s).to_owned()).collect(), |v| {
                v.split(',').map(str::to_owned).collect()
            })
    };
    let sizes: Vec<usize> = list(
        "sizes",
        &SIZES.map(|n| n.to_string()).each_ref().map(String::as_str),
    )
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();
    let threads: Vec<usize> = list(
        "threads",
        &THREADS
            .map(|n| n.to_string())
            .each_ref()
            .map(String::as_str),
    )
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();
    let (budgets, loops) = (list("budgets", &BUDGETS), list("loops", &LOOPS));
    let pools: Vec<_> = threads
        .iter()
        .map(|&t| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(t).build();
            (t, pool.unwrap())
        })
        .collect();
    let mut out = std::io::stdout().lock();
    for &n in &sizes {
        let mut d = Data::new(n);
        for (t, pool) in &pools {
            for b in &budgets {
                for lp in &loops {
                    let (ns, sum): (Vec<u64>, u64) = pool.install(|| {
                        let mut sum = 0u64;
                        for _ in 0..warmup * calls {
                            sum = sum.wrapping_add(run(lp, b, &mut d));
                        }
                        let ns = (0..samples)
                            .map(|_| {
                                let t0 = Instant::now();
                                for _ in 0..calls {
                                    sum = sum.wrapping_add(run(lp, b, &mut d));
                                }
                                t0.elapsed().as_nanos() as u64 / calls as u64
                            })
                            .collect();
                        (ns, sum)
                    });
                    let raw: Vec<String> = ns.iter().map(u64::to_string).collect();
                    writeln!(
                        out,
                        "{label} loop={lp} n={n} threads={t} budget={b} calls={calls} check={sum:x} ns={}",
                        raw.join(",")
                    )
                    .unwrap();
                    out.flush().unwrap();
                }
            }
        }
    }
}
