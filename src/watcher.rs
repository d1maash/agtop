use crate::model::Session;
use crate::sources;
use anyhow::Result;
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub type Shared = Arc<Mutex<HashMap<PathBuf, Session>>>;

pub fn build_initial_state() -> Shared {
    let map = sources::initial_scan().unwrap_or_default();
    Arc::new(Mutex::new(map))
}

/// Spawn a notify watcher + worker thread. Worker debounces events for
/// `debounce` and re-tails affected files. Also runs a safety scan every
/// `safety_scan` to pick up missed events.
pub fn spawn(shared: Shared) -> Result<()> {
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

        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let mut last_safety = Instant::now();

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
            if !ready.is_empty() {
                let mut map = shared.lock().unwrap();
                for path in ready {
                    pending.remove(&path);
                    process_path(&path, &mut map);
                }
            }

            if now.duration_since(last_safety) >= safety_scan {
                last_safety = now;
                let mut map = shared.lock().unwrap();
                let known: std::collections::HashSet<PathBuf> = map.keys().cloned().collect();
                for (kind, path) in sources::list_files() {
                    if !known.contains(&path) {
                        // new file appeared
                        let mut sess = Session::new(kind, path.clone());
                        let _ = sources::tail(&mut sess, true);
                        map.insert(path, sess);
                    }
                }
                // Re-tail anything that grew, in case events were dropped.
                let paths: Vec<PathBuf> = map.keys().cloned().collect();
                for p in paths {
                    if let Some(s) = map.get_mut(&p) {
                        let _ = sources::tail(s, true);
                    }
                }
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

fn process_path(path: &PathBuf, map: &mut HashMap<PathBuf, Session>) {
    let entry = map.entry(path.clone()).or_insert_with(|| {
        let kind = sources::classify(path).unwrap_or(crate::model::AgentKind::Claude);
        Session::new(kind, path.clone())
    });
    let _ = sources::tail(entry, true);
}
