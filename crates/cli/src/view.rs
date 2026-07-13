//! Native webview window (the `gui` feature). Opens the live preview in a
//! dedicated OS window — WebKit on macOS, WebView2 on Windows, WebKitGTK on
//! Linux — instead of a browser tab, loading the local daemon's page. It's the
//! exact same HTML/CSS/JS/MathJax the browser gets, so live-reload, search,
//! source-jump, and every other feature work unchanged; only the shell differs.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Events the webview's JS can send to the window's event loop.
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    /// The page asked to close the window (the viewer's `:q` command).
    CloseWindow,
}

/// Pick a free loopback port for a standalone `view` daemon, so it never
/// clashes with an nvim-managed daemon on the default port.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(23636)
}

/// Block until the daemon accepts connections on `port` (or the deadline
/// passes — the window then loads and shows whatever state the server reached).
pub fn wait_for_listen(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Product name for the native window (the app layer — the CLI and the nvim
/// plugin keep their own names). Shown in the window title and, eventually, a
/// macOS `.app` bundle.
pub const APP_NAME: &str = "Locus";

/// Brand the dock. A bare binary (not an `.app` bundle) shows the generic
/// executable icon, so set `NSApp.applicationIconImage` at runtime from the
/// embedded PNG (rendered from `assets/locus-icon.svg`; `assets/Locus.icns`
/// is the same art for an eventual bundle). Main thread only, after the event
/// loop's build has initialized NSApplication.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../assets/locus-icon-1024.png"));
    if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
    }
}

/// Window/taskbar icon for platforms that take one per-window (X11, Windows;
/// Wayland uses the .desktop entry instead). The blob is straight RGBA8 baked
/// from `assets/locus-icon.svg` at 128×128 — raw bytes so the gui feature
/// doesn't need a PNG decoder.
#[cfg(not(target_os = "macos"))]
fn window_icon() -> Option<tao::window::Icon> {
    let rgba = include_bytes!("../assets/locus-icon-128.rgba").to_vec();
    tao::window::Icon::from_rgba(rgba, 128, 128).ok()
}

/// Native "choose a .tex file" dialog, for launching Locus with no argument —
/// double-clicking the dock-pinned `Locus.app` passes no argv, and erroring
/// out would make the pinned icon useless. Returns `None` if the user cancels.
/// Must run on the main thread, before the event loop takes it over.
#[cfg(target_os = "macos")]
pub fn pick_tex_file() -> Option<std::path::PathBuf> {
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{ns_string, MainThreadMarker, NSArray};
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setTitle(Some(ns_string!("Open a LaTeX document")));
    panel.setMessage(Some(ns_string!("Choose the .tex file to preview")));
    // `allowedContentTypes` needs the UTType crate; the deprecated
    // extension-based filter does the same job for one extension.
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[ns_string!("tex")])));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let path = panel.URL()?.path()?;
    Some(std::path::PathBuf::from(path.to_string()))
}

/// Open the window and run its event loop. Blocks until the window is closed
/// (the event loop exits the process), so this never returns `Ok`. `doc` is the
/// document label (usually the file name); the title is branded as
/// `"<doc> — Locus"`, or just "Locus" when `doc` is empty.
pub fn run_window(url: &str, doc: &str) -> Result<()> {
    let title = if doc.is_empty() {
        APP_NAME.to_string()
    } else {
        format!("{doc} — {APP_NAME}")
    };
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut loop_builder = EventLoopBuilder::<UserEvent>::with_user_event();
    // Pin the Wayland app_id (and the GApplication id) so the compositor can
    // match the window to the desktop entry `io.github.sonv.locus.desktop` —
    // Wayland has no per-window icons; the icon comes from that association
    // (scripts/install-locus-desktop.sh installs it). Same id as the macOS
    // bundle. Without this the app_id would be whichever binary spawned the
    // window (locus vs mathpreview-cli).
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        loop_builder.with_app_id("io.github.sonv.locus");
    }
    let event_loop = loop_builder.build();
    let builder = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(LogicalSize::new(1100.0, 900.0));
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_window_icon(window_icon());
    let window = builder
        .build(&event_loop)
        .context("creating the preview window")?;
    // `window.ipc.postMessage('close')` from the page closes the window — this
    // is what the viewer's `:q` command uses. wry only injects `window.ipc`
    // when a handler is installed, so the same JS detects "am I in Locus?" by
    // its presence and falls back to `window.close()` in a browser tab.
    let proxy = event_loop.create_proxy();
    let builder = WebViewBuilder::new()
        .with_url(url)
        .with_ipc_handler(move |req| {
            if req.body() == "close" {
                let _ = proxy.send_event(UserEvent::CloseWindow);
            }
        });
    // WKWebView alone double-counts CSS `zoom` when MathJax's outer SVG uses
    // ex-sized dimensions. Mark only the macOS native shell so mathjax.css can
    // apply its responsive workaround without changing browser/WebKitGTK.
    #[cfg(target_os = "macos")]
    let builder = builder
        .with_initialization_script("document.documentElement.classList.add('locus-macos');");
    #[cfg(not(target_os = "linux"))]
    let _webview = builder.build(&window).context("creating the webview")?;
    // On Linux, wry's raw-window-handle path supports only X11 (Xlib) handles;
    // under Wayland (the GNOME default on Debian/Fedora/Ubuntu) it fails with
    // "the window handle kind is not supported". Build through the window's
    // GTK widget instead — the pattern wry's own examples use — which works on
    // both X11 and Wayland.
    #[cfg(target_os = "linux")]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .context("tao window has no default GTK vbox")?;
        builder.build_gtk(vbox).context("creating the webview")?
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // Set the dock icon on the FIRST event, not before `run`: tao
            // finishes launching NSApplication (and materializes the dock
            // tile) inside `run`, and an applicationIconImage set before that
            // is dropped with the generic executable icon shown instead.
            Event::NewEvents(StartCause::Init) => {
                #[cfg(target_os = "macos")]
                set_dock_icon();
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(UserEvent::CloseWindow) => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
