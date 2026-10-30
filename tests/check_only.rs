use inflight::{run, Deadline, Flag, Observer, Outcome, Stop};
use std::convert::Infallible;
use std::time::Duration;

pub fn sum(xs: &[f64], control: &dyn Observer<Infallible>) -> Outcome<f64> {
    run(control, |call| {
        let mut total = 0.0;
        for (k, x) in xs.iter().enumerate() {
            call.check()?;
            call.progress(k as u64 + 1, Some(xs.len() as u64));
            total += x;
        }
        Ok(total)
    })
}

#[test]
fn a_check_only_library_takes_any_control() {
    assert_eq!(sum(&[1.0, 2.0], &()), Outcome::Completed(3.0));
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    assert_eq!(
        sum(&[1.0], &(&flag, Deadline::after(Duration::MAX))),
        Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        }
    );
}
