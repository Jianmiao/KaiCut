//! Bounded preview work and result storage, independent of threads and devices.
//! The owner supplies synchronization; an audio producer should use try_lock
//! rather than wait for a GPU worker. Device calls must happen outside the lock.

use std::{collections::VecDeque, fmt};

const MAX_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    InvalidCapacity,
    EpochExhausted,
    Closed,
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCapacity => "Preview queue capacity must be in 1..=1024",
            Self::EpochExhausted => "Preview epoch cannot advance without wrapping",
            Self::Closed => "Preview queue is closed",
        })
    }
}
impl std::error::Error for QueueError {}

#[derive(Debug, PartialEq, Eq)]
pub struct Job<T> {
    pub epoch: u64,
    pub item: T,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Submission<T> {
    Accepted { evicted: Option<Job<T>> },
    Stale { item: T },
    Closed { item: T },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Completion<R> {
    Published { overwritten: Option<Job<R>> },
    Stale { result: R },
    Closed { result: R },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueCounters {
    /// Successfully accepted submissions, including those later evicted.
    pub submitted: u64,
    pub evicted: u64,
    pub stale_submissions: u64,
    pub stale_completions: u64,
    pub high_water: usize,
    pub taken: u64,
    /// Current-epoch results accepted into the single completion slot.
    pub completed: u64,
    pub overwritten_results: u64,
    pub closed_submissions: u64,
    pub closed_completions: u64,
    pub invalidated_pending: u64,
    pub invalidated_results: u64,
}

/// The bound is a count of pending jobs, not a byte budget for arbitrary `T`.
/// Store bounded descriptors or shared frame references as payloads. There is
/// at most one completed result; an in-flight worker owns its taken job outside
/// this object. Caller-owned mutex/condvar synchronization must not hold a lock
/// across decode/render/readback. Rejected or replaced payloads are returned so
/// callers can release expensive resources after dropping that external lock.
#[derive(Debug)]
pub struct PreviewQueue<T, R> {
    capacity: usize,
    epoch: u64,
    closed: bool,
    pending: VecDeque<Job<T>>,
    latest_completed: Option<Job<R>>,
    counters: QueueCounters,
}

impl<T, R> PreviewQueue<T, R> {
    pub fn new(capacity: usize) -> Result<Self, QueueError> {
        if capacity == 0 || capacity > MAX_CAPACITY {
            return Err(QueueError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            epoch: 1,
            closed: false,
            pending: VecDeque::with_capacity(capacity),
            latest_completed: None,
            counters: QueueCounters::default(),
        })
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    pub fn has_completed(&self) -> bool {
        self.latest_completed.is_some()
    }
    pub fn counters(&self) -> QueueCounters {
        self.counters
    }

    pub fn advance_epoch(&mut self) -> Result<u64, QueueError> {
        if self.closed {
            return Err(QueueError::Closed);
        }
        let next = self
            .epoch
            .checked_add(1)
            .ok_or(QueueError::EpochExhausted)?;
        self.clear_contents();
        self.epoch = next;
        Ok(next)
    }

    pub fn submit(&mut self, epoch: u64, item: T) -> Submission<T> {
        if self.closed {
            self.counters.closed_submissions = self.counters.closed_submissions.saturating_add(1);
            return Submission::Closed { item };
        }
        if epoch != self.epoch {
            self.counters.stale_submissions = self.counters.stale_submissions.saturating_add(1);
            return Submission::Stale { item };
        }
        let evicted = if self.pending.len() == self.capacity {
            self.pending.pop_front()
        } else {
            None
        };
        if evicted.is_some() {
            self.counters.evicted = self.counters.evicted.saturating_add(1);
        }
        self.pending.push_back(Job { epoch, item });
        self.counters.submitted = self.counters.submitted.saturating_add(1);
        self.counters.high_water = self.counters.high_water.max(self.pending.len());
        Submission::Accepted { evicted }
    }

    pub fn take(&mut self) -> Option<Job<T>> {
        let job = self.pending.pop_front()?;
        self.counters.taken = self.counters.taken.saturating_add(1);
        Some(job)
    }

    pub fn publish(&mut self, epoch: u64, result: R) -> Completion<R> {
        if self.closed {
            self.counters.closed_completions = self.counters.closed_completions.saturating_add(1);
            return Completion::Closed { result };
        }
        if epoch != self.epoch {
            self.counters.stale_completions = self.counters.stale_completions.saturating_add(1);
            return Completion::Stale { result };
        }
        let overwritten = self.latest_completed.replace(Job {
            epoch,
            item: result,
        });
        self.counters.completed = self.counters.completed.saturating_add(1);
        if overwritten.is_some() {
            self.counters.overwritten_results = self.counters.overwritten_results.saturating_add(1);
        }
        Completion::Published { overwritten }
    }

    pub fn take_completed(&mut self) -> Option<Job<R>> {
        self.latest_completed.take()
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.clear_contents();
    }

    fn clear_contents(&mut self) {
        self.counters.invalidated_pending = self
            .counters
            .invalidated_pending
            .saturating_add(self.pending.len() as u64);
        self.counters.invalidated_results = self
            .counters
            .invalidated_results
            .saturating_add(u64::from(self.latest_completed.is_some()));
        self.pending.clear();
        self.latest_completed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    #[test]
    fn capacity_two_keeps_newest_jobs_and_returns_evicted_payload() {
        let mut queue = PreviewQueue::<&str, &str>::new(2).unwrap();
        let epoch = queue.epoch();
        assert!(matches!(
            queue.submit(epoch, "one"),
            Submission::Accepted { evicted: None }
        ));
        queue.submit(epoch, "two");
        assert_eq!(
            queue.submit(epoch, "three"),
            Submission::Accepted {
                evicted: Some(Job { epoch, item: "one" })
            }
        );
        assert_eq!(queue.take(), Some(Job { epoch, item: "two" }));
        assert_eq!(
            queue.take(),
            Some(Job {
                epoch,
                item: "three"
            })
        );
        assert_eq!(queue.take(), None);
        assert_eq!(queue.counters().submitted, 3);
        assert_eq!(queue.counters().evicted, 1);
        assert_eq!(queue.counters().taken, 2);
        assert_eq!(queue.counters().high_water, 2);
    }

    #[test]
    fn old_in_flight_job_cannot_publish_after_an_epoch_change() {
        let mut queue = PreviewQueue::<u8, &str>::new(1).unwrap();
        let old = queue.epoch();
        queue.submit(old, 1);
        let job = queue.take().unwrap();
        let fresh = queue.advance_epoch().unwrap();
        assert_ne!(fresh, old);
        assert_eq!(
            queue.publish(job.epoch, "old frame"),
            Completion::Stale {
                result: "old frame"
            }
        );
        assert_eq!(queue.take_completed(), None);
        assert_eq!(queue.submit(old, 2), Submission::Stale { item: 2 });
        assert!(matches!(
            queue.publish(fresh, "fresh frame"),
            Completion::Published { overwritten: None }
        ));
        assert_eq!(
            queue.take_completed(),
            Some(Job {
                epoch: fresh,
                item: "fresh frame"
            })
        );
        assert_eq!(queue.counters().stale_completions, 1);
        assert_eq!(queue.counters().stale_submissions, 1);
    }

    #[test]
    fn results_are_a_single_latest_mailbox_and_rebase_clears_both_sides() {
        let mut queue = PreviewQueue::<u8, u8>::new(2).unwrap();
        let epoch = queue.epoch();
        queue.submit(epoch, 1);
        queue.publish(epoch, 10);
        assert!(queue.has_completed());
        assert_eq!(
            queue.publish(epoch, 20),
            Completion::Published {
                overwritten: Some(Job { epoch, item: 10 })
            }
        );
        assert_eq!(queue.take_completed(), Some(Job { epoch, item: 20 }));
        assert_eq!(queue.take_completed(), None);
        assert!(!queue.has_completed());
        queue.publish(epoch, 30);
        queue.advance_epoch().unwrap();
        assert_eq!(queue.take(), None);
        assert_eq!(queue.take_completed(), None);
        assert_eq!(queue.counters().completed, 3);
        assert_eq!(queue.counters().overwritten_results, 1);
    }

    #[test]
    fn closing_is_terminal_and_returns_new_payloads_to_the_caller() {
        let mut queue = PreviewQueue::<u8, u8>::new(1).unwrap();
        let epoch = queue.epoch();
        queue.submit(epoch, 1);
        queue.publish(epoch, 2);
        queue.close();
        queue.close();
        assert_eq!(queue.take(), None);
        assert_eq!(queue.take_completed(), None);
        assert_eq!(queue.submit(epoch, 3), Submission::Closed { item: 3 });
        assert_eq!(queue.publish(epoch, 4), Completion::Closed { result: 4 });
        assert_eq!(queue.advance_epoch(), Err(QueueError::Closed));
        assert_eq!(queue.epoch(), epoch);
    }

    #[test]
    fn epoch_overflow_does_not_wrap_or_erase_valid_queued_state() {
        let mut queue = PreviewQueue::<u8, u8>::new(1).unwrap();
        queue.epoch = u64::MAX;
        queue.submit(u64::MAX, 7);
        queue.publish(u64::MAX, 8);
        assert_eq!(queue.advance_epoch(), Err(QueueError::EpochExhausted));
        assert_eq!(queue.epoch(), u64::MAX);
        assert_eq!(
            queue.take(),
            Some(Job {
                epoch: u64::MAX,
                item: 7
            })
        );
        assert_eq!(
            queue.take_completed(),
            Some(Job {
                epoch: u64::MAX,
                item: 8
            })
        );
    }

    #[test]
    fn invalid_capacity_is_rejected_and_counters_saturate_safely() {
        assert!(matches!(
            PreviewQueue::<u8, u8>::new(0),
            Err(QueueError::InvalidCapacity)
        ));
        assert!(matches!(
            PreviewQueue::<u8, u8>::new(usize::MAX),
            Err(QueueError::InvalidCapacity)
        ));
        let mut queue = PreviewQueue::<u8, u8>::new(1).unwrap();
        queue.counters.submitted = u64::MAX;
        queue.counters.completed = u64::MAX;
        let epoch = queue.epoch();
        queue.submit(epoch, 1);
        queue.publish(epoch, 2);
        assert_eq!(queue.counters().submitted, u64::MAX);
        assert_eq!(queue.counters().completed, u64::MAX);
        assert_eq!(queue.take().unwrap().item, 1);
        assert_eq!(queue.take_completed().unwrap().item, 2);
    }

    #[test]
    fn worker_publication_after_a_coordinated_seek_is_stale_without_holding_work_lock() {
        let queue = Arc::new(Mutex::new(PreviewQueue::<u8, u8>::new(1).unwrap()));
        let epoch = queue.lock().unwrap().epoch();
        queue.lock().unwrap().submit(epoch, 9);
        let taken = Arc::new(Barrier::new(2));
        let rebased = Arc::new(Barrier::new(2));
        let worker_queue = Arc::clone(&queue);
        let worker_taken = Arc::clone(&taken);
        let worker_rebased = Arc::clone(&rebased);
        let worker = std::thread::spawn(move || {
            let job = worker_queue.lock().unwrap().take().unwrap();
            worker_taken.wait();
            worker_rebased.wait();
            worker_queue
                .lock()
                .unwrap()
                .publish(job.epoch, job.item + 1)
        });
        taken.wait();
        let fresh = queue
            .try_lock()
            .expect("Worker must release queue lock while doing device work")
            .advance_epoch()
            .unwrap();
        rebased.wait();
        assert_eq!(worker.join().unwrap(), Completion::Stale { result: 10 });
        let mut current = queue.lock().unwrap();
        assert_eq!(current.epoch(), fresh);
        assert_eq!(current.take_completed(), None);
    }
}
