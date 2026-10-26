//! Optional, bounded discovery of custom styles in the user's TeX search path.
//!
//! Only locations are cached. The project loader still reads each discovered
//! style (or its live-buffer override) on every conversion.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_CAPACITY: usize = 256;
const QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const LOAD_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_QUERIES_PER_LOAD: usize = 32;
#[cfg(unix)]
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct QueryKey {
    base: PathBuf,
    environment: Vec<(OsString, OsString)>,
    arguments: Vec<OsString>,
}

struct CacheEntry {
    inserted: Instant,
    value: Option<String>,
}

#[derive(Default)]
struct QueryCache {
    entries: HashMap<QueryKey, CacheEntry>,
}

impl QueryCache {
    fn get(&self, key: &QueryKey, now: Instant) -> Option<Option<String>> {
        self.entries.get(key).and_then(|entry| {
            (now.saturating_duration_since(entry.inserted) < CACHE_TTL).then(|| entry.value.clone())
        })
    }

    fn insert(&mut self, key: QueryKey, value: Option<String>, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.inserted) < CACHE_TTL);
        if self.entries.len() >= CACHE_CAPACITY {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.inserted)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                inserted: now,
                value,
            },
        );
    }
}

static CACHE: LazyLock<Mutex<QueryCache>> = LazyLock::new(|| Mutex::new(QueryCache::default()));

/// One budget shared by all preamble files in a single project load. Cached
/// lookups remain available after the uncached-query budget is exhausted.
pub(crate) struct StyleResolver {
    base: Option<PathBuf>,
    environment: Vec<(OsString, OsString)>,
    remaining_queries: usize,
    deadline: Option<Instant>,
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
        }
    }

    pub(crate) fn resolve_style(&mut self, name: &Path) -> Option<PathBuf> {
        if !valid_style_name(name) {
            return None;
        }
        let base = self.base.clone()?;
        resolve_with(&base, name, |arguments| self.query(&base, arguments))
    }

    fn query(&mut self, base: &Path, arguments: &[OsString]) -> Option<String> {
        self.query_with(base, arguments, |timeout| {
            let mut command = Command::new("kpsewhich");
            command.current_dir(base).args(arguments);
            run_bounded(command, timeout)
        })
    }

    fn query_with(
        &mut self,
        base: &Path,
        arguments: &[OsString],
        run: impl FnOnce(Duration) -> Result<Option<String>, TimedOut>,
    ) -> Option<String> {
        let key = QueryKey {
            base: base.to_path_buf(),
            environment: self.environment.clone(),
            arguments: arguments.to_vec(),
        };
        if let Some(value) = CACHE
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key, Instant::now()))
        {
            return value;
        }
        let now = Instant::now();
        let deadline = *self.deadline.get_or_insert(now + LOAD_TIMEOUT);
        let timeout = deadline.saturating_duration_since(now).min(QUERY_TIMEOUT);
        if self.remaining_queries == 0 || timeout.is_zero() {
            // A budget miss is not a filesystem miss: the next project load
            // must be allowed to finish resolving this package.
            return None;
        }
        self.remaining_queries -= 1;
        // A transient timeout says nothing about whether the style exists.
        // Retry it on the next load instead of caching a miss for 30 seconds.
        let value = run(timeout).ok()?;
        if let Ok(mut cache) = CACHE.lock() {
            cache.insert(key, value.clone(), Instant::now());
        }
        value
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
    mut query: impl FnMut(&[OsString]) -> Option<String>,
) -> Option<PathBuf> {
    let output = query(&[
        "--format=tex".into(),
        "--no-mktex=tex".into(),
        "--".into(),
        name.as_os_str().to_os_string(),
    ])?;
    let path = single_path(&output, base)?.canonicalize().ok()?;
    if !path.is_file() {
        return None;
    }

    // Standard package internals must not override MathJax's implementations.
    // TEXMFHOME, TEXMFLOCAL, and ordinary TEXINPUTS directories are outside
    // these distribution roots. Both queries must succeed: knowing only one
    // root cannot prove a package is outside the other. A successfully queried
    // empty variable is unset, but at least one root must be identified.
    let mut roots = Vec::new();
    for variable in ["TEXMFDIST", "TEXMFMAIN"] {
        let output = query(&[format!("--var-value={variable}").into()])?;
        if output.trim().is_empty() {
            continue;
        }
        let root = single_path(&output, base)?.canonicalize().ok()?;
        if !root.is_dir() {
            return None;
        }
        roots.push(root);
    }
    if roots.is_empty() || roots.iter().any(|root| path.starts_with(root)) {
        return None;
    }
    Some(path)
}

#[derive(Debug, PartialEq, Eq)]
struct TimedOut;

/// Unix sockets let us bound reads without a blocking reader thread, even if
/// an unexpected executable keeps stdout open. The supported release targets
/// are macOS and Linux. Other platforms keep project-local discovery only.
#[cfg(unix)]
fn run_bounded(mut command: Command, timeout: Duration) -> Result<Option<String>, TimedOut> {
    use std::io::{ErrorKind, Read};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;

    let Ok((mut reader, writer)) = UnixStream::pair() else {
        return Ok(None);
    };
    if reader.set_nonblocking(true).is_err() {
        return Ok(None);
    }
    let writer: OwnedFd = writer.into();
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(writer));
    let Ok(mut child) = command.spawn() else {
        return Ok(None);
    };
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    let mut exited: Option<bool> = None;
    let result = 'poll: loop {
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if output.len() + count > MAX_OUTPUT_BYTES {
                        break 'poll Ok(None);
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break 'poll Ok(None),
            }
        }
        if let Some(status) = exited {
            break Ok(status.then(|| String::from_utf8(output).ok()).flatten());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain once more after observing exit: all bytes written by
                // this child are now available, with no wait for pipe EOF.
                exited = Some(status.success());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => break Err(TimedOut),
            Err(_) => break Ok(None),
        }
    };
    if exited.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(not(unix))]
