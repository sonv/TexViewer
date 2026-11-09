//! Optional, bounded discovery of custom styles in the user's TeX search path.
//!
//! Only fully classified custom-style locations are cached. The project loader
//! still reads each discovered style (or its live-buffer override) every time.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_CAPACITY: usize = 256;
const QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const LOAD_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_QUERIES_PER_LOAD: usize = 32;
const MAX_PENDING_REFRESHES: usize = 32;
#[cfg(unix)]
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct QueryKey {
    base: PathBuf,
    environment: Vec<(OsString, OsString)>,
    arguments: Vec<OsString>,
}

struct CacheEntry {
    checked: Instant,
    value: Option<PathBuf>,
}

struct RefreshRequest {
    key: QueryKey,
    checked: Instant,
}

#[derive(Default)]
struct QueryCache {
    entries: HashMap<QueryKey, CacheEntry>,
    refreshing: HashSet<QueryKey>,
}

impl QueryCache {
    fn get(
        &mut self,
        key: &QueryKey,
        now: Instant,
        enqueue: impl FnOnce(RefreshRequest) -> bool,
    ) -> Option<Option<PathBuf>> {
        let entry = self.entries.get(key)?;
        let value = entry.value.clone();
        // Expiry schedules discovery work without delaying a render. Both
        // positive locations and genuine misses stay usable during refresh.
        if now.saturating_duration_since(entry.checked) >= CACHE_TTL
            && self.refreshing.len() < MAX_PENDING_REFRESHES
            && self.refreshing.insert(key.clone())
            && !enqueue(RefreshRequest {
                key: key.clone(),
                checked: entry.checked,
            })
        {
            self.refreshing.remove(key);
        }
        Some(value)
    }

    fn insert(&mut self, key: QueryKey, value: Option<PathBuf>, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= CACHE_CAPACITY {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.checked)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                checked: now,
                value,
            },
        );
    }

    fn finish_refresh(
        &mut self,
        request: RefreshRequest,
        result: Result<Option<PathBuf>, QueryError>,
        now: Instant,
    ) {
        self.refreshing.remove(&request.key);
        if let Some(entry) = self.entries.get_mut(&request.key) {
            // A queued query can outlive eviction and a newer foreground
            // lookup. Never overwrite that newer result with this old work.
            if entry.checked != request.checked {
                return;
            }
            if let Ok(value) = result {
                entry.value = value;
            }
            // Transient failures preserve the last answer, and retry no more
            // often than successful refreshes rather than on every keystroke.
            entry.checked = now;
        }
    }
}

static CACHE: LazyLock<Mutex<QueryCache>> = LazyLock::new(|| Mutex::new(QueryCache::default()));

// One worker and a bounded queue prevent a document with many expired misses
// from spawning one thread/process per package. Queries run outside CACHE's
// lock, so even a slow executable cannot block a cached render.
static REFRESHER: LazyLock<Option<mpsc::SyncSender<RefreshRequest>>> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::sync_channel::<RefreshRequest>(MAX_PENDING_REFRESHES);
    std::thread::Builder::new()
        .name("mathpreview-style-refresh".into())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                let result = refresh_style(&request.key);
                if let Ok(mut cache) = CACHE.lock() {
                    cache.finish_refresh(request, result, Instant::now());
                }
            }
        })
        .ok()
        .map(|_| sender)
});

fn enqueue_refresh(request: RefreshRequest) -> bool {
    REFRESHER
        .as_ref()
        .is_some_and(|sender| sender.try_send(request).is_ok())
}

fn refresh_style(key: &QueryKey) -> Result<Option<PathBuf>, QueryError> {
    let name = key.arguments.first().ok_or(QueryError::Failed)?;
    let mut resolver = StyleResolver {
        base: Some(key.base.clone()),
        environment: key.environment.clone(),
        remaining_queries: MAX_QUERIES_PER_LOAD,
        deadline: None,
        queries: HashMap::new(),
    };
    // Resolve the candidate and both distribution roots together. Publishing
    // independently refreshed raw answers could misclassify a newly upgraded
    // distribution's files against still-cached paths from the old TeX tree.
    resolve_with(&key.base, Path::new(name), |arguments| {
        resolver.query(&key.base, arguments)
    })
}

