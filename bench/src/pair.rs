//! Two versions of the library timed in one process, their calls alternating on the same pool and
//! data, so the machine's state falls on both alike.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use crate::{Data, BUDGETS, LOOPS, SIZES, THREADS};

/// One call of a loop on one version: loop, budget, data, returning a checksum.
pub type Run = fn(&str, &str, &mut Data) -> u64;

/// For each configuration, `samples` calls of each version, alternating call by call with the
/// order flipped every pair; one line per configuration with both versions' samples in call
/// order, flushed: `pair [samples=20] [warmup=3] [calls=1] [sizes=…] [threads=…] [budgets=…]
/// [loops=…]`. A sample times `calls` calls in a row and reports the time per call.
///
/// # Panics
/// On an argument not of the form `key=value`, or a number that does not parse.
pub fn pair(base: Run, candidate: Run) {
    let args: HashMap<String, String> = std::env::args()
        .skip(1)
        .map(|a| {
            let (k, v) = a.split_once('=').expect("arguments as key=value");
            (k.to_owned(), v.to_owned())
        })
        .collect();
    let get = |k: &str, default: usize| args.get(k).map_or(default, |v| v.parse().unwrap());
    let (samples, warmup, calls) = (get("samples", 20), get("warmup", 3), get("calls", 1));
    let list = |k: &str, all: Vec<String>| -> Vec<String> {
        args.get(k)
            .map_or(all, |v| v.split(',').map(str::to_owned).collect())
    };
    let names = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect();
    let numbers = |xs: &[usize]| xs.iter().map(usize::to_string).collect();
    let sizes = list("sizes", numbers(&SIZES));
    let threads = list("threads", numbers(&THREADS));
    let (budgets, loops) = (
        list("budgets", names(&BUDGETS)),
        list("loops", names(&LOOPS)),
    );
    let started = Instant::now();
    let total = sizes.len() * threads.len() * budgets.len() * loops.len();
    let mut done = 0;
    let mut out = std::io::stdout().lock();
    for n in &sizes {
        let mut d = Data::new(n.parse().unwrap());
        for t in &threads {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(t.parse().unwrap())
                .build()
                .unwrap();
            for b in &budgets {
                for lp in &loops {
                    let (mut ns, mut check) = ([vec![], vec![]], [0u64; 2]);
                    pool.install(|| {
                        let versions = [base, candidate];
                        for _ in 0..warmup {
                            for v in 0..2 {
                                check[v] = check[v].wrapping_add(versions[v](lp, b, &mut d));
                            }
                        }
                        for k in 0..samples {
                            for v in [k % 2, 1 - k % 2] {
                                let t0 = Instant::now();
                                for _ in 0..calls {
                                    let c = versions[v](lp, b, &mut d);
                                    check[v] = check[v].wrapping_add(c);
                                }
                                ns[v].push(t0.elapsed().as_nanos() as u64 / calls as u64);
                            }
                        }
                    });
                    let join = |v: &[u64]| {
                        let s: Vec<String> = v.iter().map(u64::to_string).collect();
                        s.join(",")
                    };
                    writeln!(
                        out,
                        "pair loop={lp} n={n} threads={t} budget={b} check={:x},{:x} base_ns={} candidate_ns={}",
                        check[0], check[1], join(&ns[0]), join(&ns[1])
                    )
                    .unwrap();
                    out.flush().unwrap();
                    done += 1;
                    let elapsed = started.elapsed().as_secs_f64();
                    eprintln!(
                        "{done}/{total} configurations: {elapsed:.0} s elapsed, ~{:.0} s left",
                        elapsed / done as f64 * (total - done) as f64
                    );
                }
            }
        }
    }
}