fn run_bounded(_command: Command, _timeout: Duration) -> Result<Option<String>, TimedOut> {
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
                    Some(format!("{}\n", target.display()))
                } else {
                    Some(format!("{}\n", dist.display()))
                }
            })
        };
        assert_eq!(
            lookup(&custom.join("notes.sty")),
            Some(custom.join("notes.sty"))
        );
        assert_eq!(lookup(&dist.join("notes.sty")), None);
        fs::remove_file(custom.join("notes.sty")).unwrap();
        assert_eq!(lookup(&custom.join("notes.sty")), None);
    }

    #[test]
    fn lookup_rejects_unknown_roots_and_ambiguous_paths() {
        let scratch = Scratch::new();
        let style = scratch.0.join("notes.sty");
        fs::write(&style, "").unwrap();
        assert_eq!(
            resolve_with(&scratch.0, Path::new("notes.sty"), |args| {
                (args.len() == 4).then(|| style.display().to_string())
            }),
            None
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
                    Some(style.display().to_string())
                } else if args[0] == "--var-value=TEXMFDIST" {
                    Some(dist.display().to_string())
                } else {
                    main_output.clone()
                }
            })
        };
        // A timeout, query error, or exhausted load budget must not make the
        // package under the unclassified TEXMFMAIN appear to be custom.
        assert_eq!(lookup(None), None);
        assert_eq!(lookup(Some(main.display().to_string())), None);
        assert_eq!(lookup(Some("missing-root".into())), None);
        assert_eq!(lookup(Some("bad\nroot".into())), None);
        assert_eq!(lookup(Some("\n".into())), Some(style));
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
    fn query_cache_expires_misses_and_has_a_fixed_capacity() {
        let mut cache = QueryCache::default();
        let now = Instant::now();
        let key = QueryKey {
            base: PathBuf::from("/project"),
            environment: vec![("TEXINPUTS".into(), "/styles".into())],
            arguments: vec!["notes.sty".into()],
        };
        cache.insert(key.clone(), None, now);
        assert_eq!(cache.get(&key, now), Some(None));
        assert_eq!(cache.get(&key, now + CACHE_TTL), None);
        let mut other = key.clone();
        other.environment[0].1 = "/other".into();
        assert_eq!(cache.get(&other, now), None);
        for i in 0..=CACHE_CAPACITY {
            let mut next = key.clone();
            next.arguments = vec![format!("file{i}.sty").into()];
            cache.insert(next, Some("path".into()), now);
        }
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
    }

    #[test]
    fn exhausted_budget_still_uses_cache_and_does_not_cache_skipped_queries() {
        let scratch = Scratch::new();
        let cached = vec![OsString::from("cached")];
        let skipped = vec![OsString::from("skipped")];
        let key = QueryKey {
            base: scratch.0.clone(),
            environment: Vec::new(),
            arguments: cached.clone(),
        };
        CACHE
            .lock()
            .unwrap()
            .insert(key.clone(), Some("cached path".into()), Instant::now());
        let mut resolver = StyleResolver {
            base: Some(scratch.0.clone()),
            environment: Vec::new(),
            remaining_queries: 0,
            deadline: Some(Instant::now()),
        };
        assert_eq!(
            resolver.query(&scratch.0, &cached),
            Some("cached path".into())
        );
        assert_eq!(resolver.query(&scratch.0, &skipped), None);
        let skipped_key = QueryKey {
            arguments: skipped,
            ..key
        };
        assert_eq!(
            CACHE.lock().unwrap().get(&skipped_key, Instant::now()),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_queries_retry_next_load_but_stable_misses_are_cached() {
        let scratch = Scratch::new();
        let arguments = vec![OsString::from("timeout-retry.sty")];
        let mut first = StyleResolver::new(&scratch.0);
        first.deadline = Some(Instant::now() + Duration::from_millis(30));
        assert_eq!(
            first.query_with(&scratch.0, &arguments, |timeout| {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "exec sleep 2"]);
                run_bounded(command, timeout)
            }),
            None
        );
        let mut next = StyleResolver::new(&scratch.0);
        assert_eq!(
            next.query_with(&scratch.0, &arguments, |_| Ok(Some("retried.sty".into()))),
            Some("retried.sty".into())
        );

        let missing = vec![OsString::from("stable-miss.sty")];
        assert_eq!(
            next.query_with(&scratch.0, &missing, |timeout| {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "exit 1"]);
                run_bounded(command, timeout)
            }),
            None
        );
        let mut third = StyleResolver::new(&scratch.0);
        assert_eq!(
            third.query_with(&scratch.0, &missing, |_| panic!("cached miss reran")),
            None
        );
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
            run_bounded(
                Command::new("/mathpreview-missing-kpsewhich"),
                QUERY_TIMEOUT
            ),
            Ok(None)
        );
        assert_eq!(
            run_bounded(shell("head -c 20000 /dev/zero"), QUERY_TIMEOUT),
            Ok(None)
        );
        let start = Instant::now();
        assert_eq!(
            run_bounded(shell("exec sleep 2"), Duration::from_millis(30)),
            Err(TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
