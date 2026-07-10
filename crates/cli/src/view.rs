//! Native webview window (the `gui` feature). Opens the live preview in a
//! dedicated OS window — WebKit on macOS, WebView2 on Windows, WebKitGTK on
//! Linux — instead of a browser tab, loading the local daemon's page. It's the
//! exact same HTML/CSS/JS/MathJax the browser gets, so live-reload, search,
//! source-jump, and every other feature work unchanged; only the shell differs.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
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
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    set_dock_icon();
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
    let _webview = WebViewBuilder::new()
        .with_url(url)
        .with_ipc_handler(move |req| {
            if req.body() == "close" {
                let _ = proxy.send_event(UserEvent::CloseWindow);
            }
        })
        .build(&window)
        .context("creating the webview")?;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
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
