//! Which job runs next, and when. Pure over the instants it is handed, so
//! every rule is pinned with a clock that moves only when a test says so:
//! the settle after an edit, the order of the classes, the spacing between
//! calls, and each pause and backoff a failure on the wire asks for. One
//! lane per service; one run at a time.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ryuuji_core::{EntryId, Job, Service, Task, UserId, When, Work};

use crate::service::Failure;

/// The least time between two calls to one service.
pub(crate) const SPACING: Duration = Duration::from_secs(2);
/// How long a `Settled` job waits after its last change, so a burst of
/// edits goes out as one.
const SETTLE: Duration = Duration::from_secs(10);
/// The pause when the service says the rate budget is spent.
const BUDGET_PAUSE: Duration = Duration::from_secs(60);
const OFFLINE_PAUSE: Duration = Duration::from_secs(30);
const OFFLINE_CAP: Duration = Duration::from_secs(120);
const TRANSIENT_BACKOFF: Duration = Duration::from_secs(30);
const TRANSIENT_CAP: Duration = Duration::from_secs(30 * 60);

/// What a job is about. One job per kind per show per service, so a job
/// posted again is the same job with new content rather than a second one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub service: Service,
    pub entry: EntryId,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Find,
    Reconcile,
}

impl Kind {
    fn of(task: &Task) -> Kind {
        match task {
            Task::Find { .. } => Kind::Find,
            Task::Reconcile { .. } => Kind::Reconcile,
        }
    }
}

/// A job as posted, with whose list it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Posted {
    pub user: UserId,
    pub job: Job,
}

/// What the worker does next: run a job, wait until then and ask again, or
/// wait for a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Run(Key),
    Wait(Instant),
    Idle,
}

/// How a run ended: how it failed, if it did, and what the service said
/// was left of the rate budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub failure: Option<Failure>,
    pub remaining: Option<u32>,
}

/// Why a lane is sending nothing for a while.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pause {
    /// The service answered 429.
    Throttled,
    /// The service said the rate budget was spent.
    Budget,
    /// The service could not be reached.
    Offline,
}

/// One lane as Diagnostics counts it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneFacts {
    pub paused: Option<(Pause, Instant)>,
    /// The message the token was refused with, while nothing runs.
    pub stopped: Option<String>,
    pub due: usize,
    pub waiting: usize,
    pub running: usize,
    /// When the soonest waiting reconcile goes out, after any pause: when
    /// the status bar says the next send goes out.
    pub next_due: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct Engine {
    lanes: HashMap<Service, Lane>,
}

impl Engine {
    /// Takes everything the shell wants done. A job already posted keeps
    /// its clock, one whose content changed starts over with no backoff,
    /// and one no longer posted is dropped, unless it is running, in which
    /// case it finishes and goes.
    pub(crate) fn post(&mut self, work: &Work, now: Instant) {
        for service in Service::ALL {
            let posted = work
                .services
                .iter()
                .filter(|work| work.service == service)
                .flat_map(|work| {
                    work.jobs.iter().map(move |job| {
                        let key = Key {
                            service,
                            entry: job.entry,
                            kind: Kind::of(&job.task),
                        };
                        (
                            key,
                            Posted {
                                user: work.user,
                                job: job.clone(),
                            },
                        )
                    })
                })
                .collect();
            self.lanes.entry(service).or_default().post(posted, now);
        }
    }

    /// The next step, and if it is a run, that job is running from now.
    pub(crate) fn next(&mut self, now: Instant) -> Next {
        let mut wait: Option<Instant> = None;
        for service in Service::ALL {
            let Some(lane) = self.lanes.get_mut(&service) else {
                continue;
            };
            match lane.next(now) {
                Next::Run(key) => return Next::Run(key),
                Next::Wait(at) => wait = Some(wait.map_or(at, |soonest| soonest.min(at))),
                Next::Idle => {}
            }
        }
        wait.map_or(Next::Idle, Next::Wait)
    }

