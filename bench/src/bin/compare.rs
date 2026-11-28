//! Times the loops of a base commit against the working tree, interleaved:
//! `compare [base=HEAD] [rounds=…] [interleave=sample|config|run] [log=target/compare.log] [harness arguments…]`.
//!
//! Both sides use this crate's harness, so only the library differs. `sample` (the default) links
//! both versions into one binary and alternates their calls; `config` and `run` alternate two
//! binaries per configuration or per whole grid. The order flips every time, so drift in the
//! machine's load falls on both alike. Each round compares the sides by their fastest sample,
//! which load can only slow; the rounds' ratios give a Hodges-Lehmann estimate of candidate over
//! base and its signed-rank interval at 99%, and a configuration is flagged when the interval
//! leaves out 1.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use inflight_bench::{BUDGETS, LOOPS, SIZES, THREADS};

const BENCH: &str = env!("CARGO_MANIFEST_DIR");

fn main() {
    let mut base = "HEAD".to_owned();
    let mut rounds = 0usize;
    let mut log = PathBuf::from(BENCH).join("target/compare.log");
    let mut pass = Vec::new();
    let mut mode = "sample".to_owned();
    let mut sizes: Vec<String> = SIZES.iter().map(usize::to_string).collect();
    for a in std::env::args().skip(1) {
        match a.split_once('=') {
            Some(("base", v)) => v.clone_into(&mut base),
            Some(("interleave", v)) => v.clone_into(&mut mode),
            Some(("sizes", v)) => {
                sizes = v.split(',').map(str::to_owned).collect();
                pass.push(a);
            }
            Some(("rounds", v)) => rounds = v.parse().unwrap(),
            Some(("log", v)) => log = v.into(),
            _ => pass.push(a),
        }
    }
    let target = PathBuf::from(BENCH).join("target");
    if mode == "sample" {
        let rounds = if rounds == 0 { 10 } else { rounds };
        let (seen, checks) = run_pair(&base, &target, rounds, &pass, &log);
        report(&seen, &checks);
        return;
    }
    let rounds = if rounds == 0 { 10 } else { rounds };
    let per_config = mode == "config";
    let base_bin = build_base(&base, &target);
    let cand_bin = build(Path::new(BENCH), &target.join("candidate"));
    let mut log_file = std::fs::File::create(&log).unwrap();
    let started = Instant::now();
    // (configuration) -> side -> per-round samples
    let (mut seen, mut checks) = (Configs::new(), Checks::new());
    // Each job is one run of each side: a configuration, or the whole grid.
    let jobs: Vec<Vec<String>> = if per_config {
        let mut jobs = Vec::new();
        for n in &sizes {
            for t in THREADS {
                for b in BUDGETS {
                    for lp in LOOPS {
                        let mut args: Vec<String> = pass
                            .iter()
                            .filter(|a| !a.starts_with("sizes="))
                            .cloned()
                            .collect();
                        args.extend([
                            format!("sizes={n}"),
                            format!("threads={t}"),
                            format!("budgets={b}"),
                            format!("loops={lp}"),
                        ]);
                        jobs.push(args);
                    }
                }
            }
        }
        jobs
    } else {
        vec![pass.clone()]
    };
    let total = 2 * rounds * jobs.len();
    let mut done = 0;
    for r in 0..rounds {
        let load = uptime_load();
        writeln!(log_file, "# round {}/{rounds} load={load}", r + 1).unwrap();
        for (j, args) in jobs.iter().enumerate() {
            let order = if (r + j) % 2 == 0 { [0, 1] } else { [1, 0] };
            for side in order {
                let bin = [&base_bin, &cand_bin][side];
                let out = Command::new(bin)
                    .arg(format!("label={}", ["base", "candidate"][side]))
                    .args(args)
                    .stderr(Stdio::inherit())
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{} failed", bin.display());
                let text = String::from_utf8(out.stdout).unwrap();
                writeln!(log_file, "# side={side}").unwrap();
                log_file.write_all(text.as_bytes()).unwrap();
                for line in text.lines() {
                    let (key, check, ns) = parse(line);
                    seen.entry(key.clone()).or_default()[side].push(ns);
                    checks.entry(key).or_default()[side] = check;
                }
                done += 1;
            }
        }
        let per = started.elapsed().as_secs_f64() / done as f64;
        eprintln!(
            "round {}/{rounds}: {:.0} s elapsed, ~{:.0} s left, load {load}",
            r + 1,
            started.elapsed().as_secs_f64(),
            per * (total - done) as f64,
        );
    }
    report(&seen, &checks);
}