fn query_command(key: &QueryKey) -> Command {
    let mut command = Command::new("kpsewhich");
    command.current_dir(&key.base).args(&key.arguments);
    // The cache key captures the environment of the requesting project load.
    // A delayed worker must use that same TeX search path, including variables
    // that were absent then but have since appeared in the process environment.
    for (name, _) in std::env::vars_os().filter(|(name, _)| relevant_environment_key(name)) {
        command.env_remove(name);
    }
    command.envs(key.environment.iter().cloned());
    command
}

/// One budget shared by all preamble files in a single project load. Cached
/// lookups remain available after the uncached-query budget is exhausted.
pub(crate) struct StyleResolver {
    base: Option<PathBuf>,
    environment: Vec<(OsString, OsString)>,
    remaining_queries: usize,
    deadline: Option<Instant>,
    // Raw queries are shared only within this load, especially the two
    // distribution roots. Cross-load caching stores the classified result.
    queries: HashMap<Vec<OsString>, Option<String>>,
}

impl StyleResolver {
    pub(crate) fn new(base: &Path) -> Self {
        // TeX resolves relative TEXINPUTS entries from the document's working
        // directory, not from each discovered package's own directory.
        let base = if base.as_os_str().is_empty() {
            Path::new(".")
        } else {
            base
        }
        .canonicalize()
        .ok();
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| relevant_environment_key(key))
            .collect();
        environment.sort();
        Self {
            base,
            environment,
            remaining_queries: MAX_QUERIES_PER_LOAD,
            deadline: None,
            queries: HashMap::new(),
        }
    }

    pub(crate) fn resolve_style(&mut self, name: &Path) -> Option<PathBuf> {
        if !valid_style_name(name) {
            return None;
        }
        let base = self.base.clone()?;
        let key = QueryKey {
            base: base.clone(),
            environment: self.environment.clone(),
            arguments: vec![name.as_os_str().to_os_string()],
        };
        if let Some(value) = CACHE
            .lock()
            .ok()
            .and_then(|mut cache| cache.get(&key, Instant::now(), enqueue_refresh))
        {
            return value.filter(|path| path.is_file());
        }
        let value = match resolve_with(&base, name, |arguments| self.query(&base, arguments)) {
            Ok(value) => value,
            Err(QueryError::TimedOut) => return None,
            // A machine without TeX should not retry spawning kpsewhich on
            // every edit. A later background retry can discover an install.
            Err(QueryError::Failed) => None,
        };
        if let Ok(mut cache) = CACHE.lock() {
            cache.insert(key, value.clone(), Instant::now());
        }
        value
    }

    fn query(&mut self, base: &Path, arguments: &[OsString]) -> Result<Option<String>, QueryError> {
        let key = QueryKey {
            base: base.to_path_buf(),
            environment: self.environment.clone(),
            arguments: arguments.to_vec(),
        };
        self.query_with(arguments, |timeout| {
            run_bounded(query_command(&key), timeout)
        })
    }

    fn query_with(
        &mut self,
        arguments: &[OsString],
        run: impl FnOnce(Duration) -> Result<Option<String>, QueryError>,
    ) -> Result<Option<String>, QueryError> {
        if let Some(value) = self.queries.get(arguments) {
            return Ok(value.clone());
        }
        let now = Instant::now();
        let deadline = *self.deadline.get_or_insert(now + LOAD_TIMEOUT);
        let timeout = deadline.saturating_duration_since(now).min(QUERY_TIMEOUT);
        if self.remaining_queries == 0 || timeout.is_zero() {
            // A budget miss is not a filesystem miss: the next project load
            // must be allowed to finish resolving this package.
            return Err(QueryError::TimedOut);
        }
        self.remaining_queries -= 1;
        // A transient timeout says nothing about whether the style exists.
        // Retry it on the next load instead of caching a miss for 30 seconds.
        let value = run(timeout)?;
        self.queries.insert(arguments.to_vec(), value.clone());
        Ok(value)
    }
}

fn relevant_environment_key(key: &OsStr) -> bool {
    key.to_str().is_some_and(|key| {
        matches!(key, "PATH" | "HOME" | "USERPROFILE")
            || key.starts_with("TEX")
            || key.starts_with("KPSE")
            || key.starts_with("SELFAUTO")
    })
}

fn valid_style_name(name: &Path) -> bool {
    let Some(name_str) = name.to_str() else {
        return false;
    };
    name_str.len() <= 1024
        && !name_str.chars().any(char::is_control)
        && name.extension() == Some(OsStr::new("sty"))
        && crate::root::path_within_bounds(name_str)
}