    pub(crate) fn posted(&self, key: &Key) -> Option<&Posted> {
        self.lanes
            .get(&key.service)?
            .slots
            .iter()
            .find(|slot| slot.key == *key)
            .map(|slot| &slot.posted)
    }

    pub(crate) fn finished(&mut self, key: Key, outcome: Outcome, now: Instant) {
        if let Some(lane) = self.lanes.get_mut(&key.service) {
            lane.finished(key, outcome, now);
        }
    }

    /// A token was checked and kept. It may be another account's than the
    /// one the posted jobs were built for (a refused token replaced by a
    /// paste), so nothing posted before it runs on it: the lane starts
    /// clean, and the core posts its work again once it takes the sign-in.
    pub(crate) fn signed_in(&mut self, service: Service) {
        self.lanes.entry(service).or_default().signed_out();
    }

    /// The token is forgotten, so nothing posted for the account may run
    /// on: every job goes, one running only once it finishes, and a
    /// refusal that stopped the lane is over with the token it named.
    pub(crate) fn signed_out(&mut self, service: Service) {
        if let Some(lane) = self.lanes.get_mut(&service) {
            lane.signed_out();
        }
    }

    pub(crate) fn facts(&self, service: Service, now: Instant) -> LaneFacts {
        self.lanes
            .get(&service)
            .map(|lane| lane.facts(now))
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct Lane {
    slots: Vec<Slot>,
    running: Option<Key>,
    /// The earliest the next call may go out, from the spacing.
    free_at: Option<Instant>,
    paused: Option<(Pause, Instant)>,
    /// Failures to reach the service in a row, for the doubling pause.
    offline: u32,
    /// The message the token was refused with; nothing runs until a
    /// sign-in.
    stopped: Option<String>,
}

struct Slot {
    key: Key,
    posted: Posted,
    /// When first posted; within a class the oldest runs first.
    since: Instant,
    /// When its content last changed; the settle counts from here.
    changed_at: Instant,
    /// Transient failures in a row, and when it may run again after them.
    failures: u32,
    not_before: Option<Instant>,
    /// Changed since its run started, so the run's end does not close it.
    changed: bool,
    /// No longer posted; it goes as soon as it is not running.
    dropped: bool,
}

impl Slot {
    /// When the job is due by its class and its backoff. `Now` and
    /// `Background` are due from their posting; the class decides only
    /// which runs first.
    fn due_at(&self) -> Instant {
        let class = match self.posted.job.when {
            When::Now | When::Background => self.since,
            When::Settled => self.changed_at + SETTLE,
        };
        self.not_before.map_or(class, |at| class.max(at))
    }

    fn rank(&self) -> u8 {
        match self.posted.job.when {
            When::Now => 0,
            When::Settled => 1,
            When::Background => 2,
        }
    }
}

impl Lane {
    fn post(&mut self, posted: Vec<(Key, Posted)>, now: Instant) {
        let keys: Vec<Key> = posted.iter().map(|(key, _)| *key).collect();
        for (key, posted) in posted {
            match self.slots.iter_mut().find(|slot| slot.key == key) {
                Some(slot) => {
                    slot.dropped = false;
                    if slot.posted != posted {
                        slot.posted = posted;
                        slot.changed_at = now;
                        slot.failures = 0;
                        slot.not_before = None;
                        slot.changed = self.running == Some(key);
                    }
                }
                None => self.slots.push(Slot {
                    key,
                    posted,
                    since: now,
                    changed_at: now,
                    failures: 0,
                    not_before: None,
                    changed: false,
                    dropped: false,
                }),
            }
        }
        let running = self.running;
        self.slots.retain_mut(|slot| {
            if keys.contains(&slot.key) {
                return true;
            }
            if running == Some(slot.key) {
                slot.dropped = true;
                return true;
            }
            false
        });
    }

    fn signed_out(&mut self) {
        let running = self.running;
        self.slots.retain_mut(|slot| {
            slot.dropped = true;
            running == Some(slot.key)
        });
        self.stopped = None;
    }

    /// The due job of the best class, oldest first, once the lane is free
    /// of its spacing and its pause; else when to ask again.
    fn next(&mut self, now: Instant) -> Next {
        if self.running.is_some() || self.stopped.is_some() {
            return Next::Idle;
        }
        if let Some((_, until)) = self.paused
            && until <= now
        {
            self.paused = None;
        }
        let gate = self
            .paused
            .map(|(_, until)| until)
            .into_iter()
            .chain(self.free_at)
            .max()
            .filter(|at| *at > now);
        let mut best: Option<(u8, Instant, Key)> = None;
        let mut soonest: Option<Instant> = None;
        for slot in self.slots.iter().filter(|slot| !slot.dropped) {
            let due_at = slot.due_at();
            if due_at > now {
                soonest = Some(soonest.map_or(due_at, |at| at.min(due_at)));
                continue;
            }
            let order = (slot.rank(), slot.since, slot.key);
            if best.is_none_or(|(rank, since, _)| (order.0, order.1) < (rank, since)) {
                best = Some(order);
            }
        }
        match (best, gate) {
            (Some(_), Some(at)) => Next::Wait(at),
            (Some((_, _, key)), None) => {
                self.running = Some(key);
                if let Some(slot) = self.slots.iter_mut().find(|slot| slot.key == key) {
                    slot.changed = false;
                }
                Next::Run(key)
            }
            (None, _) => soonest.map_or(Next::Idle, Next::Wait),
        }
    }

    fn finished(&mut self, key: Key, outcome: Outcome, now: Instant) {
        if self.running == Some(key) {
            self.running = None;
        }
        self.free_at = Some(now + SPACING);
        if outcome.remaining == Some(0) {
            self.pause(Pause::Budget, now + BUDGET_PAUSE);
        }
        match &outcome.failure {
            Some(Failure::Offline) => {
                self.offline += 1;
                let pause = backoff(OFFLINE_PAUSE, self.offline, OFFLINE_CAP);
                self.pause(Pause::Offline, now + pause);
            }
            Some(Failure::Throttled(until)) => {
                self.offline = 0;
                self.pause(Pause::Throttled, *until);
            }
            Some(Failure::Refused(message)) => {
                self.offline = 0;
                self.stopped = Some(message.clone());
            }
            Some(Failure::Transient | Failure::Rejected(_) | Failure::NotFound) | None => {
                self.offline = 0;
            }
        }
        let Some(at) = self.slots.iter().position(|slot| slot.key == key) else {
            return;
        };
        let slot = &mut self.slots[at];
        let ends = match outcome.failure {
            None | Some(Failure::Rejected(_) | Failure::NotFound) => true,
            Some(Failure::Transient) => {
                // A run of the content before a change says nothing about
                // the content after it.
                if !slot.changed {
                    slot.failures += 1;
                    let wait = backoff(TRANSIENT_BACKOFF, slot.failures, TRANSIENT_CAP);
                    slot.not_before = Some(now + wait);
                }
                false
            }
            Some(Failure::Refused(_) | Failure::Throttled(_) | Failure::Offline) => false,
        };
        if slot.dropped || (ends && !slot.changed) {
            self.slots.remove(at);
        }
    }

    /// The later of the pause standing and this one.
    fn pause(&mut self, why: Pause, until: Instant) {
        if self.paused.is_none_or(|(_, standing)| standing < until) {
            self.paused = Some((why, until));
        }
    }

    fn facts(&self, now: Instant) -> LaneFacts {
        let open = || {
            self.slots
                .iter()
                .filter(|slot| !slot.dropped && self.running != Some(slot.key))
        };
        let due = open().filter(|slot| slot.due_at() <= now).count();
        let waiting = open().filter(|slot| slot.due_at() > now).count();
        let paused = self.paused.filter(|(_, until)| *until > now);
        // Only a reconcile sends, and a pause holds it until the pause ends.
        let next_due = open()
            .filter(|slot| slot.key.kind == Kind::Reconcile)
            .map(|slot| paused.map_or(slot.due_at(), |(_, until)| slot.due_at().max(until)))
            .filter(|at| *at > now)
            .min();
        LaneFacts {
            paused,
            stopped: self.stopped.clone(),
            due,
            waiting,
            running: usize::from(self.running.is_some()),
            next_due,
        }
    }
}

/// `base`, doubled for each failure past the first, up to `cap`.
fn backoff(base: Duration, failures: u32, cap: Duration) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    base.saturating_mul(1 << doublings).min(cap)
}

#[cfg(test)]
pub(crate) mod tests {
    use ryuuji_core::{
        Command, DataDir, ListState, ListStatus, MediaId, NewEntry, Ryuuji, ServiceWork,
        WatchStatus,
    };

