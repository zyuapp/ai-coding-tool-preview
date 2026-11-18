//! The provider every engine runs as: threads' sessions kept warm in the channel's pool, each read
//! back as the engine's own, and the filings that hold a thread while it is archived or restored.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, FutureExt, Shared};

use crate::provider::{AgentProvider, ProviderResult, RunInput};
use crate::session_pool::{PooledSession, SessionHooks, SessionOpener, SessionPool};
use crate::types::{Continuation, CoordinationRole};

/// How one engine opens a thread's session, and what it can do with one between runs.
pub trait SessionEngine: Send + Sync + Sized + 'static {
    type Session: PooledSession;

    /// Everything a run's session is built with. A run that disagrees needs a session of its own.
    fn session_key(&self, input: &RunInput) -> String;

    /// A session for the run's thread, before the pool holds it.
    fn open(&self, key: String, hooks: SessionHooks) -> Arc<Self::Session>;

    /// Starts a session the pool now holds, for the run that opened it.
    fn start(&self, _session: &Arc<Self::Session>, _seed: &RunInput) {}

    /// Kills one background process of the session. An engine that lists none has none to stop.
    fn stop_process(_session: &Arc<Self::Session>, _process_id: &str) -> bool {
        false
    }

    /// Offers the title to the engine's own record of the thread. One that keeps none says so.
    fn label(_session: &Arc<Self::Session>, _title: &str) -> bool {
        false
    }

    /// Files the thread away in the engine's own record, or brings it back. One that keeps no
    /// archive leaves that to the app.
    fn archive_thread(
        _provider: &PooledProvider<Self>,
        _task_id: &str,
        _continuation: Option<Continuation>,
        _archived: bool,
    ) -> bool {
        false
    }
}

type Filing = Shared<BoxFuture<'static, ()>>;

/// Runs one engine's threads on the channel's shared pool.
pub struct PooledProvider<E> {
    pool: SessionPool,
    /// The last filing asked of each thread, so an archive and a restore land in the order given
    /// and a run waits for both.
    filings: Arc<Mutex<HashMap<String, (u64, Filing)>>>,
    next_filing: Arc<AtomicU64>,
    engine: Arc<E>,
}

impl<E> Clone for PooledProvider<E> {
    fn clone(&self) -> Self {
        PooledProvider {
            pool: self.pool.clone(),
            filings: self.filings.clone(),
            next_filing: self.next_filing.clone(),
            engine: self.engine.clone(),
        }
    }
}

impl<E: SessionEngine> PooledProvider<E> {
    pub fn with_engine(pool: SessionPool, engine: E) -> Self {
        PooledProvider {
            pool,
            filings: Arc::default(),
            next_filing: Arc::default(),
            engine: Arc::new(engine),
        }
    }

    pub fn pool(&self) -> &SessionPool {
        &self.pool
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The thread's live session, when this engine opened it.
    pub fn session(&self, task_id: &str) -> Option<Arc<E::Session>> {
        self.pool
            .live_session(task_id)?
            .as_any()
            .downcast::<E::Session>()
            .ok()
    }

    fn pending_filing(&self, task_id: &str) -> Option<Filing> {
        self.filings
            .lock()
            .unwrap()
            .get(task_id)
            .map(|(_, filing)| filing.clone())
    }

    /// Files the thread after its last filing has landed. A filing that fails changes nothing.
    pub fn file(&self, task_id: &str, filing: BoxFuture<'static, anyhow::Result<()>>) {
        let id = self.next_filing.fetch_add(1, Ordering::Relaxed);
        let previous = self.pending_filing(task_id);
        let filings = self.filings.clone();
        let task = task_id.to_string();
        let queued: Filing = async move {
            if let Some(previous) = previous {
                previous.await;
            }
            let _ = filing.await;
            let mut filings = filings.lock().unwrap();
            if filings
                .get(&task)
                .is_some_and(|(current, _)| *current == id)
            {
                filings.remove(&task);
            }
        }
        .boxed()
        .shared();
        self.filings
            .lock()
            .unwrap()
            .insert(task_id.to_string(), (id, queued.clone()));
        tokio::spawn(queued);
    }
}

impl<E: SessionEngine> AgentProvider for PooledProvider<E> {
    /// Runs on the thread's session. A thread still being filed away or brought back waits for
    /// that filing to land first.
    fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        let key = self.engine.session_key(&input);
        let pool = self.pool.clone();
        let filing = self.pending_filing(&input.task_id);
        let (opens, starts) = (self.engine.clone(), self.engine.clone());
        async move {
            if let Some(filing) = filing {
                filing.await;
            }
            let seed = input.clone();
            let session_key = key.clone();
            pool.execute(
                input,
                key,
                SessionOpener {
                    open: move |hooks| opens.open(session_key, hooks),
                    start: move |session: &Arc<E::Session>| starts.start(session, &seed),
                },
            )
            .await
        }
        .boxed()
    }

    fn stop_process(&self, task_id: &str, process_id: &str) -> bool {
        self.session(task_id)
            .is_some_and(|session| E::stop_process(&session, process_id))
    }

    fn label_thread(&self, task_id: &str, title: &str) -> bool {
        self.session(task_id)
            .is_some_and(|session| E::label(&session, title))
    }

    fn archive_thread(
        &self,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        E::archive_thread(self, task_id, continuation, archived)
    }

    fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>) {
        tokio::spawn(self.pool.retire_out_of_role(task_id, role));
    }

    fn close_all(&self) {
        self.pool.close_all();
    }
}
