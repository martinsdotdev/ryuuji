//! The thread the wire runs on. It owns the engine, the vault and the
//! transport; the shell reaches it through [`Syncer`] and hears from it
//! through the report queue, whose counter is what a shell holding only the
//! latest value acts on: when the number moves, it drains `take_reports`.
//! Diagnostics reads the lanes from a mirror the worker publishes after
//! each step. No token leaves this module except inside a request.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use ryuuji_core::{Account, Cover, Report, Service, ShowInfo, Signed, Task, Work};
use tracing::{info, warn};

use crate::engine::{Engine, Key, Next, Outcome, Pause, SPACING};
use crate::http::{Http, Request, Response};
use crate::reconcile::{self, Call, Reconcile};
use crate::service::{Failure, Session};
use crate::vault::Tokens;
use crate::{Credentials, Token};

type Transport = Box<dyn FnMut(&Request, Option<&Token>) -> Result<Response, Failure> + Send>;
/// How a picture is fetched. It is handed an address and nothing else, so
/// no token can ride along.
type Fetch = Box<dyn FnMut(&str) -> Result<Vec<u8>, Failure> + Send>;
type OnReport = Box<dyn Fn(u64) + Send>;

/// How often the pacing wait looks up to see whether the worker is stopping.
const STOP_CHECK: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("could not spawn the sync worker thread")]
    Thread(#[source] io::Error),
}

/// Starts the worker. `on_report` is called on the worker thread with a
/// number that never repeats, each time reports are queued; the shell then
/// drains them with [`Syncer::take_reports`].
pub fn start(on_report: impl Fn(u64) + Send + 'static) -> Result<Syncer, SyncError> {
    let http = std::sync::Arc::new(Http::new());
    let pictures = http.clone();
    start_with(
        Tokens::default(),
        Box::new(move |request, token| http.send(request, token)),
        Box::new(move |url| pictures.fetch(url)),
        Box::new(on_report),
    )
}

fn start_with(
    vault: Tokens,
    transport: Transport,
    fetch: Fetch,
    on_report: OnReport,
) -> Result<Syncer, SyncError> {
    let (tx, rx) = mpsc::channel();
    let reports = Reports::default();
    let mirror = Mirror::default();
    let stopping = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        engine: Engine::default(),
        vault,
        calls: HashMap::new(),
        transport,
        fetch,
        last_sent: None,
        run_remaining: None,
        reports: reports.clone(),
        on_report,
        mirror: mirror.clone(),
        stopping: stopping.clone(),
    };
    let thread = thread::Builder::new()
        .name("ryuuji-sync".to_owned())
        .spawn(move || worker.run(rx))
        .map_err(SyncError::Thread)?;
    Ok(Syncer {
        tx,
        thread: Some(thread),
        reports,
        mirror,
        stopping,
    })
}

/// The shell's handle on the worker. Dropping it stops the worker and
/// waits for it.
pub struct Syncer {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    reports: Reports,
    mirror: Mirror,
    stopping: Arc<AtomicBool>,
}

impl Syncer {
    /// Hands the worker everything to do. An equal post changes nothing: a
    /// job already posted keeps its clock, one whose content changed starts
    /// over, and one left out is dropped.
    pub fn post(&self, work: &Work) {
        let _ = self.tx.send(Msg::Work(work.clone()));
    }

    /// Checks the credentials against the service and keeps them when they
    /// work. The answer comes as `Checking`, then `SignedIn` or
    /// `NotSignedIn`.
    pub fn sign_in(&self, service: Service, credentials: Credentials) {
        let _ = self.tx.send(Msg::SignIn(service, credentials));
    }

    /// Forgets the token. The answer is `SignedOut`.
    pub fn sign_out(&self, service: Service) {
        let _ = self.tx.send(Msg::SignOut(service));
    }

    /// Every report queued since the last take, oldest first.
    pub fn take_reports(&self) -> Vec<Report> {
        self.reports.take()
    }

    /// Each lane as of the worker's last step. Never a token.
    pub fn facts(&self) -> Vec<ServiceFacts> {
        self.mirror.read()
    }
}

