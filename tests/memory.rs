use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use inflight::{Control, MemoryCap, Stop};

const MIB: usize = 1 << 20;
const INTERVAL: Duration = Duration::from_millis(1);

#[test]
fn a_cap_below_the_process_stops_at_once() {
    let cap = MemoryCap::start(0, INTERVAL).expect("this platform reports resident memory");
    assert_eq!(cap.check(), ControlFlow::Break(Stop::Memory));
}

#[test]
fn growing_past_the_cap_stops_a_running_call() {
    let cap = MemoryCap::start(256 * MIB, INTERVAL).expect("this platform reports resident memory");
    assert_eq!(
        cap.check(),
        ControlFlow::Continue(()),
        "the test process starts below the cap"
    );
    // Ones, not zeros: zeroed pages may stay unmapped and never count as resident.
    let grown = vec![1u8; 512 * MIB];
    let started = Instant::now();
    while cap.check().is_continue() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no stop with {} MiB live",
            grown.len() / MIB
        );
        std::thread::sleep(INTERVAL);
    }
    assert_eq!(cap.check(), ControlFlow::Break(Stop::Memory));
}

#[test]
fn dropping_a_cap_does_not_wait_out_its_interval() {
    let cap = MemoryCap::start(usize::MAX, Duration::from_secs(60))
        .expect("this platform reports resident memory");
    let started = Instant::now();
    drop(cap);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}
