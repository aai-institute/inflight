use std::sync::Mutex;
use std::thread;

use inflight::{try_run, Closed, Ended, Flag, Observer, Opened, Outcome, Reports, Scope, Stop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Named(&'static str);

impl Reports for Named {
    type Event = ();
}

#[derive(Debug, PartialEq)]
enum Failure {
    Bad,
    Stopped(Stop),
}

impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        Failure::Stopped(stop)
    }
}

#[derive(Default)]
struct Ends(Mutex<Vec<(&'static str, Ended)>>);

struct End<'e>(&'e Ends, &'static str);

impl Observer<Named> for Ends {
    fn check(&self) -> std::ops::ControlFlow<Stop> {
        std::ops::ControlFlow::Continue(())
    }
    fn open(&self, scope: &Named) -> Opened<'_, Named> {
        Opened::With(Box::new(End(self, scope.0)))
    }
}

impl Observer<Named> for End<'_> {
    fn closed(&self, closed: Closed) {
        self.0 .0.lock().unwrap().push((self.1, closed.ended));
    }
}

fn blocks(call: &Scope<'_, Named>, flag: &Flag) -> Result<(), Failure> {
    thread::scope(|threads| {
        let a = threads.spawn(|| {
            call.within(Named("a"), |a| {
                flag.raise(Stop::Signal);
                a.check().map_err(Failure::from)
            })
        });
        let a = a.join().unwrap();
        let b = threads.spawn(|| call.within(Named("b"), |_| Err::<(), _>(Failure::Bad)));
        let b = b.join().unwrap();
        b.and(a)
    })
}

#[test]
fn a_parallel_blocks_own_error_survives_a_sibling_stop() {
    let flag = Flag::new();
    let ends = Ends::default();
    let result = try_run(&(&ends, &flag), |call| blocks(call, &flag));
    assert_eq!(result, Err(Failure::Bad));
    let ends = ends.0.into_inner().unwrap();
    assert_eq!(
        ends,
        [("a", Ended::Stopped(Stop::Signal)), ("b", Ended::Failed)]
    );
}

#[test]
fn an_error_that_is_the_stop_still_stops_the_call() {
    let flag = Flag::new();
    let result = try_run(&flag, |call| {
        call.within(Named("a"), |a| {
            flag.raise(Stop::Signal);
            a.check().map_err(Failure::from)
        })
    });
    assert_eq!(
        result,
        Ok(Outcome::Stopped {
            reason: Stop::Signal,
            residue: ()
        })
    );
}

impl inflight::Stopping for Failure {
    fn stop(&self) -> Option<Stop> {
        match self {
            Failure::Stopped(stop) => Some(*stop),
            Failure::Bad => None,
        }
    }
}