fn single_path(output: &str, base: &Path) -> Option<PathBuf> {
    let path = output.trim();
    if path.is_empty() || path.chars().any(char::is_control) {
        return None;
    }
    let path = Path::new(path);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(base.join(path))
    }
}

fn resolve_with(
    base: &Path,
    name: &Path,
    mut query: impl FnMut(&[OsString]) -> Result<Option<String>, QueryError>,
) -> Result<Option<PathBuf>, QueryError> {
    let Some(output) = query(&[
        "--format=tex".into(),
        "--no-mktex=tex".into(),
        "--".into(),
        name.as_os_str().to_os_string(),
    ])?
    else {
        return Ok(None);
    };
    let Some(path) = single_path(&output, base).and_then(|path| path.canonicalize().ok()) else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }

    // Standard package internals must not override MathJax's implementations.
    // TEXMFHOME, TEXMFLOCAL, and ordinary TEXINPUTS directories are outside
    // these distribution roots. Both queries must succeed: knowing only one
    // root cannot prove a package is outside the other. A successfully queried
    // empty variable is unset, but at least one root must be identified.
    let mut roots = Vec::new();
    for variable in ["TEXMFDIST", "TEXMFMAIN"] {
        let output =
            query(&[format!("--var-value={variable}").into()])?.ok_or(QueryError::Failed)?;
        if output.trim().is_empty() {
            continue;
        }
        let root = single_path(&output, base)
            .and_then(|path| path.canonicalize().ok())
            .ok_or(QueryError::Failed)?;
        if !root.is_dir() {
            return Err(QueryError::Failed);
        }
        roots.push(root);
    }
    if roots.is_empty() {
        return Err(QueryError::Failed);
    }
    Ok((!roots.iter().any(|root| path.starts_with(root))).then_some(path))
}

#[derive(Debug, PartialEq, Eq)]
enum QueryError {
    TimedOut,
    Failed,
}

/// Unix sockets let us bound reads without a blocking reader thread, even if
/// an unexpected executable keeps stdout open. The supported release targets
/// are macOS and Linux. Other platforms keep project-local discovery only.
#[cfg(unix)]
fn run_bounded(mut command: Command, timeout: Duration) -> Result<Option<String>, QueryError> {
    use std::io::{ErrorKind, Read};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;

    let Ok((mut reader, writer)) = UnixStream::pair() else {
        return Err(QueryError::Failed);
    };
    if reader.set_nonblocking(true).is_err() {
        return Err(QueryError::Failed);
    }
    let writer: OwnedFd = writer.into();
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(writer));
    let Ok(mut child) = command.spawn() else {
        return Err(QueryError::Failed);
    };
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    let mut exited: Option<std::process::ExitStatus> = None;
    let result = 'poll: loop {
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if output.len() + count > MAX_OUTPUT_BYTES {
                        break 'poll Err(QueryError::Failed);
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break 'poll Err(QueryError::Failed),
            }
        }
        if let Some(status) = exited {
            break if status.success() {
                String::from_utf8(output)
                    .map(Some)
                    .map_err(|_| QueryError::Failed)
            } else if status.code() == Some(1) {
                // kpsewhich reports an absent file with exit status 1.
                Ok(None)
            } else {
                Err(QueryError::Failed)
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain once more after observing exit: all bytes written by
                // this child are now available, with no wait for pipe EOF.
                exited = Some(status);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => break Err(QueryError::TimedOut),
            Err(_) => break Err(QueryError::Failed),
        }
    };
    if exited.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(not(unix))]