    use super::*;

    /// `n` entry ids as the store mints them, since nothing else may. The
    /// library they belong to goes with the temp dir.
    pub(crate) fn entries(n: usize) -> Vec<EntryId> {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = Ryuuji::open(&DataDir::at(tmp.path()).unwrap()).unwrap();
        for i in 0..n {
            app.dispatch(Command::AddEntry(NewEntry {
                title: format!("Show {i}"),
                status: WatchStatus::Watching,
                progress: 0,
                total: None,
                rewatching: false,
            }));
        }
        let ids: Vec<EntryId> = app.state().library.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), n);
        ids
    }

    const USER: UserId = UserId(7);

    fn work(jobs: Vec<Job>) -> Work {
        Work {
            services: vec![ServiceWork {
                service: Service::AniList,
                user: USER,
                jobs,
            }],
        }
    }

    fn reconcile(entry: EntryId, when: When, progress: u32) -> Job {
        Job {
            entry,
            task: Task::Reconcile {
                media: MediaId(5),
                wanted: ListState {
                    status: ListStatus::Watching,
                    progress,
                    repeat: 0,
                },
                base: None,
            },
            when,
        }
    }

    fn find(entry: EntryId) -> Job {
        Job {
            entry,
            task: Task::Find {
                title: "Show".into(),
            },
            when: When::Background,
        }
    }

    fn key(entry: EntryId, kind: Kind) -> Key {
        Key {
            service: Service::AniList,
            entry,
            kind,
        }
    }

    fn ok() -> Outcome {
        Outcome {
            failure: None,
            remaining: None,
        }
    }

    fn failed(failure: Failure) -> Outcome {
        Outcome {
            failure: Some(failure),
            remaining: None,
        }
    }

    fn clock() -> impl Fn(u64) -> Instant {
        let start = Instant::now();
        move |seconds| start + Duration::from_secs(seconds)
    }

    #[test]
    fn an_engine_with_nothing_posted_is_idle() {
        let at = clock();
        let mut engine = Engine::default();
        assert_eq!(engine.next(at(0)), Next::Idle);
        engine.post(&Work { services: vec![] }, at(0));
        assert_eq!(engine.next(at(0)), Next::Idle);
        assert_eq!(engine.facts(Service::AniList, at(0)), LaneFacts::default());
    }

    #[test]
    fn a_settled_job_runs_ten_seconds_after_its_last_change_and_a_change_restarts_the_clock() {
        let at = clock();
        let [a] = entries(1)[..] else { unreachable!() };
        let mut engine = Engine::default();
        engine.post(&work(vec![reconcile(a, When::Settled, 4)]), at(0));
        assert_eq!(engine.next(at(0)), Next::Wait(at(10)));
        assert_eq!(engine.next(at(9)), Next::Wait(at(10)));
        // An equal post changes nothing.
        engine.post(&work(vec![reconcile(a, When::Settled, 4)]), at(5));
        assert_eq!(engine.next(at(9)), Next::Wait(at(10)));
        // A change starts the settle over.
        engine.post(&work(vec![reconcile(a, When::Settled, 5)]), at(7));
        assert_eq!(engine.next(at(10)), Next::Wait(at(17)));
        assert_eq!(engine.next(at(16)), Next::Wait(at(17)));
        let key = key(a, Kind::Reconcile);
        assert_eq!(engine.next(at(17)), Next::Run(key));
        assert_eq!(
            engine.posted(&key),
            Some(&Posted {
                user: USER,
                job: reconcile(a, When::Settled, 5)
            })
        );
        // Running, it is not offered again.
        assert_eq!(engine.next(at(17)), Next::Idle);
    }

    #[test]
    fn now_runs_first_then_what_has_settled_then_the_background_oldest_first() {
        let at = clock();
        let [a, b, c, d] = entries(4)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![
                find(a),
                reconcile(b, When::Settled, 4),
                reconcile(c, When::Now, 4),
            ]),
            at(0),
        );
        engine.post(
            &work(vec![
                find(a),
                reconcile(b, When::Settled, 4),
                reconcile(c, When::Now, 4),
                find(d),
            ]),
            at(1),
        );
        assert_eq!(engine.next(at(1)), Next::Run(key(c, Kind::Reconcile)));
        engine.finished(key(c, Kind::Reconcile), ok(), at(1));
        assert_eq!(engine.next(at(1)), Next::Wait(at(3)));
        assert_eq!(engine.next(at(3)), Next::Run(key(a, Kind::Find)));
        engine.finished(key(a, Kind::Find), ok(), at(3));
        assert_eq!(engine.next(at(5)), Next::Run(key(d, Kind::Find)));
        engine.finished(key(d, Kind::Find), ok(), at(5));
        assert_eq!(engine.next(at(7)), Next::Wait(at(10)));
        assert_eq!(engine.next(at(10)), Next::Run(key(b, Kind::Reconcile)));
        engine.finished(key(b, Kind::Reconcile), ok(), at(10));
        assert_eq!(engine.next(at(12)), Next::Idle);
    }

    #[test]
    fn calls_to_one_service_are_two_seconds_apart() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        assert_eq!(engine.next(at(0)), Next::Run(key(a, Kind::Reconcile)));
        engine.finished(key(a, Kind::Reconcile), ok(), at(0));
        assert_eq!(engine.next(at(0)), Next::Wait(at(2)));
        assert_eq!(engine.next(at(1)), Next::Wait(at(2)));
        assert_eq!(engine.next(at(2)), Next::Run(key(b, Kind::Reconcile)));
    }

    #[test]
    fn a_throttle_pauses_the_lane_until_the_time_given_and_a_spent_budget_for_a_minute() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        let a = key(a, Kind::Reconcile);
        let b = key(b, Kind::Reconcile);
        assert_eq!(engine.next(at(0)), Next::Run(a));
        engine.finished(a, failed(Failure::Throttled(at(30))), at(0));
        assert_eq!(engine.next(at(2)), Next::Wait(at(30)));
        assert_eq!(
            engine.facts(Service::AniList, at(2)),
            LaneFacts {
                paused: Some((Pause::Throttled, at(30))),
                stopped: None,
                due: 2,
                waiting: 0,
                running: 0,
                next_due: Some(at(30)),
            }
        );
        assert_eq!(engine.next(at(29)), Next::Wait(at(30)));
        // The throttled job is not at fault and runs first again.
        assert_eq!(engine.next(at(30)), Next::Run(a));
        engine.finished(
            a,
            Outcome {
                failure: None,
                remaining: Some(0),
            },
            at(30),
        );
        assert_eq!(engine.next(at(32)), Next::Wait(at(90)));
        assert_eq!(
            engine.facts(Service::AniList, at(40)).paused,
            Some((Pause::Budget, at(90)))
        );
        assert_eq!(engine.next(at(90)), Next::Run(b));
        engine.finished(
            b,
            Outcome {
                failure: None,
                remaining: Some(1),
            },
            at(90),
        );
        assert_eq!(engine.facts(Service::AniList, at(91)).paused, None);
    }

    #[test]
    fn an_unreachable_service_pauses_the_lane_doubling_to_two_minutes_until_it_answers() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        let a = key(a, Kind::Reconcile);
        let b = key(b, Kind::Reconcile);
        let mut t = 0;
        for pause in [30, 60, 120, 120] {
            assert_eq!(engine.next(at(t)), Next::Run(a), "at {t}");
            engine.finished(a, failed(Failure::Offline), at(t));
            assert_eq!(engine.next(at(t + 2)), Next::Wait(at(t + pause)), "at {t}");
            assert_eq!(
                engine.facts(Service::AniList, at(t + 2)).paused,
                Some((Pause::Offline, at(t + pause)))
            );
            t += pause;
        }
        // An answer resets the doubling.
        assert_eq!(engine.next(at(t)), Next::Run(a));
        engine.finished(a, ok(), at(t));
        assert_eq!(engine.next(at(t + 2)), Next::Run(b));
        engine.finished(b, failed(Failure::Offline), at(t + 2));
        assert_eq!(engine.next(at(t + 4)), Next::Wait(at(t + 32)));
    }

    #[test]
    fn a_transient_failure_backs_the_job_off_doubling_to_half_an_hour_and_a_change_resets_it() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        let ka = key(a, Kind::Reconcile);
        let kb = key(b, Kind::Reconcile);
        assert_eq!(engine.next(at(0)), Next::Run(ka));
        engine.finished(ka, failed(Failure::Transient), at(0));
        // The backoff is the job's, not the lane's.
        assert_eq!(engine.next(at(2)), Next::Run(kb));
        engine.finished(kb, ok(), at(2));
        assert_eq!(engine.next(at(4)), Next::Wait(at(30)));
        assert_eq!(engine.facts(Service::AniList, at(4)).waiting, 1);
        let mut t = 30;
        for wait in [60, 120, 240, 480, 960, 1800, 1800] {
            assert_eq!(engine.next(at(t)), Next::Run(ka), "at {t}");
            engine.finished(ka, failed(Failure::Transient), at(t));
            assert_eq!(engine.next(at(t + 2)), Next::Wait(at(t + wait)), "at {t}");
            t += wait;
        }
        // A change starts the job over with no backoff.
        engine.post(&work(vec![reconcile(a, When::Now, 5)]), at(t - 1000));
        assert_eq!(engine.next(at(t - 1000)), Next::Run(ka));
        engine.finished(ka, failed(Failure::Transient), at(t - 1000));
        assert_eq!(engine.next(at(t - 998)), Next::Wait(at(t - 970)));
    }

    #[test]
    fn a_refused_token_stops_the_lane_until_a_sign_in_and_the_work_posted_after_it() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        let a = key(a, Kind::Reconcile);
        assert_eq!(engine.next(at(0)), Next::Run(a));
        engine.finished(a, failed(Failure::Refused("Invalid token".into())), at(0));
        assert_eq!(engine.next(at(5)), Next::Idle);
        assert_eq!(engine.next(at(500)), Next::Idle);
        let facts = engine.facts(Service::AniList, at(5));
        assert_eq!(facts.stopped.as_deref(), Some("Invalid token"));
        assert_eq!(facts.due, 2);
        engine.signed_in(Service::AniList);
        assert_eq!(engine.next(at(5)), Next::Idle);
        engine.post(&work(vec![reconcile(a.entry, When::Now, 4)]), at(5));
        assert_eq!(engine.next(at(5)), Next::Run(a));
    }

    #[test]
    fn a_sign_out_drops_every_job_lifts_a_refusal_and_lets_a_run_finish() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(&work(vec![find(a), reconcile(b, When::Settled, 4)]), at(0));
        assert_eq!(engine.facts(Service::AniList, at(0)).due, 1);
        engine.signed_out(Service::AniList);
        assert_eq!(engine.facts(Service::AniList, at(0)), LaneFacts::default());
        assert_eq!(engine.next(at(0)), Next::Idle);
        assert_eq!(engine.next(at(10)), Next::Idle);

        let ka = key(a, Kind::Reconcile);
        engine.post(&work(vec![reconcile(a, When::Now, 4)]), at(10));
        assert_eq!(engine.next(at(10)), Next::Run(ka));
        engine.finished(ka, failed(Failure::Refused("Invalid token".into())), at(10));
        assert_eq!(engine.next(at(12)), Next::Idle);
        engine.signed_out(Service::AniList);
        assert_eq!(engine.facts(Service::AniList, at(12)), LaneFacts::default());

        // Posted again and running when the sign-out comes, the run
        // finishes and the job goes with it.
        engine.post(&work(vec![reconcile(a, When::Now, 4)]), at(12));
        assert_eq!(engine.next(at(12)), Next::Run(ka));
        engine.signed_out(Service::AniList);
        assert!(engine.posted(&ka).is_some());
        assert_eq!(engine.facts(Service::AniList, at(12)).running, 1);
        engine.finished(ka, ok(), at(12));
        assert_eq!(engine.posted(&ka), None);
        assert_eq!(engine.next(at(14)), Next::Idle);
    }

    #[test]
    fn a_sign_in_starts_the_lane_clean_so_no_job_built_for_another_account_runs() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        let ka = key(a, Kind::Reconcile);
        engine.post(&work(vec![reconcile(a, When::Now, 4), find(b)]), at(0));
        assert_eq!(engine.next(at(0)), Next::Run(ka));
        engine.finished(ka, failed(Failure::Refused("Invalid token".into())), at(0));
        assert_eq!(engine.next(at(2)), Next::Idle);
        engine.signed_in(Service::AniList);
        assert_eq!(engine.facts(Service::AniList, at(2)), LaneFacts::default());
        assert_eq!(engine.next(at(2)), Next::Idle);
        engine.post(&work(vec![reconcile(a, When::Now, 5)]), at(2));
        assert_eq!(engine.next(at(2)), Next::Run(ka));
    }

    #[test]
    fn a_job_no_longer_posted_is_dropped_and_one_running_still_finishes() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        engine.post(&work(vec![reconcile(a, When::Now, 4)]), at(0));
        let ka = key(a, Kind::Reconcile);
        assert_eq!(engine.facts(Service::AniList, at(0)).due, 1);
        assert_eq!(engine.next(at(0)), Next::Run(ka));
        engine.finished(ka, ok(), at(0));
        assert_eq!(engine.next(at(2)), Next::Idle);

        engine.post(&work(vec![reconcile(a, When::Now, 4)]), at(2));
        assert_eq!(engine.next(at(2)), Next::Run(ka));
        engine.post(&work(vec![]), at(3));
        assert_eq!(
            engine.facts(Service::AniList, at(3)),
            LaneFacts {
                paused: None,
                stopped: None,
                due: 0,
                waiting: 0,
                running: 1,
                next_due: None,
            }
        );
        // The run still reports through the worker; the engine only lets
        // the job go.
        assert!(engine.posted(&ka).is_some());
        engine.finished(ka, failed(Failure::Transient), at(3));
        assert_eq!(engine.posted(&ka), None);
        assert_eq!(engine.next(at(5)), Next::Idle);
    }

    #[test]
    fn a_job_changed_while_running_stays_posted_with_its_clock_restarted() {
        let at = clock();
        let [a] = entries(1)[..] else { unreachable!() };
        let mut engine = Engine::default();
        engine.post(&work(vec![reconcile(a, When::Settled, 4)]), at(0));
        let key = key(a, Kind::Reconcile);
        assert_eq!(engine.next(at(10)), Next::Run(key));
        engine.post(&work(vec![reconcile(a, When::Settled, 5)]), at(11));
        engine.finished(key, ok(), at(12));
        assert_eq!(engine.next(at(12)), Next::Wait(at(21)));
        assert_eq!(engine.next(at(21)), Next::Run(key));
        assert_eq!(
            engine.posted(&key).map(|posted| &posted.job),
            Some(&reconcile(a, When::Settled, 5))
        );
        // A change during a run that then fails transiently is not backed
        // off: the failure was the old content's.
        engine.post(&work(vec![reconcile(a, When::Settled, 6)]), at(22));
        engine.finished(key, failed(Failure::Transient), at(23));
        assert_eq!(engine.next(at(23)), Next::Wait(at(32)));
    }

    #[test]
    fn a_rejected_write_or_a_missing_show_ends_the_job() {
        let at = clock();
        let [a, b] = entries(2)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![reconcile(a, When::Now, 4), reconcile(b, When::Now, 4)]),
            at(0),
        );
        let a = key(a, Kind::Reconcile);
        let b = key(b, Kind::Reconcile);
        assert_eq!(engine.next(at(0)), Next::Run(a));
        engine.finished(a, failed(Failure::Rejected("validation".into())), at(0));
        assert_eq!(engine.next(at(2)), Next::Run(b));
        engine.finished(b, failed(Failure::NotFound), at(2));
        assert_eq!(engine.next(at(4)), Next::Idle);
        assert_eq!(engine.facts(Service::AniList, at(4)).paused, None);
    }

    #[test]
    fn the_facts_count_what_is_due_waiting_and_running() {
        let at = clock();
        let [a, b, c] = entries(3)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        engine.post(
            &work(vec![
                reconcile(a, When::Now, 4),
                reconcile(b, When::Settled, 4),
                find(c),
            ]),
            at(0),
        );
        let counts = |facts: LaneFacts| (facts.due, facts.waiting, facts.running);
        assert_eq!(counts(engine.facts(Service::AniList, at(0))), (2, 1, 0));
        assert_eq!(engine.next(at(0)), Next::Run(key(a, Kind::Reconcile)));
        assert_eq!(counts(engine.facts(Service::AniList, at(0))), (1, 1, 1));
        assert_eq!(counts(engine.facts(Service::AniList, at(10))), (2, 0, 1));
    }

    #[test]
    fn the_facts_say_when_the_soonest_waiting_send_goes_out() {
        let at = clock();
        let [a, b, c] = entries(3)[..] else {
            unreachable!()
        };
        let mut engine = Engine::default();
        assert_eq!(engine.facts(Service::AniList, at(0)).next_due, None);
        let search = Job {
            when: When::Settled,
            ..find(c)
        };
        engine.post(&work(vec![reconcile(a, When::Settled, 4)]), at(0));
        // A search sends nothing, however soon it runs.
        engine.post(
            &work(vec![reconcile(a, When::Settled, 4), search.clone()]),
            at(2),
        );
        engine.post(
            &work(vec![
                reconcile(a, When::Settled, 4),
                search,
                reconcile(b, When::Settled, 5),
            ]),
            at(3),
        );
        assert_eq!(
            engine.facts(Service::AniList, at(4)).next_due,
            Some(at(0) + SETTLE)
        );
        // Due now is no longer waiting, so the next one is the later show.
        assert_eq!(
            engine.facts(Service::AniList, at(11)).next_due,
            Some(at(3) + SETTLE)
        );
        assert_eq!(engine.facts(Service::AniList, at(14)).next_due, None);
    }

    #[test]
    fn a_backoff_doubles_from_its_base_up_to_its_cap() {
        let s = Duration::from_secs;
        assert_eq!(backoff(s(30), 0, s(1800)), s(30));
        assert_eq!(backoff(s(30), 1, s(1800)), s(30));
        assert_eq!(backoff(s(30), 2, s(1800)), s(60));
        assert_eq!(backoff(s(30), 7, s(1800)), s(1800));
        assert_eq!(backoff(s(30), u32::MAX, s(1800)), s(1800));
    }
}
