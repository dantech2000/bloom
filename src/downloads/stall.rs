// SPDX-License-Identifier: AGPL-3.0-or-later
//! A read that waits too long on a download. ureq 3 has a timeout for the
//! whole body, but none for the time between two reads, and a blocked read
//! does not see a cancel. This connector wraps the transport of ureq: a
//! read waits in slices of one second, so a cancel works within a second,
//! and a read that gets no byte for `stall` ends with a timeout error. The
//! download then goes on with its retry and a `Range` request.

use std::{
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};

use ureq::{
    Error,
    unversioned::transport::{Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport},
};

/// How long one wait for input lasts before the transport looks at the
/// cancel flag again.
const SLICE: Duration = Duration::from_secs(1);

/// An agent for the files of the downloads: the settings of `config`, a
/// stall limit on every read and a read that stops at `cancel`.
pub fn agent(config: ureq::config::Config, stall: Duration, cancel: Arc<AtomicBool>) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        DefaultConnector::new().chain(Watch { stall, cancel }),
        ureq::unversioned::resolver::DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct Watch {
    stall: Duration,
    cancel: Arc<AtomicBool>,
}

impl Connector<Box<dyn Transport>> for Watch {
    type Out = Watched;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, Error> {
        Ok(chained.map(|inner| Watched { inner, stall: self.stall, cancel: self.cancel.clone() }))
    }
}

#[derive(Debug)]
struct Watched {
    inner: Box<dyn Transport>,
    stall: Duration,
    cancel: Arc<AtomicBool>,
}

/// What one wait does next, for the time already waited.
#[derive(Debug, PartialEq)]
enum Wait {
    /// Wait this long for input.
    For(Duration),
    /// Give up: the stall limit or the limit of ureq is over, or the user
    /// cancelled.
    Stop,
}

/// The next slice of a wait. `limit` is the time that ureq allows for this
/// read (`None`: no limit).
fn next_wait(waited: Duration, stall: Duration, limit: Option<Duration>, cancelled: bool) -> Wait {
    if cancelled {
        return Wait::Stop;
    }
    let mut left = stall.saturating_sub(waited);
    if let Some(limit) = limit {
        left = left.min(limit.saturating_sub(waited));
    }
    if left.is_zero() {
        return Wait::Stop;
    }
    Wait::For(left.min(SLICE))
}

impl Transport for Watched {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let limit = (!timeout.after.is_not_happening()).then(|| *timeout.after);
        let begin = Instant::now();
        loop {
            let slice = match next_wait(begin.elapsed(), self.stall, limit, self.cancel.load(Ordering::Acquire)) {
                Wait::For(slice) => slice,
                Wait::Stop => return Err(Error::Timeout(timeout.reason)),
            };
            let next = NextTimeout { after: slice.into(), reason: timeout.reason };
            match self.inner.await_input(next) {
                Err(Error::Timeout(_)) => continue,
                other => return other,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_comes_in_slices_until_the_stall_limit() {
        let stall = Duration::from_secs(30);
        assert_eq!(next_wait(Duration::ZERO, stall, None, false), Wait::For(SLICE));
        assert_eq!(next_wait(Duration::from_secs(29), stall, None, false), Wait::For(SLICE));
        assert_eq!(next_wait(Duration::from_millis(29_500), stall, None, false), Wait::For(Duration::from_millis(500)));
        assert_eq!(next_wait(stall, stall, None, false), Wait::Stop);
    }

    #[test]
    fn the_limit_of_ureq_and_a_cancel_end_the_wait() {
        let stall = Duration::from_secs(30);
        let limit = Some(Duration::from_secs(5));
        assert_eq!(next_wait(Duration::from_secs(5), stall, limit, false), Wait::Stop);
        assert_eq!(next_wait(Duration::from_secs(4), stall, limit, false), Wait::For(SLICE));
        assert_eq!(next_wait(Duration::ZERO, stall, None, true), Wait::Stop);
    }
}