fn run_bounded(_command: Command, _timeout: Duration) -> Result<Option<String>, QueryError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mathpreview-kpse-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn custom_style_is_resolved_but_distribution_style_is_not() {
        let scratch = Scratch::new();
        let dist = scratch.0.join("texmf-dist");
        let custom = scratch.0.join("custom styles");
        fs::create_dir_all(&dist).unwrap();
        fs::create_dir_all(&custom).unwrap();
        for dir in [&dist, &custom] {
            fs::write(dir.join("notes.sty"), "original").unwrap();
        }
        let lookup = |target: &Path| {
            resolve_with(&scratch.0, Path::new("notes.sty"), |args| {
                if args.len() == 4 {
                    assert_eq!(args[0], "--format=tex");
                    assert_eq!(args[1], "--no-mktex=tex");
                    assert_eq!(args[2], "--");
                    Ok(Some(format!("{}\n", target.display())))
                } else {
                    Ok(Some(format!("{}\n", dist.display())))
                }
            })
        };
        assert_eq!(
            lookup(&custom.join("notes.sty")),
            Ok(Some(custom.join("notes.sty")))
        );
        assert_eq!(lookup(&dist.join("notes.sty")), Ok(None));
        fs::remove_file(custom.join("notes.sty")).unwrap();
        assert_eq!(lookup(&custom.join("notes.sty")), Ok(None));
    }

    #[test]
    fn lookup_rejects_unknown_roots_and_ambiguous_paths() {
        let scratch = Scratch::new();
        let style = scratch.0.join("notes.sty");
        fs::write(&style, "").unwrap();
        assert_eq!(
            resolve_with(&scratch.0, Path::new("notes.sty"), |args| {
                Ok((args.len() == 4).then(|| style.display().to_string()))
            }),
            Err(QueryError::Failed)
        );
        for output in ["", "first.sty\nsecond.sty", "bad\0path"] {
            assert_eq!(single_path(output, &scratch.0), None);
        }
        assert_eq!(single_path("notes.sty\r\n", &scratch.0), Some(style));
    }

    #[test]
    fn distinct_distribution_roots_require_complete_classification() {
        let scratch = Scratch::new();
        let dist = scratch.0.join("dist");
        let main = scratch.0.join("main");
        fs::create_dir_all(&dist).unwrap();
        fs::create_dir_all(&main).unwrap();
        let style = main.join("package.sty");
        fs::write(&style, "").unwrap();
        let lookup = |main_output: Option<String>| {
            resolve_with(&scratch.0, Path::new("package.sty"), |args| {
                if args.len() == 4 {
                    Ok(Some(style.display().to_string()))
                } else if args[0] == "--var-value=TEXMFDIST" {
                    Ok(Some(dist.display().to_string()))
                } else {
                    Ok(main_output.clone())
                }
            })
        };
        // A timeout, query error, or exhausted load budget must not make the
        // package under the unclassified TEXMFMAIN appear to be custom.
        assert_eq!(lookup(None), Err(QueryError::Failed));
        assert_eq!(lookup(Some(main.display().to_string())), Ok(None));
        assert_eq!(lookup(Some("missing-root".into())), Err(QueryError::Failed));
        assert_eq!(lookup(Some("bad\nroot".into())), Err(QueryError::Failed));
        assert_eq!(lookup(Some("\n".into())), Ok(Some(style)));
    }

    #[test]
    fn style_names_are_bounded_and_do_not_accept_arbitrary_inputs() {
        for name in ["notes.sty", "shared/notes.sty", "-notes.sty"] {
            assert!(valid_style_name(Path::new(name)));
        }
        for name in [
            "",
            "notes.tex",
            "/etc/file.sty",
            "../../../../../file.sty",
            "bad\n.sty",
        ] {
            assert!(!valid_style_name(Path::new(name)));
        }
        assert!(!valid_style_name(Path::new(&format!(
            "{}.sty",
            "x".repeat(1024)
        ))));
    }

    #[test]
    fn stale_cached_misses_return_immediately_and_refresh_once() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: vec![("TEXINPUTS".into(), "/styles".into())],
            arguments: vec!["notes.sty".into()],
        };
        cache.insert(key.clone(), None, now);
        assert_eq!(
            cache.get(&key, now, |_| panic!("fresh result queued")),
            Some(None)
        );
        let mut refresh = None;
        assert_eq!(
            cache.get(&key, now + CACHE_TTL, |request| {
                refresh = Some(request);
                true
            }),
            Some(None)
        );
        assert_eq!(
            cache.get(&key, now + CACHE_TTL, |_| panic!("duplicate refresh")),
            Some(None)
        );
        cache.finish_refresh(
            refresh.unwrap(),
            Ok(Some("new style location".into())),
            now + CACHE_TTL,
        );
        assert_eq!(
            cache.get(&key, now + CACHE_TTL, |_| panic!("finished refresh queued")),
            Some(Some("new style location".into()))
        );
        let mut other = key.clone();
        other.environment[0].1 = "/other".into();
        assert_eq!(
            cache.get(&other, now, |_| panic!("uncached key queued")),
            None
        );
    }

    #[test]
    fn stale_cache_entries_survive_insertion_and_capacity_remains_bounded() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: Vec::new(),
            arguments: vec!["notes.sty".into()],
        };
        cache.insert(key.clone(), Some("path".into()), now);
        for i in 0..=CACHE_CAPACITY {
            let mut next = key.clone();
            next.arguments = vec![format!("file{i}.sty").into()];
            cache.insert(next, None, now + CACHE_TTL);
            if i == 0 {
                assert_eq!(cache.entries.get(&key).unwrap().value, Some("path".into()));
            }
        }
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
    }

    #[test]
    fn stale_hit_refreshes_keep_last_answer_on_failure_and_throttle_retries() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: Vec::new(),
            arguments: vec!["notes.sty".into()],
        };
        cache.insert(key.clone(), Some("original".into()), now);
        let mut checked = now;
        for result in [Err(QueryError::TimedOut), Err(QueryError::Failed)] {
            let stale = checked + CACHE_TTL;
            let mut request = None;
            assert_eq!(
                cache.get(&key, stale, |refresh| {
                    request = Some(refresh);
                    true
                }),
                Some(Some("original".into()))
            );
            cache.finish_refresh(request.unwrap(), result, stale);
            assert_eq!(
                cache.get(&key, stale + CACHE_TTL / 2, |_| panic!(
                    "failure retried early"
                )),
                Some(Some("original".into()))
            );
            checked = stale;
        }
        let mut request = None;
        cache.get(&key, checked + CACHE_TTL, |refresh| {
            request = Some(refresh);
            true
        });
        cache.finish_refresh(
            request.unwrap(),
            Ok(Some("changed".into())),
            checked + CACHE_TTL,
        );
        assert_eq!(cache.entries[&key].value, Some("changed".into()));
        let mut request = None;
        cache.get(&key, checked + CACHE_TTL * 2, |refresh| {
            request = Some(refresh);
            true
        });
        cache.finish_refresh(request.unwrap(), Ok(None), checked + CACHE_TTL * 2);
        assert_eq!(
            cache.entries[&key].value, None,
            "a genuine later miss replaces the old path"
        );
    }

    #[test]
    fn refresh_queue_is_bounded_and_rejected_work_can_retry() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let keys: Vec<_> = (0..=MAX_PENDING_REFRESHES)
            .map(|i| QueryKey {
                base: PathBuf::from("/project"),
                environment: Vec::new(),
                arguments: vec![format!("file{i}.sty").into()],
            })
            .collect();
        for key in &keys {
            cache.insert(key.clone(), Some("cached".into()), now);
        }
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_REFRESHES);
        for key in &keys {
            assert_eq!(
                cache.get(key, now + CACHE_TTL, |request| sender
                    .try_send(request)
                    .is_ok()),
                Some(Some("cached".into()))
            );
        }
        assert_eq!(cache.refreshing.len(), MAX_PENDING_REFRESHES);
        assert!(!cache.refreshing.contains(keys.last().unwrap()));
        let first = receiver.try_recv().unwrap();
        cache.finish_refresh(first, Ok(None), now + CACHE_TTL);
        let key = keys.last().unwrap();
        cache.get(key, now + CACHE_TTL, |_| false);
        assert!(
            !cache.refreshing.contains(key),
            "failed enqueue stuck a key in flight"
        );
        cache.get(key, now + CACHE_TTL, |request| {
            sender.try_send(request).is_ok()
        });
        assert!(cache.refreshing.contains(key));
    }

    #[test]
    fn late_refresh_never_overwrites_newer_or_evicted_entries() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: Vec::new(),
            arguments: vec!["notes.sty".into()],
        };
        cache.insert(key.clone(), Some("original".into()), now);
        let mut request = None;
        cache.get(&key, now + CACHE_TTL, |refresh| {
            request = Some(refresh);
            true
        });
        cache.insert(key.clone(), Some("newer".into()), now + CACHE_TTL);
        cache.finish_refresh(
            request.unwrap(),
            Ok(Some("outdated".into())),
            now + CACHE_TTL,
        );
        assert_eq!(cache.entries[&key].value, Some("newer".into()));
        let mut request = None;
        cache.get(&key, now + CACHE_TTL * 2, |refresh| {
            request = Some(refresh);
            true
        });
        cache.entries.remove(&key);
        cache.finish_refresh(
            request.unwrap(),
            Ok(Some("outdated".into())),
            now + CACHE_TTL * 2,
        );
        assert!(!cache.entries.contains_key(&key));
        assert!(!cache.refreshing.contains(&key));
    }

    #[test]
    fn background_command_uses_the_captured_lookup_environment() {
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: vec![
                ("PATH".into(), "/captured/bin".into()),
                ("TEXINPUTS".into(), "captured/styles".into()),
            ],
            arguments: vec!["notes.sty".into()],
        };
        let command = query_command(&key);
        let environment: HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            environment[OsStr::new("PATH")],
            Some(OsStr::new("/captured/bin"))
        );
        assert_eq!(
            environment[OsStr::new("TEXINPUTS")],
            Some(OsStr::new("captured/styles"))
        );
        for (name, _) in std::env::vars_os().filter(|(name, _)| relevant_environment_key(name)) {
            if !key
                .environment
                .iter()
                .any(|(captured, _)| captured == &name)
            {
                assert_eq!(
                    environment[name.as_os_str()],
                    None,
                    "new environment leaked into old query"
                );
            }
        }
        assert_eq!(command.get_current_dir(), Some(Path::new("/project")));
    }

    #[cfg(unix)]
    #[test]
    fn captured_path_selects_the_lookup_executable() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let executable = scratch.0.join("kpsewhich");
        fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$TEXINPUTS\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let key = QueryKey {
            base: scratch.0.clone(),
            environment: vec![
                ("PATH".into(), scratch.0.as_os_str().to_os_string()),
                ("TEXINPUTS".into(), "captured/styles".into()),
            ],
            arguments: Vec::new(),
        };
        assert_eq!(
            run_bounded(query_command(&key), QUERY_TIMEOUT),
            Ok(Some("captured/styles\n".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn refresh_classifies_a_distribution_upgrade_before_publishing_the_path() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let old_dist = scratch.0.join("old-texmf-dist");
        let new_dist = scratch.0.join("new-texmf-dist");
        for dist in [&old_dist, &new_dist] {
            fs::create_dir_all(dist).unwrap();
            fs::write(dist.join("notes.sty"), "standard package internals").unwrap();
        }
        let executable = scratch.0.join("kpsewhich");
        fs::write(&executable, "#!/bin/sh\ncase \"$1\" in\n  --var-value=*)\n    test ! -f fail-root || exit 2\n    /bin/cat next-root;;\n  *) /bin/cat next-style;;\nesac\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let key = QueryKey {
            base: scratch.0.clone(),
            environment: vec![("PATH".into(), scratch.0.as_os_str().to_os_string())],
            arguments: vec!["notes.sty".into()],
        };
        fs::write(scratch.0.join("next-root"), old_dist.to_str().unwrap()).unwrap();
        fs::write(
            scratch.0.join("next-style"),
            old_dist.join("notes.sty").to_str().unwrap(),
        )
        .unwrap();
        let mut cache = QueryCache::default();
        let now = Instant::now();
        cache.insert(key.clone(), refresh_style(&key).unwrap(), now);
        assert_eq!(cache.entries[&key].value, None);

        // The old distribution still exists when an upgrade switches the
        // candidate to the new tree. Failing root queries must never publish
        // that candidate as a custom style against the old cached roots.
        fs::write(
            scratch.0.join("next-style"),
            new_dist.join("notes.sty").to_str().unwrap(),
        )
        .unwrap();
        fs::write(scratch.0.join("fail-root"), "").unwrap();
        let mut request = None;
        assert_eq!(
            cache.get(&key, now + CACHE_TTL, |refresh| {
                request = Some(refresh);
                true
            }),
            Some(None)
        );
        let result = refresh_style(&key);
        assert_eq!(result, Err(QueryError::Failed));
        cache.finish_refresh(request.unwrap(), result, now + CACHE_TTL);
        assert_eq!(cache.entries[&key].value, None);
        fs::remove_file(scratch.0.join("fail-root")).unwrap();
        fs::write(scratch.0.join("next-root"), new_dist.to_str().unwrap()).unwrap();
        let mut request = None;
        cache.get(&key, now + CACHE_TTL * 2, |refresh| {
            request = Some(refresh);
            true
        });
        let result = refresh_style(&key);
        assert_eq!(result, Ok(None));
        cache.finish_refresh(request.unwrap(), result, now + CACHE_TTL * 2);
        assert_eq!(cache.entries[&key].value, None);
        assert!(old_dist.is_dir());
    }

    #[test]
    fn missing_executable_is_cached_between_project_loads() {
        let scratch = Scratch::new();
        let mut first = StyleResolver::new(&scratch.0);
        first.environment = vec![("PATH".into(), scratch.0.join("no-bin").into_os_string())];
        assert_eq!(first.resolve_style(Path::new("missing.sty")), None);
        assert!(
            first.deadline.is_some(),
            "first lookup should try discovery"
        );
        let mut next = StyleResolver::new(&scratch.0);
        next.environment = first.environment;
        assert_eq!(next.resolve_style(Path::new("missing.sty")), None);
        assert!(
            next.deadline.is_none(),
            "cached missing tool tried discovery again"
        );
        assert_eq!(next.remaining_queries, MAX_QUERIES_PER_LOAD);
    }

    #[test]
    fn exhausted_budget_still_uses_cache_and_does_not_cache_skipped_queries() {
        let scratch = Scratch::new();
        let cached = Path::new("cached.sty");
        let skipped = Path::new("skipped.sty");
        let style = scratch.0.join(cached);
        fs::write(&style, "cached style").unwrap();
        let key = QueryKey {
            base: scratch.0.clone(),
            environment: Vec::new(),
            arguments: vec![cached.as_os_str().to_os_string()],
        };
        CACHE
            .lock()
            .unwrap()
            .insert(key.clone(), Some(style.clone()), Instant::now());
        let mut resolver = StyleResolver {
            base: Some(scratch.0.clone()),
            environment: Vec::new(),
            remaining_queries: 0,
            deadline: Some(Instant::now()),
            queries: HashMap::new(),
        };
        assert_eq!(resolver.resolve_style(cached), Some(style));
        assert_eq!(resolver.resolve_style(skipped), None);
        let skipped_key = QueryKey {
            arguments: vec![skipped.as_os_str().to_os_string()],
            ..key
        };
        assert_eq!(
            CACHE
                .lock()
                .unwrap()
                .get(&skipped_key, Instant::now(), |_| panic!(
                    "uncached key queued"
                )),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn raw_queries_share_one_load_budget_and_retry_timeouts_on_the_next_load() {
        let scratch = Scratch::new();
        let arguments = vec![OsString::from("timeout-retry.sty")];
        let mut first = StyleResolver::new(&scratch.0);
        first.deadline = Some(Instant::now() + Duration::from_millis(30));
        assert_eq!(
            first.query_with(&arguments, |timeout| {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "exec sleep 2"]);
                run_bounded(command, timeout)
            }),
            Err(QueryError::TimedOut)
        );
        let mut next = StyleResolver::new(&scratch.0);
        assert_eq!(
            next.query_with(&arguments, |_| Ok(Some("retried.sty".into()))),
            Ok(Some("retried.sty".into()))
        );

        let missing = vec![OsString::from("stable-miss.sty")];
        assert_eq!(
            next.query_with(&missing, |timeout| {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "exit 1"]);
                run_bounded(command, timeout)
            }),
            Ok(None)
        );
        let spent = next.remaining_queries;
        assert_eq!(
            next.query_with(&missing, |_| panic!("memoized miss reran")),
            Ok(None)
        );
        assert_eq!(next.remaining_queries, spent);
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_output_status_and_runtime_are_bounded() {
        fn shell(script: &str) -> Command {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        }
        assert_eq!(
            run_bounded(shell("printf 'style.sty\\n'"), QUERY_TIMEOUT),
            Ok(Some("style.sty\n".into()))
        );
        assert_eq!(
            run_bounded(shell("printf 'style.sty'; exit 1"), QUERY_TIMEOUT),
            Ok(None)
        );
        assert_eq!(
            run_bounded(shell("exit 2"), QUERY_TIMEOUT),
            Err(QueryError::Failed)
        );
        assert_eq!(
            run_bounded(
                Command::new("/mathpreview-missing-kpsewhich"),
                QUERY_TIMEOUT
            ),
            Err(QueryError::Failed)
        );
        assert_eq!(
            run_bounded(shell("head -c 20000 /dev/zero"), QUERY_TIMEOUT),
            Err(QueryError::Failed)
        );
        let start = Instant::now();
        assert_eq!(
            run_bounded(shell("exec sleep 2"), Duration::from_millis(30)),
            Err(QueryError::TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
