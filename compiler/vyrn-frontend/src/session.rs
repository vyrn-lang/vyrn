//! What a host keeps between analyses: the editor's per-body recheck, its
//! judgment memo, the walks its lowering reuses, the disk as it last saw it,
//! and the foreign generator cache entries it has warned of.
//!
//! The host owns one [`Session`] and passes it to every load in
//! [`crate::loader::LoadOptions::session`]. The loader stamps it on each
//! program it links ([`crate::ast::Program::session`]), and every pass reads it
//! there. `vyrn check` passes none and keeps nothing. Each cache sits behind
//! its own lock, and every entry answers by its key alone, so threads may share
//! a session and their analyses may interleave.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ast::Type;
use crate::loader::{DiskResolver, ModuleResolver};

/// What one host keeps between its analyses.
pub struct Session {
    /// Whether the placer serves an unchanged imported body its last verdict
    /// ([`crate::movecheck::Judgments`]).
    judge_memo: bool,
    /// What [`Session::read`], [`Session::list_kinds`] and
    /// [`Session::real_path`] answered under the watched roots; `None` before
    /// [`Session::watch`], and every read goes to the disk.
    disk: Mutex<Option<Disk>>,
    /// The generator cache keys already reported as foreign.
    warned: Mutex<HashSet<String>>,
    pub(crate) recheck: Mutex<crate::checker::recheck::Cache>,
    pub(crate) judged: Mutex<crate::movecheck::Judged>,
    /// The walks `vyrn_lower`'s lowering keeps between lowerings.
    pub walks: Mutex<Walks>,
}

/// The count of lowerings, and per recheck entry serial and type arguments,
/// the calls one walk found and the lowering that last read or wrote them.
pub type Walks = (
    u64,
    HashMap<(u64, Vec<Type>), (Vec<(String, HashMap<String, Type>)>, u64)>,
);

/// The disk as a session last saw it under `roots`: each answer stands until
/// [`Session::changed`] names its path. Keyed by the path as the caller
/// spelled it, because an error names that spelling.
struct Disk {
    /// The watched directories, each as [`key_of`] spells it, ending in `/`.
    roots: Vec<String>,
    reads: HashMap<String, Result<String, String>>,
    lists: HashMap<String, Result<Vec<String>, String>>,
    reals: HashMap<String, Option<String>>,
}

/// Locks `m`. A panic under the lock leaves every cache a set of whole
/// entries, so a poisoned lock still holds valid data.
pub fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    /// An empty session. `judge_memo` arms the judgment memo, which only a
    /// host that reads refusals and lowers nothing (the editor) may arm.
    pub fn new(judge_memo: bool) -> Arc<Session> {
        Arc::new(Session {
            judge_memo,
            disk: Mutex::new(None),
            warned: Mutex::default(),
            recheck: Mutex::default(),
            judged: Mutex::default(),
            walks: Mutex::default(),
        })
    }

    /// The judgment memo, if armed.
    pub(crate) fn judge_memo(&self) -> Option<&Mutex<crate::movecheck::Judged>> {
        self.judge_memo.then_some(&self.judged)
    }

    /// Keeps what this session reads and lists, and the real paths it asks
    /// for, under `roots`, until [`Session::changed`] names the path.
    ///
    /// For a host that is told of every change under `roots`: the editor, when
    /// its client sends `workspace/didChangeWatchedFiles`. A host without such
    /// events never calls it, and every load reads the disk again. A path
    /// outside `roots` is always read again.
    pub fn watch(&self, roots: &[String]) {
        *locked(&self.disk) = Some(Disk {
            roots: (roots.iter()).map(|r| format!("{}/", key_of(r))).collect(),
            reads: HashMap::new(),
            lists: HashMap::new(),
            reals: HashMap::new(),
        });
    }

    /// Forgets what [`Session::watch`] kept for `path`, for everything under it
    /// (a removed directory), and its directory's listing (a created or
    /// removed entry).
    pub fn changed(&self, path: &str) {
        let path = key_of(path);
        let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
        let stale = |k: &String| {
            let k = key_of(k);
            k == path
                || k.strip_prefix(path.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        };
        if let Some(d) = locked(&self.disk).as_mut() {
            d.reads.retain(|k, _| !stale(k));
            d.reals.retain(|k, _| !stale(k));
            d.lists.retain(|k, _| !stale(k) && key_of(k) != parent);
        }
    }

    /// [`crate::manifest::real_path`] of `path`, kept as [`Session::watch`]
    /// says.
    pub fn real_path(&self, path: &str) -> Option<String> {
        self.on_disk(|d| &mut d.reals, path, || crate::manifest::real_path(path))
    }

    /// How many bodies this session's checks typed and replayed since the last
    /// call ([`crate::checker::recheck`]), and starts the count again.
    pub fn recheck_tally(&self) -> (u64, u64) {
        std::mem::take(&mut locked(&self.recheck).tally)
    }

    /// Whether `key` is reported as foreign for the first time.
    pub(crate) fn first_warning(&self, key: &str) -> bool {
        locked(&self.warned).insert(key.to_string())
    }

    /// The answer kept for `path` in `table`, or `read`'s, kept when `path`
    /// lies under a watched root.
    fn on_disk<T: Clone>(
        &self,
        table: fn(&mut Disk) -> &mut HashMap<String, T>,
        path: &str,
        read: impl FnOnce() -> T,
    ) -> T {
        let watched = locked(&self.disk).as_mut().and_then(|d| {
            let key = key_of(path);
            (d.roots.iter().any(|r| key.starts_with(r.as_str())))
                .then(|| table(d).get(path).cloned())
        });
        match watched {
            None => read(),
            Some(Some(kept)) => kept,
            Some(None) => {
                let answer = read();
                if let Some(d) = locked(&self.disk).as_mut() {
                    table(d).insert(path.to_string(), answer.clone());
                }
                answer
            }
        }
    }
}

/// The disk as the session saw it. Open buffers are not here: the editor's
/// resolver answers them before it asks the session.
impl ModuleResolver for Session {
    fn read(&self, resolved: &str) -> Result<String, String> {
        self.on_disk(|d| &mut d.reads, resolved, || DiskResolver.read(resolved))
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        self.on_disk(
            |d| &mut d.lists,
            resolved,
            || DiskResolver.list_kinds(resolved),
        )
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        DiskResolver.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        DiskResolver.gen_cache_put(key, value)
    }
}

/// `path` as a watched root or an event compares it: slash-separated, without a
/// trailing slash, and lower-case where the filesystem ignores case.
fn key_of(path: &str) -> String {
    crate::origin::OriginMaps::norm_path_key(path.trim_end_matches(['/', '\\']))
}

/// A program's [`Session`], or none. It belongs to the host, not the program:
/// every two compare equal and print alike, so a key over a program's `Debug`
/// (the derive memo's, a generator's compiled module) is the same in every
/// host.
#[derive(Clone, Default)]
pub struct SessionRef(pub Option<Arc<Session>>);

impl SessionRef {
    pub fn get(&self) -> Option<&Arc<Session>> {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for SessionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionRef")
    }
}

impl PartialEq for SessionRef {
    fn eq(&self, _: &SessionRef) -> bool {
        true
    }
}