impl Drop for Syncer {
    fn drop(&mut self) {
        // Set before the message is sent: a job already waiting out the
        // pacing cannot read the mailbox, and this is what tells it to give
        // up rather than hold the join below for the whole spacing.
        self.stopping.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One lane as Diagnostics shows it: what the worker alone knows. The
/// account is the core's, and the facts never come from the vault, so
/// publishing them touches no credential store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceFacts {
    pub service: Service,
    /// The message the service refused the kept token with, while nothing
    /// runs until a sign-in.
    pub stopped: Option<String>,
    pub paused: Option<Paused>,
    /// `X-RateLimit-Remaining` from the last answer that carried it.
    pub rate_remaining: Option<u32>,
    pub last_call: Option<LastCall>,
    /// Jobs due now.
    pub due: usize,
    /// Jobs waiting on a settle or a backoff.
    pub waiting: usize,
    /// The run under way, if one is.
    pub running: usize,
    /// When the soonest waiting send goes out, after any pause.
    pub next_send: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paused {
    pub why: Pause,
    pub until: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastCall {
    pub at: SystemTime,
    /// The HTTP status, or `None` when the service could not be reached.
    pub status: Option<u16>,
}

enum Msg {
    Work(Work),
    SignIn(Service, Credentials),
    SignOut(Service),
    Stop,
}

/// The reports waiting for the shell, and the count that tells it there
/// are some.
#[derive(Clone, Default)]
struct Reports(Arc<Mutex<Queue>>);

#[derive(Default)]
struct Queue {
    reports: Vec<Report>,
    counter: u64,
}

impl Reports {
    /// Queues `report` and returns the count, which never repeats.
    fn queue(&self, report: Report) -> u64 {
        let mut queue = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        queue.reports.push(report);
        queue.counter += 1;
        queue.counter
    }

    fn take(&self) -> Vec<Report> {
        std::mem::take(
            &mut self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reports,
        )
    }
}

/// The facts Diagnostics reads. One writer; a poisoned lock is taken
/// anyway, because the facts are a display copy nothing decides on.
#[derive(Clone, Default)]
struct Mirror(Arc<Mutex<Vec<ServiceFacts>>>);

impl Mirror {
    fn publish(&self, facts: Vec<ServiceFacts>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = facts;
    }

    fn read(&self) -> Vec<ServiceFacts> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The last call to a service and the budget it named.
#[derive(Default)]
struct Calls {
    last: Option<LastCall>,
    remaining: Option<u32>,
}

struct Worker {
    engine: Engine,
    vault: Tokens,
    calls: HashMap<Service, Calls>,
    transport: Transport,
    fetch: Fetch,
    /// When the last request went out, so the next keeps its distance.
    last_sent: Option<Instant>,
    /// `X-RateLimit-Remaining` from the run under way.
    run_remaining: Option<u32>,
    reports: Reports,
    on_report: OnReport,
    mirror: Mirror,
    /// Set by [`Syncer::drop`] before it sends `Msg::Stop`, because a job
    /// under way cannot read the mailbox.
    stopping: Arc<AtomicBool>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Msg>) {
        self.publish();
        loop {
            let now = Instant::now();
            let first = match self.engine.next(now) {
                Next::Run(key) => {
                    self.run_job(key);
                    None
                }
                Next::Wait(until) => match rx.recv_timeout(until.saturating_duration_since(now)) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                Next::Idle => match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => return,
                },
            };
            for msg in first.into_iter().chain(rx.try_iter()) {
                match msg {
                    Msg::Work(work) => self.engine.post(&work, Instant::now()),
                    Msg::SignIn(service, credentials) => self.sign_in(service, credentials),
                    Msg::SignOut(service) => self.sign_out(service),
                    Msg::Stop => return,
                }
            }
            self.publish();
        }
    }

    /// Runs the job the engine picked and tells it how that went. What the
    /// run found becomes a report; a refusal, a rejection and a missing
    /// show become theirs; a throttle, an outage and a transient failure
    /// are the engine's to wait out.
    fn run_job(&mut self, key: Key) {
        let Some(posted) = self.engine.posted(&key).cloned() else {
            self.engine.finished(
                key,
                Outcome {
                    failure: None,
                    remaining: None,
                },
                Instant::now(),
            );
            return;
        };
        let service = key.service;
        let entry = posted.job.entry;
        self.run_remaining = None;
        let session = Session::of(service);
        let outcome = match &posted.job.task {
            Task::Find { title } => session
                .find(&mut |request| self.call(service, request), title)
                .map(|candidates| Report::Found {
                    service,
                    entry,
                    query: title.clone(),
                    candidates,
                }),
            Task::Reconcile {
                media,
                wanted,
                base,
            } => {
                let job = Reconcile {
                    service,
                    user: posted.user,
                    entry,
                    media: *media,
                    wanted: *wanted,
                    base: *base,
                };
                reconcile::run(&job, |call| match call {
                    Call::Read { media } => {
                        session.read(&mut |request| self.call(service, request), media)
                    }
                    Call::Save { media, id, write } => {
                        session.save(&mut |request| self.call(service, request), media, id, write)
                    }
                })
            }
            Task::Describe { media } => session
                .describe(&mut |request| self.call(service, request), *media)
                .map(|(info, url)| Report::Described {
                    service,
                    media: *media,
                    info,
                    cover: url.and_then(|url| self.cover(&url)),
                }),
        };
        let failure = outcome.as_ref().err().cloned();
        match outcome {
            Ok(report) => self.queue(report),
            Err(Failure::Refused(message)) => {
                warn!(service = service.tag(), %message, "token refused");
                self.queue(Report::Refused { service, message });
            }
            Err(Failure::Rejected(message)) => match &posted.job.task {
                Task::Reconcile { media, wanted, .. } => {
                    warn!(
                        service = service.tag(),
                        entry = entry.as_i64(),
                        %message,
                        "write rejected"
                    );
                    self.queue(Report::Rejected {
                        service,
                        entry,
                        media: *media,
                        wanted: *wanted,
                        message,
                    });
                }
                Task::Find { title } => {
                    warn!(
                        service = service.tag(),
                        entry = entry.as_i64(),
                        title = %title,
                        %message,
                        "search rejected"
                    );
                    self.queue(Report::SearchRefused {
                        service,
                        entry,
                        query: title.clone(),
                        message,
                    });
                }
                // Said as an answer with nothing in it, the way a show the
                // service cannot find is. The row keeps what it had and is
                // stamped, so the same refused request does not go out again
                // on every change of work, as it did while only logged here.
                Task::Describe { media } => {
                    warn!(
                        service = service.tag(),
                        media = media.0,
                        %message,
                        "show info unreadable"
                    );
                    self.queue(Report::Described {
                        service,
                        media: *media,
                        info: ShowInfo::default(),
                        cover: None,
                    });
                }
            },
            Err(Failure::NotFound) => match &posted.job.task {
                Task::Reconcile { media, .. } => {
                    info!(
                        service = service.tag(),
                        entry = entry.as_i64(),
                        media = media.0,
                        "media gone from the service"
                    );
                    self.queue(Report::Gone {
                        service,
                        entry,
                        media: *media,
                    });
                }
                // Asked without the token, a show the account can see may
                // still read as missing, so only a reconcile says it is
                // gone. Saying it has nothing keeps it from being asked
                // again before the week is out.
                Task::Describe { media } => {
                    info!(
                        service = service.tag(),
                        media = media.0,
                        "show info not found"
                    );
                    self.queue(Report::Described {
                        service,
                        media: *media,
                        info: ShowInfo::default(),
                        cover: None,
                    });
                }
                Task::Find { title } => warn!(
                    service = service.tag(),
                    entry = entry.as_i64(),
                    title = %title,
                    "search answered not found"
                ),
            },
            Err(failure @ (Failure::Throttled(_) | Failure::Offline | Failure::Transient)) => {
                info!(
                    service = service.tag(),
                    entry = entry.as_i64(),
                    %failure,
                    "run held back"
                );
            }
        }
        self.engine.finished(
            key,
            Outcome {
                failure,
                remaining: self.run_remaining,
            },
            Instant::now(),
        );
    }

    /// Checks a pasted token against the service. One that has already
    /// passed its `exp` is not sent at all. One that works is kept, and the
    /// lane runs again if a refusal had stopped it; one that does not is
    /// kept nowhere. One the vault would not keep is not signed in either:
    /// every signed request reads the vault, so a token that is not there
    /// would fail the first of them.
    fn sign_in(&mut self, service: Service, credentials: Credentials) {
        self.queue(Report::Checking(service));
        let Credentials::Token(token) = credentials;
        let expires = token.expires();
        if token.expired_at(SystemTime::now()) {
            self.queue(Report::NotSignedIn {
                service,
                message: format!(
                    "This {} token has already expired; get a new one from the site.",
                    service.label()
                ),
            });
            return;
        }
        let session = Session::of(service);
        let checked = session.viewer(&mut |request| self.send(service, request, Some(&token)));
        match checked {
            Ok((user, name)) => {
                if let Err(error) = self.vault.keep(service, token) {
                    warn!(service = service.tag(), %error, "token not kept");
                    self.queue(Report::NotSignedIn {
                        service,
                        message: format!(
                            "Couldn't keep the token in Windows Credential Manager: {error}"
                        ),
                    });
                    return;
                }
                self.engine.signed_in(service);
                info!(service = service.tag(), user = user.0, "signed in");
                self.queue(Report::SignedIn(Account {
                    service,
                    user,
                    name,
                    expires,
                    signed: Signed::In,
                }));
            }
            Err(failure) => {
                warn!(service = service.tag(), %failure, "sign-in failed");
                self.queue(Report::NotSignedIn {
                    service,
                    message: failure.to_string(),
                });
            }
        }
    }

    /// Forgets the token, and the work posted for it: a job that ran on
    /// before the core posts again would meet the empty vault and report
    /// a refusal for an account that is only signed out. A vault that
    /// would not let go is logged and the sign-out reported anyway; the
    /// next sign-in overwrites what stayed.
    fn sign_out(&mut self, service: Service) {
        if let Err(error) = self.vault.forget(service) {
            warn!(service = service.tag(), %error, "token not forgotten");
        }
        self.engine.signed_out(service);
        info!(service = service.tag(), "signed out");
        self.queue(Report::SignedOut(service));
    }

    fn queue(&self, report: Report) {
        let count = self.reports.queue(report);
        (self.on_report)(count);
    }

    /// Fetches a cover, or nothing when it cannot be had: the info stands
    /// without it, and the next fetch tries again.
    fn cover(&mut self, url: &str) -> Option<Cover> {
        let extension = url
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_owned())
            .unwrap_or_default();
        match (self.fetch)(url) {
            Ok(bytes) => Some(Cover { bytes, extension }),
            Err(failure) => {
                warn!(url, %failure, "cover not fetched");
                None
            }
        }
    }

    /// Sends a request on `service`'s behalf with the kept token, or
    /// refuses before sending when there is none or it has passed.
    fn call(&mut self, service: Service, request: &Request) -> Result<Response, Failure> {
        let token = if request.signed {
            Some(self.token_for(service)?)
        } else {
            None
        };
        self.send(service, request, token.as_ref())
    }

    fn token_for(&self, service: Service) -> Result<Token, Failure> {
        let kept = self.vault.token(service).map_err(|error| {
            Failure::Refused(format!(
                "Couldn't read the {} token from Windows Credential Manager: {error}",
                service.label()
            ))
        })?;
        let Some(token) = kept else {
            return Err(Failure::Refused(format!(
                "No {} token is kept on this computer; paste one in Settings.",
                service.label()
            )));
        };
        if token.expired_at(SystemTime::now()) {
            return Err(Failure::Refused(format!(
                "The {} token has expired; paste a new one in Settings.",
                service.label()
            )));
        }
        Ok(token)
    }

    /// The one way out: paced to the spacing, and noted for the facts.
    fn send(
        &mut self,
        service: Service,
        request: &Request,
        token: Option<&Token>,
    ) -> Result<Response, Failure> {
        if let Some(last) = self.last_sent {
            let until = last + SPACING;
            // Waited in slices rather than one sleep. A job cannot read the
            // mailbox, so a single sleep held `Msg::Stop` for as long as the
            // spacing and `Syncer::drop` joined on it: closing the app sat
            // out the pacing of a call it was about to throw away.
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                if self.stopping.load(Ordering::Relaxed) {
                    return Err(Failure::Offline);
                }
                thread::sleep(left.min(STOP_CHECK));
            }
        }
        self.last_sent = Some(Instant::now());
        let result = (self.transport)(request, token);
        let calls = self.calls.entry(service).or_default();
        calls.last = Some(LastCall {
            at: SystemTime::now(),
            status: result.as_ref().ok().map(|response| response.status),
        });
        if let Ok(Response {
            rate_remaining: Some(remaining),
            ..
        }) = &result
        {
            calls.remaining = Some(*remaining);
            self.run_remaining = Some(*remaining);
        }
        result
    }