/// The configurations' rounds, one process running both versions, its base renamed so the two
/// link side by side. Both versions' loops are compiled in one crate and one codegen unit, so
/// neither is optimized differently for where it sits.
fn run_pair(
    rev: &str,
    target: &Path,
    rounds: usize,
    pass: &[String],
    log: &Path,
) -> (Configs, Checks) {
    let repo = Path::new(BENCH).parent().unwrap();
    let sha = git(repo, &["rev-parse", "--short", rev]);
    let base = target.join(format!("pair-base-{sha}"));
    if !base.exists() {
        unpack(repo, &sha, &base);
        let manifest = base.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        let renamed = text.replacen("name = \"inflight\"", "name = \"inflight-base\"", 1);
        assert_ne!(text, renamed, "the base's package name");
        std::fs::write(&manifest, renamed).unwrap();
    }
    let krate = target.join("pair");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    let manifest = format!(
        "[package]\nname = \"inflight-pair\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\nlicense = \"MIT\"\n\n\
         [dependencies]\ninflight-bench = {{ path = {bench:?} }}\n\
         inflight-base = {{ path = {base:?}, features = [\"rayon\"] }}\n\
         inflight = {{ path = {repo:?}, features = [\"rayon\"] }}\n\n\
         [profile.release]\ndebug = false\ncodegen-units = 1\n\n[workspace]\n",
        bench = BENCH,
        base = base.display().to_string(),
        repo = repo.display().to_string(),
    );
    std::fs::write(krate.join("Cargo.toml"), manifest).unwrap();
    let main = format!(
        "mod base {{\n    use ::inflight_base as krate;\n    include!({work:?});\n}}\n\n\
         mod candidate {{\n    use ::inflight as krate;\n    include!({work:?});\n}}\n\n\
         fn main() {{\n    inflight_bench::pair::pair(base::run, candidate::run);\n}}\n",
        work = Path::new(BENCH).join("src/work.rs").display().to_string(),
    );
    std::fs::write(krate.join("src/main.rs"), main).unwrap();
    let ok = Command::new("cargo")
        .args(["build", "--release", "--quiet", "--manifest-path"])
        .arg(krate.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.join("pair-target"))
        .status()
        .unwrap();
    assert!(ok.success(), "building the pair");
    eprintln!("base {rev} = {sha}, load {}", uptime_load());
    // A process places each version's code anew, which moves its time by up to a few percent at
    // one thread: each round is its own process, so the interval counts that variation too.
    let mut log_file = std::fs::File::create(log).unwrap();
    let (mut seen, mut checks) = (Configs::new(), Checks::new());
    let started = Instant::now();
    for r in 0..rounds {
        let load = uptime_load();
        let out = Command::new(target.join("pair-target/release/inflight-pair"))
            .args(pass)
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "the pair failed");
        let text = String::from_utf8(out.stdout).unwrap();
        writeln!(log_file, "# round {}/{rounds} load={load}", r + 1).unwrap();
        log_file.write_all(text.as_bytes()).unwrap();
        for line in text.lines() {
            let mut key = Vec::new();
            let mut sides: [Vec<u64>; 2] = [vec![], vec![]];
            let mut check = [String::new(), String::new()];
            for field in line.split(' ').skip(1) {
                match field.split_once('=') {
                    Some(("check", v)) => {
                        let (a, b) = v.split_once(',').unwrap();
                        check = [a.to_owned(), b.to_owned()];
                    }
                    Some(("base_ns", v)) => sides[0] = numbers(v),
                    Some(("candidate_ns", v)) => sides[1] = numbers(v),
                    _ => key.push(field),
                }
            }
            let key = key.join(" ");
            let [a, b] = sides;
            let entry = seen.entry(key.clone()).or_default();
            entry[0].push(a);
            entry[1].push(b);
            checks.insert(key, check);
        }
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!(
            "round {}/{rounds}: {elapsed:.0} s elapsed, ~{:.0} s left, load {load}",
            r + 1,
            elapsed / (r + 1) as f64 * (rounds - r - 1) as f64
        );
    }
    (seen, checks)
}

fn numbers(v: &str) -> Vec<u64> {
    v.split(',').map(|x| x.parse().unwrap()).collect()
}

/// Each configuration's samples per round, base and candidate.
type Configs = BTreeMap<String, [Vec<Vec<u64>>; 2]>;
/// Each configuration's result checksums, base and candidate.
type Checks = BTreeMap<String, [String; 2]>;

fn unpack(repo: &Path, sha: &str, tree: &Path) {
    std::fs::create_dir_all(tree).unwrap();
    let mut archive = Command::new("git")
        .current_dir(repo)
        .args(["archive", sha])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let ok = Command::new("tar")
        .args(["-x", "-C"])
        .arg(tree)
        .stdin(archive.stdout.take().unwrap())
        .status()
        .unwrap();
    assert!(
        ok.success() && archive.wait().unwrap().success(),
        "unpacking {sha}"
    );
}

