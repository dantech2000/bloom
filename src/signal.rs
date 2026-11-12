// SPDX-License-Identifier: AGPL-3.0-or-later
//! A signal with a queue for each reader: every reader gets every value.
//!
//! A static channel with one queue has one value for all its receivers.
//! The task of a window reads until it finds its window gone, so the task
//! of a window that closed took the value meant for the window that is
//! open (the status of the updater, a media key), and dropped it.

use std::sync::Mutex;

pub struct Signal<T> {
    /// How many values a reader that does not read can have waiting.
    capacity: usize,
    readers: Mutex<Vec<async_channel::Sender<T>>>,
}

impl<T: Clone> Signal<T> {
    pub const fn new(capacity: usize) -> Self {
        Self { capacity, readers: Mutex::new(Vec::new()) }
    }

    /// A reader of the values sent from now on.
    pub fn reader(&self) -> async_channel::Receiver<T> {
        let (tx, rx) = async_channel::bounded(self.capacity);
        self.readers.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
        rx
    }

    /// Gives the value to every reader. A reader that is gone leaves the
    /// list. False when the queue of a reader was full and the value did
    /// not go to it.
    pub fn send(&self, value: T) -> bool {
        let mut all = true;
        self.readers.lock().unwrap_or_else(|e| e.into_inner()).retain(|reader| {
            match reader.try_send(value.clone()) {
                Ok(()) => true,
                Err(async_channel::TrySendError::Full(_)) => {
                    all = false;
                    true
                }
                Err(async_channel::TrySendError::Closed(_)) => false,
            }
        });
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader a closed window left behind does not take the value of
    /// another: each has its own. With one queue for all it did.
    #[test]
    fn a_reader_left_behind_does_not_take_the_value_of_another() {
        let signal: Signal<u32> = Signal::new(4);
        let (stale, live) = (signal.reader(), signal.reader());
        assert!(signal.send(7));
        assert_eq!(stale.try_recv(), Ok(7));
        assert_eq!(live.try_recv(), Ok(7), "the stale reader took the value");
        // A reader that is gone leaves the list at the next value.
        drop(stale);
        assert!(signal.send(8));
        assert_eq!(live.try_recv(), Ok(8));
        assert_eq!(signal.readers.lock().unwrap().len(), 1);
    }

    /// A reader that does not read keeps `capacity` values; the sender
    /// hears that one more did not fit, and the other readers get it.
    #[test]
    fn a_full_queue_is_told_and_holds_no_other_reader_back() {
        let signal: Signal<u32> = Signal::new(2);
        let (slow, quick) = (signal.reader(), signal.reader());
        for value in 0..2 {
            assert!(signal.send(value));
            assert_eq!(quick.try_recv(), Ok(value));
        }
        assert!(!signal.send(2), "a value that did not fit was not told");
        assert_eq!(quick.try_recv(), Ok(2));
        assert_eq!((slow.try_recv(), slow.try_recv(), slow.try_recv().is_err()), (Ok(0), Ok(1), true));
    }
}
