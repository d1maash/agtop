use crate::model::{AgentKind, Session};
use crate::sources;
use anyhow::Result;
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Immutable view of all known sessions. Published by the watcher thread,
/// consumed by the UI. Cloning is an `Arc` bump — no session data copies.
pub type Snapshot = Arc<Vec<Session>>;

/// Cell holding the latest published snapshot. The mutex is held only long
/// enough to clone the inner `Arc`, so UI reads never wait on watcher work.
pub type Shared = Arc<Mutex<Snapshot>>;

/// Cheap, non-blocking read of the current snapshot.
pub fn current(shared: &Shared) -> Snapshot {
    shared.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Parse every known jsonl up-front and seed the first snapshot. Returns the
/// shared cell plus the owned `HashMap` that the watcher thread will keep
/// mutating in place.
pub fn build_initial_state() -> (Shared, HashMap<PathBuf, Session>) {
    let map = sources::initial_scan().unwrap_or_default();
    let snap: Vec<Session> = map.values().cloned().collect();
    let shared = Arc::new(Mutex::new(Arc::new(snap)));
    (shared, map)
}

fn publish(shared: &Shared, map: &HashMap<PathBuf, Session>) {
    let snap: Vec<Session> = map.values().cloned().collect();
    let new = Arc::new(snap);
    *shared.lock().unwrap_or_else(|p| p.into_inner()) = new;
}

/// Spawn a notify watcher + worker thread. The worker owns `map` privately
/// (no locks during file IO), debounces filesystem events for `debounce`,
/// re-tails affected files, and republishes a snapshot when anything
/// actually changed — throttled so a burst of events doesn't thrash.
pub fn spawn(shared: Shared, mut map: HashMap<PathBuf, Session>) -> Result<()> {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    for root in [sources::claude_root(), sources::codex_root()] {
        if let Some(p) = root {
            if p.exists() {
                let _ = watcher.watch(&p, RecursiveMode::Recursive);
            }
        }
    }

    thread::spawn(move || {
        // Move watcher into the thread so it isn't dropped (which stops watching).
        let _keep = watcher;

        let debounce = Duration::from_millis(400);
        let safety_scan = Duration::from_secs(15);
        // UI ticks at 250ms; republishing faster than that would be wasted
        // work, and burst-of-events spam would otherwise allocate a fresh
        // Vec<Session> per event.
        let publish_min_interval = Duration::from_millis(250);

        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let mut last_safety = Instant::now();
        let mut last_publish = Instant::now() - publish_min_interval;
        let mut dirty = true; // publish the initial-scan state once on entry

        loop {
            // Block briefly for events, then process whatever is queued.
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(ev)) => collect(&ev, &mut pending),
                Ok(Err(_)) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(res) = rx.try_recv() {
                if let Ok(ev) = res {
                    collect(&ev, &mut pending);
                }
            }

            // Flush paths whose debounce elapsed.
            let now = Instant::now();
            let ready: Vec<PathBuf> = pending
                .iter()
                .filter(|(_, t)| now.duration_since(**t) >= debounce)
                .map(|(p, _)| p.clone())
                .collect();
            for path in ready {
                pending.remove(&path);
                if tail_path(&mut map, &path) {
                    dirty = true;
                }
            }

            if now.duration_since(last_safety) >= safety_scan {
                last_safety = now;

                for (kind, path) in sources::list_files() {
                    if !map.contains_key(&path) {
                        let mut sess = Session::new(kind, path.clone());
                        let _ = sources::tail(&mut sess, true);
                        map.insert(path, sess);
                        dirty = true;
                    }
                }
                // Re-tail anything that grew, in case events were dropped.
                // `tail` short-circuits on metadata.len() == file_offset, so
                // unchanged files cost one stat call (no open, no lock).
                let paths: Vec<PathBuf> = map.keys().cloned().collect();
                for p in &paths {
                    if let Some(s) = map.get_mut(p) {
                        if sources::tail(s, true).unwrap_or(false) {
                            dirty = true;
                        }
                    }
                }
            }

            if dirty && now.duration_since(last_publish) >= publish_min_interval {
                publish(&shared, &map);
                dirty = false;
                last_publish = now;
            }
        }
    });

    Ok(())
}

fn collect(ev: &notify::Event, pending: &mut HashMap<PathBuf, Instant>) {
    let interesting = matches!(
        ev.kind,
        EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Any)
            | EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Metadata(_))
    );
    if !interesting {
        return;
    }
    let now = Instant::now();
    for p in &ev.paths {
        if sources::classify(p).is_some() {
            pending.insert(p.clone(), now);
        }
    }
}

fn tail_path(map: &mut HashMap<PathBuf, Session>, path: &PathBuf) -> bool {
    let entry = map.entry(path.clone()).or_insert_with(|| {
        let kind = sources::classify(path).unwrap_or(AgentKind::Claude);
        Session::new(kind, path.clone())
    });
    sources::tail(entry, true).unwrap_or(false)
}
