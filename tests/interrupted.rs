use inflight::{run_fallible, Flag, Interrupted, Outcome, Reports, Scope, Stop};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Solve;
impl Reports for Solve {
    type Event = ();
}

// The algorithm's own error: no stop variant anywhere in it.
#[derive(Debug, PartialEq)]
enum Singular {
    At(usize),
}

impl From<Singular> for Interrupted<Singular> {
    fn from(error: Singular) -> Self {
        Self::Own(error)
    }
}

fn pivot(k: usize) -> Result<(), Singular> {
    match k {
        3 => Err(Singular::At(k)),
        _ => Ok(()),
    }
}

fn eliminate(call: &Scope<'_, Solve>, n: usize) -> Result<usize, Interrupted<Singular>> {
    for k in 0..n {
        call.check()?;
        pivot(k)?;
    }
    Ok(n)
}

#[test]
fn only_the_algorithms_own_error_comes_out() {
    assert_eq!(
        run_fallible(&(), |call| eliminate(call, 3)),
        Ok(Outcome::Completed(3))
    );
    assert_eq!(
        run_fallible(&(), |call| eliminate(call, 5)),
        Err(Singular::At(3))
    );
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    assert_eq!(
        run_fallible(&flag, |call| eliminate(call, 5)),
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        })
    );
}

#[test]
fn the_own_error_wins_over_a_stop_already_latched() {
    let flag = Flag::new();
    let result = run_fallible(&flag, |call: &Scope<'_, Solve>| -> Result<(), _> {
        flag.raise(Stop::Requested);
        let _ = call.check();
        Err(Interrupted::Own(Singular::At(0)))
    });
    assert_eq!(result, Err(Singular::At(0)));
}