    fn publish(&self) {
        let now = Instant::now();
        let clock = SystemTime::now();
        let facts = Service::ALL
            .into_iter()
            .map(|service| {
                let lane = self.engine.facts(service, now);
                let calls = self.calls.get(&service);
                ServiceFacts {
                    service,
                    stopped: lane.stopped,
                    paused: lane.paused.map(|(why, until)| Paused {
                        why,
                        until: clock + until.saturating_duration_since(now),
                    }),
                    rate_remaining: calls.and_then(|calls| calls.remaining),
                    last_call: calls.and_then(|calls| calls.last),
                    due: lane.due,
                    waiting: lane.waiting,
                    running: lane.running,
                    next_send: lane
                        .next_due
                        .map(|at| clock + at.saturating_duration_since(now)),
                }
            })
            .collect();
        self.mirror.publish(facts);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use ryuuji_core::{
        Asked, Job, ListEntryId, ListState, ListStatus, MediaId, Remote, ServiceWork, UserId, When,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::engine::tests::entries;
    use crate::tests::token_until;
    use crate::vault::{Doing, VaultError};

    const VIEWER: &str = include_str!("../fixtures/anilist/viewer.json");
    const SEARCH: &str = include_str!("../fixtures/anilist/search.json");
    const READ_LISTED: &str = include_str!("../fixtures/anilist/read_listed.json");
    const SAVED: &str = include_str!("../fixtures/anilist/save.json");
    const DESCRIBED: &str = include_str!("../fixtures/anilist/describe.json");
    const NOT_FOUND: &str = include_str!("../fixtures/anilist/not_found_404.json");
    const REJECTED: &str = include_str!("../fixtures/anilist/validation_400.json");

    const ANILIST: Service = Service::AniList;

    /// What one request looked like to the fake transport.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Seen {
        operation: &'static str,
        variables: Value,
        signed: bool,
    }

    /// A transport answering from the fixtures by the operation asked, and
    /// recording what it was asked.
    fn fake(seen: Arc<Mutex<Vec<Seen>>>) -> Transport {
        Box::new(move |request: &Request, token: Option<&Token>| {
            let query = request.body["query"].as_str().expect("a query");
            let operation = if query.contains("Viewer") {
                "viewer"
            } else if query.contains("coverImage") {
                "describe"
            } else if query.contains("SaveMediaListEntry") {
                "save"
            } else if query.contains("mediaListEntry") {
                "read"
            } else {
                "find"
            };
            seen.lock().unwrap().push(Seen {
                operation,
                variables: request.body["variables"].clone(),
                signed: token.is_some(),
            });
            Ok(Response::fixture(match operation {
                "viewer" => VIEWER,
                "describe" => DESCRIBED,
                "save" => SAVED,
                "read" => READ_LISTED,
                _ => SEARCH,
            }))
        })
    }

    /// A transport answering every request as `fixture`.
    fn always(fixture: &'static str) -> Transport {
        Box::new(move |_: &Request, _: Option<&Token>| Ok(Response::fixture(fixture)))
    }

    fn started(transport: Transport) -> (Syncer, Receiver<u64>) {
        started_with(Tokens::default(), transport)
    }

    /// A picture fetch that answers every address with its own text, and
    /// records the addresses it was asked for.
    fn pictures(asked: Arc<Mutex<Vec<String>>>) -> Fetch {
        Box::new(move |url: &str| {
            asked.lock().unwrap().push(url.to_owned());
            Ok(format!("picture at {url}").into_bytes())
        })
    }

    fn started_with(vault: Tokens, transport: Transport) -> (Syncer, Receiver<u64>) {
        started_fetching(vault, transport, pictures(Arc::default()))
    }

    fn started_fetching(
        vault: Tokens,
        transport: Transport,
        fetch: Fetch,
    ) -> (Syncer, Receiver<u64>) {
        let (tx, rx) = channel();
        let syncer = start_with(
            vault,
            transport,
            fetch,
            Box::new(move |count| {
                let _ = tx.send(count);
            }),
        )
        .unwrap();
        (syncer, rx)
    }

    fn next_count(counts: &Receiver<u64>) -> u64 {
        counts
            .recv_timeout(Duration::from_secs(30))
            .expect("a report in time")
    }

    /// Polls until `probe` answers; the mirror is published a step after
    /// the report that a test waited on.
    fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = probe() {
                return found;
            }
            assert!(Instant::now() < deadline, "not in time");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    fn reconcile_now(entry: ryuuji_core::EntryId, wanted: ListState, base: ListState) -> Work {
        Work {
            services: vec![ServiceWork {
                service: ANILIST,
                user: UserId(7),
                jobs: vec![Job {
                    entry,
                    task: Task::Reconcile {
                        media: MediaId(154587),
                        wanted,
                        base: Some(base),
                    },
                    when: When::Now,
                }],
            }],
        }
    }

    #[test]
    fn a_show_not_found_without_the_token_is_described_as_empty_not_gone() {
        let (syncer, counts) = started(always(NOT_FOUND));
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        syncer.post(&Work {
            services: vec![ServiceWork {
                service: ANILIST,
                user: UserId(7),
                jobs: vec![Job {
                    entry,
                    task: Task::Describe {
                        media: MediaId(154587),
                    },
                    when: When::Now,
                }],
            }],
        });
        next_count(&counts);
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Described {
                service: ANILIST,
                media: MediaId(154587),
                info: ShowInfo::default(),
                cover: None,
            }]
        );
    }

    /// Only logged, a rejected search left no mark, so the core still read
    /// the show as never looked up and posted the same search again on every
    /// change of work.
    #[test]
    fn a_rejected_search_is_reported_to_the_core_rather_than_only_logged() {
        let mut vault = Tokens::default();
        vault.keep(ANILIST, token_until(4_102_444_800)).unwrap();
        let (syncer, counts) = started_with(vault, always(REJECTED));
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        syncer.post(&Work {
            services: vec![ServiceWork {
                service: ANILIST,
                user: UserId(7),
                jobs: vec![Job {
                    entry,
                    task: Task::Find {
                        title: "Show".into(),
                    },
                    when: When::Now,
                }],
            }],
        });
        next_count(&counts);
        let reports = syncer.take_reports();
        assert!(
            matches!(
                &reports[..],
                [Report::SearchRefused { service: ANILIST, entry: got, query, .. }]
                    if *got == entry && query == "Show"
            ),
            "{reports:?}"
        );
    }

    /// The same for show info, said as an answer with nothing in it, which
    /// the store keeps without emptying what it had.
    #[test]
    fn a_rejected_describe_is_reported_as_empty_rather_than_only_logged() {
        let (syncer, counts) = started(always(REJECTED));
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        syncer.post(&Work {
            services: vec![ServiceWork {
                service: ANILIST,
                user: UserId(7),
                jobs: vec![Job {
                    entry,
                    task: Task::Describe {
                        media: MediaId(154587),
                    },
                    when: When::Now,
                }],
            }],
        });
        next_count(&counts);
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Described {
                service: ANILIST,
                media: MediaId(154587),
                info: ShowInfo::default(),
                cover: None,
            }]
        );
    }

    #[test]
    fn a_posted_describe_reports_the_info_and_a_cover_fetched_without_the_token() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (syncer, counts) = started_fetching(
            Tokens::default(),
            fake(seen.clone()),
            pictures(asked.clone()),
        );
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        syncer.post(&Work {
            services: vec![ServiceWork {
                service: ANILIST,
                user: UserId(7),
                jobs: vec![Job {
                    entry,
                    task: Task::Describe {
                        media: MediaId(154587),
                    },
                    when: When::Now,
                }],
            }],
        });
        next_count(&counts);
        let url = "https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/bx154587-qQTzQnEJJ3oB.jpg";
        let reports = syncer.take_reports();
        let [
            Report::Described {
                service,
                media,
                info,
                cover: Some(cover),
            },
        ] = &reports[..]
        else {
            panic!("one description with a cover: {reports:?}");
        };
        assert_eq!((*service, *media), (ANILIST, MediaId(154587)));
        assert_eq!(info.studio.as_deref(), Some("MADHOUSE"));
        assert_eq!(cover.extension, "jpg");
        assert_eq!(cover.bytes, format!("picture at {url}").into_bytes());
        assert_eq!(*asked.lock().unwrap(), vec![url.to_owned()]);
        // Asked with no token kept at all, which a signed call would refuse.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Seen {
                operation: "describe",
                variables: json!({ "id": 154587 }),
                signed: false,
            }]
        );
    }

    #[test]
    fn a_sign_in_is_checked_then_kept_and_a_posted_reconcile_is_reported() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (syncer, counts) = started(fake(seen.clone()));
        let token = token_until(4_102_444_800);
        syncer.sign_in(ANILIST, Credentials::Token(token.clone()));
        assert_eq!(next_count(&counts), 1);
        assert_eq!(next_count(&counts), 2);
        let account = Account {
            service: ANILIST,
            user: UserId(7_749_611),
            name: "RyuujiApp".into(),
            expires: token.expires(),
            signed: Signed::In,
        };
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Checking(ANILIST), Report::SignedIn(account)]
        );
        assert_eq!(syncer.take_reports(), vec![]);

        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 8, 0);
        let wanted = state(ListStatus::Watching, 9, 0);
        syncer.post(&reconcile_now(entry, wanted, base));
        assert_eq!(next_count(&counts), 3);
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Reconciled {
                service: ANILIST,
                user: UserId(7),
                entry,
                media: MediaId(154587),
                asked: Asked { wanted, base },
                remote: Remote::Listed {
                    id: Some(ListEntryId(599_961_536)),
                    state: wanted,
                },
            }]
        );

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.iter()
                .map(|call| (call.operation, call.signed))
                .collect::<Vec<_>>(),
            vec![("viewer", true), ("read", true), ("save", true)]
        );
        assert_eq!(
            seen[2].variables,
            json!({ "id": 599_961_536, "mediaId": 154587, "progress": 9 })
        );

        let facts = wait_for(|| {
            syncer
                .facts()
                .into_iter()
                .find(|facts| facts.service == ANILIST && facts.running == 0 && facts.due == 0)
        });
        assert_eq!(facts.stopped, None);
        assert_eq!(facts.rate_remaining, Some(28));
        assert_eq!(facts.last_call.map(|call| call.status), Some(Some(200)));
        assert_eq!(facts.paused, None);
        assert_eq!((facts.due, facts.waiting, facts.running), (0, 0, 0));
    }

    #[test]
    fn a_passed_token_is_refused_before_any_request_and_so_is_a_lane_with_none() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (syncer, counts) = started(fake(seen.clone()));
        assert_eq!(
            wait_for(|| syncer.facts().into_iter().find(|f| f.service == ANILIST)).stopped,
            None
        );
        syncer.sign_in(ANILIST, Credentials::Token(token_until(1)));
        assert_eq!(next_count(&counts), 1);
        assert_eq!(next_count(&counts), 2);
        assert_eq!(
            syncer.take_reports(),
            vec![
                Report::Checking(ANILIST),
                Report::NotSignedIn {
                    service: ANILIST,
                    message: "This AniList token has already expired; get a new one from the site."
                        .into(),
                },
            ]
        );

        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let held = state(ListStatus::Watching, 3, 0);
        syncer.post(&reconcile_now(
            entry,
            state(ListStatus::Watching, 4, 0),
            held,
        ));
        assert_eq!(next_count(&counts), 3);
        let message = "No AniList token is kept on this computer; paste one in Settings.";
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Refused {
                service: ANILIST,
                message: message.into(),
            }]
        );
        assert!(seen.lock().unwrap().is_empty(), "a request went out");
        let facts = wait_for(|| {
            syncer
                .facts()
                .into_iter()
                .find(|facts| facts.service == ANILIST && facts.stopped.is_some())
        });
        assert_eq!(facts.stopped.as_deref(), Some(message));
        assert_eq!(facts.last_call, None);

        // The sign-out lifts the refusal with the job it stopped.
        syncer.sign_out(ANILIST);
        assert_eq!(next_count(&counts), 4);
        assert_eq!(syncer.take_reports(), vec![Report::SignedOut(ANILIST)]);
        let facts = wait_for(|| {
            syncer
                .facts()
                .into_iter()
                .find(|facts| facts.service == ANILIST && facts.stopped.is_none())
        });
        assert_eq!((facts.due, facts.waiting, facts.running), (0, 0, 0));
    }

    /// After a relaunch the core still holds the account and the vault the
    /// token, and this process saw no sign-in: the work the core posts
    /// runs, and the reconcile's signed requests read the token from the
    /// vault.
    #[test]
    fn work_posted_for_an_account_this_process_never_saw_sign_in_runs_on_the_kept_token() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut vault = Tokens::default();
        vault.keep(ANILIST, token_until(4_102_444_800)).unwrap();
        let (syncer, counts) = started_with(vault, fake(seen.clone()));

        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 8, 0);
        let wanted = state(ListStatus::Watching, 9, 0);
        syncer.post(&reconcile_now(entry, wanted, base));
        assert_eq!(next_count(&counts), 1);
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Reconciled {
                service: ANILIST,
                user: UserId(7),
                entry,
                media: MediaId(154587),
                asked: Asked { wanted, base },
                remote: Remote::Listed {
                    id: Some(ListEntryId(599_961_536)),
                    state: wanted,
                },
            }]
        );
        // Both requests went out signed, and nothing was pasted: the token
        // was the vault's.
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .map(|call| (call.operation, call.signed))
                .collect::<Vec<_>>(),
            vec![("read", true), ("save", true)]
        );
        let facts = wait_for(|| {
            syncer
                .facts()
                .into_iter()
                .find(|facts| facts.service == ANILIST && facts.running == 0 && facts.due == 0)
        });
        assert_eq!(facts.stopped, None);
        assert_eq!(facts.last_call.map(|call| call.status), Some(Some(200)));
    }

    #[test]
    fn a_token_the_vault_will_not_keep_is_not_signed_in_and_never_used() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let vault = Tokens::refusing(|| VaultError::Store {
            doing: Doing::Write,
            message: "Platform failure: Windows error code 5".into(),
        });
        let (syncer, counts) = started_with(vault, fake(seen.clone()));
        syncer.sign_in(ANILIST, Credentials::Token(token_until(4_102_444_800)));
        assert_eq!(next_count(&counts), 1);
        assert_eq!(next_count(&counts), 2);
        assert_eq!(
            syncer.take_reports(),
            vec![
                Report::Checking(ANILIST),
                Report::NotSignedIn {
                    service: ANILIST,
                    message: "Couldn't keep the token in Windows Credential Manager: writing \
                              the credential failed: Platform failure: Windows error code 5"
                        .into(),
                },
            ]
        );

        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        syncer.post(&reconcile_now(
            entry,
            state(ListStatus::Watching, 4, 0),
            state(ListStatus::Watching, 3, 0),
        ));
        assert_eq!(next_count(&counts), 3);
        assert_eq!(
            syncer.take_reports(),
            vec![Report::Refused {
                service: ANILIST,
                message: "No AniList token is kept on this computer; paste one in Settings.".into(),
            }]
        );
        // The check went out with the pasted token; nothing after it did.
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .map(|call| (call.operation, call.signed))
                .collect::<Vec<_>>(),
            vec![("viewer", true)]
        );
    }
}
