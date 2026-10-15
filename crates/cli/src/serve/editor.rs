//! Atomic editor focus snapshots. A preamble-only file changes MathJax's
//! page-wide configuration, so hold the last good preview until the editor
//! returns to a document body. No editor handshake means normal CLI watching.

use super::*;
use std::collections::VecDeque;

const MAX_BUFFERS: usize = 256;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const RETIRED_SESSIONS: usize = 32;

#[derive(Default)]
pub(super) struct EditorState {
    session: Option<String>,
    retired: VecDeque<String>,
    focus_sequence: u64,
    file_sequences: HashMap<PathBuf, u64>,
    /// Remember the role of retained unsaved sources after an include is
    /// removed, so re-adding that include cannot bypass the mid-edit guard.
    source_paths: HashSet<PathBuf>,
    pub(super) cursors: HashMap<PathBuf, (usize, usize)>,
    pub(super) paused: bool,
    pub(super) pending: bool,
}

impl EditorState {
    fn select_session(&mut self, session: &str) -> bool {
        if self.session.as_deref() == Some(session) {
            return true;
        }
        if self.retired.iter().any(|old| old == session) {
            return false;
        }
        if let Some(old) = self.session.replace(session.to_string()) {
            self.retired.push_back(old);
            if self.retired.len() > RETIRED_SESSIONS {
                self.retired.pop_front();
            }
        }
        self.focus_sequence = 0;
        self.file_sequences.clear();
        self.cursors.clear();
        true
    }

    pub(super) fn accept_buffer(&mut self, version: Option<(&str, u64)>, path: &Path) -> bool {
        let Some((session, sequence)) = version else {
            // Old editors and direct /buffer callers keep their existing API.
            return true;
        };
        if self.session.is_none() {
            self.select_session(session);
        }
        if self.session.as_deref() != Some(session)
            || sequence <= self.focus_sequence
            || self
                .file_sequences
                .get(path)
                .is_some_and(|old| sequence <= *old)
        {
            return false;
        }
        self.file_sequences.insert(path.to_path_buf(), sequence);
        true
    }
}

#[derive(Deserialize)]
pub(super) struct EditorRequest {
    session: String,
    sequence: u64,
    file: String,
    #[serde(default)]
    buffers: Vec<EditorBuffer>,
}

#[derive(Deserialize)]
struct EditorBuffer {
    file: String,
    source: String,
    #[serde(default)]
    cursor: Option<[usize; 2]>,
}

fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn buffer_version(
    headers: &axum::http::HeaderMap,
) -> Result<Option<(&str, u64)>, StatusCode> {
    let session = headers.get("x-mathpreview-editor-session");
    let sequence = headers.get("x-mathpreview-editor-sequence");
    match (session, sequence) {
        (None, None) => Ok(None),
        (Some(session), Some(sequence)) => {
            let session = session.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
            let sequence = sequence
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or(StatusCode::BAD_REQUEST)?;
            if !valid_session(session) {
                return Err(StatusCode::BAD_REQUEST);
            }
            Ok(Some((session, sequence)))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

/// Roles are explicit converter metadata, never guessed from names or a lack
/// of rendered anchors (empty chapters and comment-only includes are body).
pub(super) fn file_roles(current: &LiveSnapshot) -> (HashSet<PathBuf>, HashSet<PathBuf>) {
    let root = project::normalize_source_path(&current.document.root_file);
    let mut preamble = HashSet::new();
    let mut body = HashSet::from([root.clone()]);
    if current.format() == DocumentFormat::Latex {
        for dependency in &current.document.dependencies {
            let path = project::normalize_source_path(&dependency.path);
            match dependency.kind.as_str() {
                "preamble" => {
                    preamble.insert(path);
                }
                "body" => {
                    body.insert(path);
                }
                _ => {}
            }
        }
    }
    preamble.retain(|path| !body.contains(path));
    (preamble, body)
}

pub(super) async fn defer_render(state: &AppState) -> bool {
    let mut editor = state.editor_state.lock().await;
    if editor.paused {
        editor.pending = true;
        return true;
    }
    let current = state.current.read().await;
    if current.format() == DocumentFormat::Markdown {
        return false;
    }
    let (preamble, body) = file_roles(&current);
    let overrides = state.buffer_overrides.read().await;
    editor
        .source_paths
        .retain(|path| overrides.contains_key(path));
    editor.source_paths.extend(
        overrides
            .keys()
            .filter(|path| preamble.contains(*path) || body.contains(*path))
            .cloned(),
    );
    let mut context = current.viewer.preamble.raw_preamble.clone();
    // Newly declared literal environments in an unsaved preamble must also
    // protect dollar signs in the corresponding unsaved body buffers.
    for (path, source) in overrides.iter() {
        if preamble.contains(path) || (overrides.len() > 1 && path == &current.document.root_file) {
            context.push('\n');
            context.push_str(source);
        }
    }
    let incomplete_sources: Vec<_> = overrides
        .iter()
        .filter(|(path, source)| {
            editor.source_paths.contains(*path)
                && !is_buffer_renderable_with_preamble(
                    source,
                    editor.cursors.get(*path).copied(),
                    &context,
                )
        })
        .collect();
    let incomplete = if incomplete_sources.is_empty() {
        false
    } else if incomplete_sources
        .iter()
        .any(|(path, _)| **path == current.document.root_file)
    {
        true
    } else {
        // A valid root/body edit can remove an incomplete include. Current
        // preview roles still contain that old path, so consult the candidate
        // graph before freezing it. This extra load is only on the exceptional
        // incomplete-source path, never on ordinary valid keystrokes.
        match project::load_project_with_overrides(&current.document.root_file, &overrides) {
            Ok(candidate) => {
                let reachable: HashSet<_> = candidate
                    .files
                    .iter()
                    .map(|file| &file.path)
                    .chain(candidate.preamble_files.iter().map(|file| &file.path))
                    .collect();
                let mut context = candidate.preamble.source.clone();
                for file in &candidate.preamble_files {
                    context.push('\n');
                    context.push_str(&file.source);
                }
                incomplete_sources.iter().any(|(path, source)| {
                    reachable.contains(path)
                        && !is_buffer_renderable_with_preamble(
                            source,
                            editor.cursors.get(*path).copied(),
                            &context,
                        )
                })
            }
            Err(_) => true,
        }
    };
    if incomplete {
        editor.pending = true;
    }
    incomplete
}

pub(super) async fn serve_editor_state(
    State(state): State<AppState>,
    Json(request): Json<EditorRequest>,
) -> StatusCode {
    if !valid_session(&request.session) || request.sequence == 0 || request.file.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    if request.buffers.len() > MAX_BUFFERS
        || request
            .buffers
            .iter()
            .map(|buffer| buffer.source.len())
            .sum::<usize>()
            > MAX_SNAPSHOT_BYTES
    {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let (root, format, preamble, body) = {
        let current = state.current.read().await;
        let (preamble, body) = file_roles(&current);
        (
            current.document.root_file.clone(),
            current.format(),
            preamble,
            body,
        )
    };
    let active = project::normalize_source_path(Path::new(&request.file));
    // An unrelated file cannot release an existing hold or take over its
    // editor session. Only the root and explicit document source roles can.
    if !preamble.contains(&active) && !body.contains(&active) {
        return StatusCode::BAD_REQUEST;
    }
    let known = state.watched.read().await.clone();
    let normalized_root = project::normalize_source_path(&root);
    let mut buffers = Vec::with_capacity(request.buffers.len());
    let mut seen = HashSet::new();
    for buffer in request.buffers {
        let path = project::normalize_source_path(Path::new(&buffer.file));
        if buffer.file.is_empty()
            || buffer
                .cursor
                .is_some_and(|[line, col]| line == 0 || col == 0)
            || (path != normalized_root
                && (format != DocumentFormat::Latex || !known.contains(&path)))
            || !seen.insert(path.clone())
        {
            return StatusCode::BAD_REQUEST;
        }
        buffers.push((
            if path == normalized_root {
                root.clone()
            } else {
                path
            },
            buffer.source,
            buffer.cursor.map(|[line, col]| (line, col)),
        ));
    }
    let present: HashSet<_> = buffers.iter().map(|(path, _, _)| path.clone()).collect();
    touch_editor_contact(&state);
    let seq = {
        let mut editor = state.editor_state.lock().await;
        if !editor.select_session(&request.session) || request.sequence <= editor.focus_sequence {
            return StatusCode::NO_CONTENT;
        }
        editor.focus_sequence = request.sequence;
        let was_paused = editor.paused;
        editor.paused = preamble.contains(&active);
        // Advancing the render generation invalidates pre-focus work even
        // when this transition only enters a hold and starts no new render.
        let seq = begin_render_attempt(&state);
        let mut overrides = state.buffer_overrides.write().await;
        let mut push_seq = state.buffer_push_seq.write().await;
        // The focus batch is an authoritative editor snapshot. Omitted files
        // have been saved, unloaded, or explicitly discarded; retaining their
        // old text would resurrect abandoned edits on the next render. A
        // newer in-flight per-file push still wins over this older snapshot.
        overrides.retain(|path, _| {
            let keep = present.contains(path)
                || editor
                    .file_sequences
                    .get(path)
                    .is_some_and(|old| *old > request.sequence);
            if !keep {
                editor.pending = true;
                push_seq.remove(path);
                editor.cursors.remove(path);
            }
            keep
        });
        for (path, source, cursor) in buffers {
            if editor
                .file_sequences
                .get(&path)
                .is_some_and(|old| *old > request.sequence)
            {
                continue;
            }
            editor.file_sequences.insert(path.clone(), request.sequence);
            if let Some(cursor) = cursor {
                editor.cursors.insert(path.clone(), cursor);
            } else {
                editor.cursors.remove(&path);
            }
            if overrides.get(&path) != Some(&source) {
                editor.pending = true;
            }
            overrides.insert(path.clone(), source);
            push_seq.insert(path, seq);
        }
        if editor.paused {
            return StatusCode::ACCEPTED;
        }
        if !was_paused && !editor.pending {
            return StatusCode::NO_CONTENT;
        }
        // The pending flag is cleared only by a successful snapshot commit.
        seq
    };
    let _permit = state.render_permit.lock().await;
    if !is_latest_render_attempt(&state, seq) {
        return StatusCode::NO_CONTENT;
    }
    if defer_render(&state).await {
        return StatusCode::ACCEPTED;
    }
    match render_cached(&state, &root).await {
        Ok((out, _)) => {
            if is_latest_render_attempt(&state, seq) {
                update_watched(&state, &out).await;
                broadcast_render(&state, out, seq).await;
            }
            StatusCode::NO_CONTENT
        }
        Err(error) => {
            if is_latest_render_attempt(&state, seq) && !defer_render(&state).await {
                log_event(&state, "error", format!("editor-state render: {error:#}"));
                let _ = state.tx.send(
                    serde_json::json!({
                        "event": "error", "message": error.to_string(),
                    })
                    .to_string(),
                );
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::NO_CONTENT
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        state: AppState,
        dir: PathBuf,
        root: PathBuf,
        preamble: PathBuf,
        chapter: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn fixture() -> Fixture {
        let nonce = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview-editor-state-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let root = dir.join("main.tex");
        let preamble = dir.join("preamble.tex");
        let chapter = dir.join("chapter.tex");
        std::fs::write(&root, "\\documentclass{article}\n\\input{preamble}\n\\begin{document}\n$\\foo$\n\\input{chapter}\n\\end{document}\n").unwrap();
        std::fs::write(&preamble, r"\newcommand{\foo}{OLD}").unwrap();
        std::fs::write(&chapter, "Chapterdisk.").unwrap();
        let output = mathpreview_core::render_project(&root, &HtmlOptions::default()).unwrap();
        let state = super::super::tests::app_state_for_output(output);
        let (snapshot, _) = render_cached(&state, &root).await.unwrap();
        update_watched(&state, &snapshot).await;
        *state.last_blocks.write().await = snapshot.document.blocks.clone();
        *state.current.write().await = snapshot;
        Fixture {
            state,
            dir,
            root,
            preamble,
            chapter,
        }
    }

    async fn focus(
        f: &Fixture,
        session: &str,
        sequence: u64,
        path: &Path,
        buffers: &[(&Path, &str)],
    ) -> StatusCode {
        serve_editor_state(
            State(f.state.clone()),
            Json(EditorRequest {
                session: session.to_string(),
                sequence,
                file: path.to_string_lossy().into_owned(),
                buffers: buffers
                    .iter()
                    .map(|(path, source)| EditorBuffer {
                        file: path.to_string_lossy().into_owned(),
                        source: (*source).to_string(),
                        cursor: None,
                    })
                    .collect(),
            }),
        )
        .await
    }

    async fn macro_body(f: &Fixture) -> String {
        f.state
            .current
            .read()
            .await
            .viewer
            .preamble
            .macros
            .iter()
            .find(|mac| mac.name == "foo")
            .unwrap()
            .body
            .clone()
    }

    fn version_headers(path: &Path, sequence: u64) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-mathpreview-path",
            path.to_str().unwrap().parse().unwrap(),
        );
        headers.insert(
            "x-mathpreview-editor-session",
            "editor-one".parse().unwrap(),
        );
        headers.insert(
            "x-mathpreview-editor-sequence",
            sequence.to_string().parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn preamble_focus_holds_saves_and_unsaved_resume_sends_one_reload() {
        let f = fixture().await;
        let mut receiver = f.state.tx.subscribe();
        let stale = begin_render_attempt(&f.state);
        assert_eq!(
            focus(
                &f,
                "editor-one",
                1,
                &f.preamble,
                &[(&f.preamble, r"\newcommand{\foo}{INTERMEDIATE}")]
            )
            .await,
            StatusCode::ACCEPTED
        );
        assert!(!is_latest_render_attempt(&f.state, stale));
        assert_eq!(macro_body(&f).await, "OLD");
        std::fs::write(&f.preamble, r"\newcommand{\foo}{SAVED}").unwrap();
        prepare_file_changes(&f.state, &HashSet::from([f.preamble.clone()])).await;
        assert!(defer_render(&f.state).await);
        trigger_rerender(&f.state, &f.root).await.unwrap();
        assert_eq!(macro_body(&f).await, "OLD");
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            focus(
                &f,
                "editor-one",
                2,
                &f.chapter,
                &[
                    (&f.preamble, r"\newcommand{\foo}{FINAL}"),
                    (&f.chapter, "Chapterunsaved.")
                ]
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(macro_body(&f).await, "FINAL");
        let message: serde_json::Value =
            serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(message["event"], "full-reload");
        assert!(receiver.try_recv().is_err());
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Chapterunsaved"));
        assert!(!f.state.editor_state.lock().await.paused);
        assert!(!f.state.editor_state.lock().await.pending);
        assert_eq!(
            std::fs::read_to_string(&f.preamble).unwrap(),
            r"\newcommand{\foo}{SAVED}"
        );
    }

    #[tokio::test]
    async fn stale_editor_requests_cannot_overwrite_or_reactivate_a_hold() {
        let f = fixture().await;
        focus(&f, "editor-one", 10, &f.preamble, &[]).await;
        focus(
            &f,
            "editor-one",
            20,
            &f.root,
            &[(&f.preamble, r"\newcommand{\foo}{FINAL}")],
        )
        .await;
        assert_eq!(
            focus(
                &f,
                "editor-one",
                15,
                &f.preamble,
                &[(&f.preamble, r"\newcommand{\foo}{STALE}")]
            )
            .await,
            StatusCode::NO_CONTENT
        );
        let status = serve_buffer_push(
            State(f.state.clone()),
            version_headers(&f.preamble, 18),
            r"\newcommand{\foo}{STALE}".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(macro_body(&f).await, "FINAL");
        assert!(!f.state.editor_state.lock().await.paused);
        focus(&f, "editor-two", 1, &f.root, &[]).await;
        focus(&f, "editor-one", 100, &f.preamble, &[]).await;
        assert_eq!(
            f.state.editor_state.lock().await.session.as_deref(),
            Some("editor-two")
        );
        assert!(!f.state.editor_state.lock().await.paused);
    }

    #[tokio::test]
    async fn snapshots_preserve_newer_per_file_pushes_and_discard_omitted_edits() {
        let f = fixture().await;
        focus(&f, "editor-one", 1, &f.preamble, &[]).await;
        let status = serve_buffer_push(
            State(f.state.clone()),
            version_headers(&f.chapter, 30),
            "Newestchapter.".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        focus(
            &f,
            "editor-one",
            20,
            &f.root,
            &[
                (&f.chapter, "Oldchapter."),
                (&f.preamble, r"\newcommand{\foo}{TEMP}"),
            ],
        )
        .await;
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Newestchapter"));
        assert_eq!(macro_body(&f).await, "TEMP");
        focus(&f, "editor-one", 40, &f.root, &[]).await;
        assert_eq!(macro_body(&f).await, "OLD");
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Chapterdisk"));
        assert!(f.state.buffer_overrides.read().await.is_empty());
    }

    #[tokio::test]
    async fn invalid_snapshot_has_no_partial_mutation_or_focus_authority() {
        let f = fixture().await;
        focus(&f, "editor-one", 1, &f.preamble, &[]).await;
        let outsider = f.dir.join("unrelated.tex");
        assert_eq!(
            focus(&f, "bad-editor", 1, &outsider, &[]).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            focus(
                &f,
                "bad-editor",
                1,
                &f.root,
                &[(&f.preamble, "BAD"), (&outsider, "OUTSIDE")]
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert!(f.state.buffer_overrides.read().await.is_empty());
        let editor = f.state.editor_state.lock().await;
        assert_eq!(editor.session.as_deref(), Some("editor-one"));
        assert!(editor.paused);
    }

    #[tokio::test]
    async fn paused_broadcast_cannot_commit_even_with_a_newer_render_generation() {
        let f = fixture().await;
        let (out, _) = render_cached(&f.state, &f.root).await.unwrap();
        focus(&f, "editor-one", 1, &f.preamble, &[]).await;
        let mut receiver = f.state.tx.subscribe();
        let seq = begin_render_attempt(&f.state);
        assert_eq!(broadcast_render(&f.state, out, seq).await, (0, "deferred"));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn cli_only_watchers_and_ordinary_body_edits_remain_live() {
        let f = fixture().await;
        let mut receiver = f.state.tx.subscribe();
        std::fs::write(&f.preamble, r"\newcommand{\foo}{DISK}").unwrap();
        let seq = prepare_file_changes(&f.state, &HashSet::from([f.preamble.clone()])).await;
        assert!(!defer_render(&f.state).await);
        let (out, _) = render_cached(&f.state, &f.root).await.unwrap();
        broadcast_render(&f.state, out, seq).await;
        assert_eq!(macro_body(&f).await, "DISK");
        receiver.try_recv().unwrap();
        focus(
            &f,
            "editor-one",
            1,
            &f.chapter,
            &[(&f.chapter, "Chapterlive.")],
        )
        .await;
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Chapterlive"));
        assert!(!f.state.editor_state.lock().await.paused);
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn client_versions_are_per_file_after_the_latest_focus_barrier() {
        let mut state = EditorState::default();
        state.select_session("editor-one");
        state.focus_sequence = 3;
        assert!(state.accept_buffer(Some(("editor-one", 9)), Path::new("a.tex")));
        assert!(state.accept_buffer(Some(("editor-one", 8)), Path::new("b.tex")));
        assert!(!state.accept_buffer(Some(("editor-one", 8)), Path::new("a.tex")));
        assert!(!state.accept_buffer(Some(("editor-one", 3)), Path::new("c.tex")));
        assert!(!state.accept_buffer(Some(("old-editor", 100)), Path::new("c.tex")));
    }

    #[tokio::test]
    async fn incomplete_inactive_buffer_blocks_focus_body_push_and_saved_source() {
        let f = fixture().await;
        let initial = f.state.current.read().await.viewer.html.clone();
        let mut receiver = f.state.tx.subscribe();
        assert_eq!(
            focus(
                &f,
                "editor-one",
                1,
                &f.root,
                &[(&f.chapter, "Incomplete $x")]
            )
            .await,
            StatusCode::ACCEPTED,
        );
        assert_eq!(
            f.state
                .buffer_overrides
                .read()
                .await
                .get(&f.chapter)
                .unwrap(),
            "Incomplete $x"
        );
        assert_eq!(f.state.current.read().await.viewer.html, initial);
        let root_source = std::fs::read_to_string(&f.root).unwrap();
        serve_buffer_push(
            State(f.state.clone()),
            version_headers(&f.root, 2),
            root_source,
        )
        .await;
        assert_eq!(f.state.current.read().await.viewer.html, initial);
        std::fs::write(&f.chapter, "Incomplete $x").unwrap();
        prepare_file_changes(&f.state, &HashSet::from([f.chapter.clone()])).await;
        assert!(f
            .state
            .buffer_overrides
            .read()
            .await
            .contains_key(&f.chapter));
        assert!(defer_render(&f.state).await);
        trigger_rerender(&f.state, &f.root).await.unwrap();
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            serve_buffer_push(
                State(f.state.clone()),
                version_headers(&f.chapter, 3),
                "Completed $x$.".to_string()
            )
            .await,
            StatusCode::NO_CONTENT,
        );
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Completed"));
        receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn late_preamble_upload_holds_even_before_editor_role_refresh() {
        let f = fixture().await;
        let mut receiver = f.state.tx.subscribe();
        assert_eq!(
            serve_buffer_push(
                State(f.state.clone()),
                version_headers(&f.preamble, 1),
                r"\newcommand{\foo}{PENDING}".to_string()
            )
            .await,
            StatusCode::ACCEPTED,
        );
        assert!(f.state.editor_state.lock().await.paused);
        assert_eq!(macro_body(&f).await, "OLD");
        assert!(receiver.try_recv().is_err());
        focus(
            &f,
            "editor-one",
            2,
            &f.root,
            &[(&f.preamble, r"\newcommand{\foo}{FINAL}")],
        )
        .await;
        assert_eq!(macro_body(&f).await, "FINAL");
        receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn focus_snapshot_preserves_transient_def_cursor_guard() {
        let f = fixture().await;
        let mut receiver = f.state.tx.subscribe();
        let source = "\\begin{equation}\na \\def\n\\end{equation}";
        let status = serve_editor_state(
            State(f.state.clone()),
            Json(EditorRequest {
                session: "editor-one".to_string(),
                sequence: 1,
                file: f.root.to_string_lossy().into_owned(),
                buffers: vec![EditorBuffer {
                    file: f.chapter.to_string_lossy().into_owned(),
                    source: source.to_string(),
                    cursor: Some([2, 6]),
                }],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(receiver.try_recv().is_err());
        assert!(defer_render(&f.state).await);
        assert_eq!(
            focus(
                &f,
                "editor-one",
                2,
                &f.root,
                &[(&f.chapter, "Repaired $a=b$.")]
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert!(receiver.try_recv().is_ok());
    }

    #[tokio::test]
    async fn snapshot_limits_and_versions_are_checked_before_mutation() {
        let f = fixture().await;
        let request = EditorRequest {
            session: "editor-one".to_string(),
            sequence: 1,
            file: f.root.to_string_lossy().into_owned(),
            buffers: (0..=MAX_BUFFERS)
                .map(|_| EditorBuffer {
                    file: f.preamble.to_string_lossy().into_owned(),
                    source: String::new(),
                    cursor: None,
                })
                .collect(),
        };
        assert_eq!(
            serve_editor_state(State(f.state.clone()), Json(request)).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            focus(&f, "editor-one", 0, &f.root, &[]).await,
            StatusCode::BAD_REQUEST
        );
        let mut headers = version_headers(&f.root, 1);
        headers.remove("x-mathpreview-editor-sequence");
        assert_eq!(
            serve_buffer_push(State(f.state.clone()), headers, String::new()).await,
            StatusCode::BAD_REQUEST
        );
        assert!(f.state.editor_state.lock().await.session.is_none());
        assert!(f.state.buffer_overrides.read().await.is_empty());
    }

    #[tokio::test]
    async fn removing_incomplete_includes_unblocks_without_discarding_their_buffers() {
        let f = fixture().await;
        let mut receiver = f.state.tx.subscribe();
        let invalid = "Unfinished $x";
        assert_eq!(
            focus(
                &f,
                "editor-one",
                1,
                &f.root,
                &[(&f.chapter, invalid), (&f.preamble, invalid)]
            )
            .await,
            StatusCode::ACCEPTED
        );
        assert!(receiver.try_recv().is_err());
        let source =
            "\\documentclass{article}\n\\begin{document}\nRemovedinputs.\n\\end{document}\n";
        assert_eq!(
            serve_buffer_push(
                State(f.state.clone()),
                version_headers(&f.root, 2),
                source.to_string()
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Removedinputs"));
        assert!(receiver.try_recv().is_ok());
        let overrides = f.state.buffer_overrides.read().await;
        assert_eq!(overrides.get(&f.chapter).unwrap(), invalid);
        assert_eq!(overrides.get(&f.preamble).unwrap(), invalid);
        drop(overrides);
        let source =
            "\\documentclass{article}\n\\begin{document}\n\\input{chapter}\n\\end{document}\n";
        assert_eq!(
            serve_buffer_push(
                State(f.state.clone()),
                version_headers(&f.root, 3),
                source.to_string()
            )
            .await,
            StatusCode::ACCEPTED
        );
        assert!(receiver.try_recv().is_err());
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Removedinputs"));
        // The removed child is not currently watched, so a complete focus
        // snapshot can drop its abandoned override and read its valid disk.
        assert_eq!(
            focus(&f, "editor-one", 4, &f.root, &[(&f.root, source)]).await,
            StatusCode::NO_CONTENT
        );
        assert!(f
            .state
            .current
            .read()
            .await
            .document
            .body_html
            .contains("Chapterdisk"));
    }
}