/// The base commit's library with this crate's harness, built once per commit.
fn build_base(rev: &str, target: &Path) -> PathBuf {
    let repo = Path::new(BENCH).parent().unwrap();
    let sha = git(repo, &["rev-parse", "--short", rev]);
    let tree = target.join(format!("base-{sha}"));
    if !tree.exists() {
        unpack(repo, &sha, &tree);
    }
    // The harness is always the working tree's, so only the library differs.
    let bench = tree.join("bench");
    std::fs::create_dir_all(bench.join("src/bin")).unwrap();
    for f in [
        "Cargo.toml",
        "src/lib.rs",
        "src/work.rs",
        "src/pair.rs",
        "src/bin/loops.rs",
    ] {
        std::fs::copy(Path::new(BENCH).join(f), bench.join(f)).unwrap();
    }
    if Path::new(BENCH).join("Cargo.lock").exists() {
        std::fs::copy(
            Path::new(BENCH).join("Cargo.lock"),
            bench.join("Cargo.lock"),
        )
        .unwrap();
    }
    eprintln!("base {rev} = {sha}");
    build(&bench, &target.join(format!("base-{sha}-target")))
}

fn build(bench: &Path, target: &Path) -> PathBuf {
    let ok = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--quiet",
            "--bin",
            "loops",
            "--manifest-path",
        ])
        .arg(bench.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target)
        .status()
        .unwrap();
    assert!(ok.success(), "building {}", bench.display());
    target.join("release/loops")
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn uptime_load() -> String {
    let out = Command::new("uptime").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let load = text.rsplit(':').next().unwrap_or("").trim();
    load.split([' ', ',']).next().unwrap_or("").to_owned()
}

/// A line's configuration, its checksum and its samples.
fn parse(line: &str) -> (String, String, Vec<u64>) {
    let mut key = Vec::new();
    let (mut check, mut ns) = (String::new(), Vec::new());
    for field in line.split(' ').skip(1) {
        match field.split_once('=') {
            Some(("check", v)) => v.clone_into(&mut check),
            Some(("ns", v)) => ns = v.split(',').map(|x| x.parse().unwrap()).collect(),
            _ => key.push(field),
        }
    }
    (key.join(" "), check, ns)
}

fn quantile(xs: &[u64], p: f64) -> f64 {
    let mut xs = xs.to_vec();
    xs.sort_unstable();
    xs[((xs.len() - 1) as f64 * p).round() as usize] as f64
}

fn median_f(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    (xs[(n - 1) / 2] + xs[n / 2]) / 2.0
}

/// The Hodges-Lehmann estimate of the median of `d` and its signed-rank interval at 99%, from the
/// Walsh averages; the interval spans all of them when `d` is too short for 99%.
fn hodges_lehmann(d: &[f64]) -> (f64, f64, f64) {
    let n = d.len();
    let mut w: Vec<f64> = (0..n)
        .flat_map(|i| (i..n).map(move |j| (i, j)))
        .map(|(i, j)| (d[i] + d[j]) / 2.0)
        .collect();
    w.sort_by(f64::total_cmp);
    // ways[t]: subsets of 1..=n with rank sum t, the signed-rank statistic's null distribution.
    let top = n * (n + 1) / 2;
    let mut ways = vec![0f64; top + 1];
    ways[0] = 1.0;
    for r in 1..=n {
        for t in (r..=top).rev() {
            ways[t] += ways[t - r];
        }
    }
    let all = 2f64.powi(n as i32);
    // The largest k with P(T < k) <= 0.005: the interval is the k-th Walsh average from each end.
    let (mut k, mut below) = (0, 0.0);
    while k <= top && (below + ways[k]) / all <= 0.005 {
        below += ways[k];
        k += 1;
    }
    let k = k.max(1);
    (median_f(w.clone()), w[k - 1], w[w.len() - k])
}

fn report(seen: &Configs, checks: &Checks) {
    println!(
        "{:58} {:>11} {:>7} {:>17} {:>6} bits",
        "configuration", "base_min_us", "B/A", "99% interval", "flag"
    );
    let (mut flagged, mut differ) = (0, 0);
    for (key, [a, b]) in seen {
        let d: Vec<f64> = (a.iter().zip(b))
            .map(|(a, b)| (quantile(b, 0.0) / quantile(a, 0.0)).ln())
            .collect();
        let (est, lo, hi) = hodges_lehmann(&d);
        let base_min = median_f(a.iter().map(|s| quantile(s, 0.0)).collect());
        let flag = if lo > 0.0 {
            "slower"
        } else if hi < 0.0 {
            "faster"
        } else {
            ""
        };
        flagged += usize::from(!flag.is_empty());
        let [ca, cb] = &checks[key];
        differ += usize::from(ca != cb);
        println!(
            "{key:58} {:>11.1} {:>7.3} {:>17} {flag:>6} {}",
            base_min / 1e3,
            est.exp(),
            format!("[{:.3}..{:.3}]", lo.exp(), hi.exp()),
            if ca == cb { "same" } else { "DIFFER" },
        );
    }
    println!(
        "{flagged} of {} configurations flagged; results differ in {differ}",
        seen.len()
    );
}
