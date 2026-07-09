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
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

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

/// Open the window and run its event loop. Blocks until the window is closed
/// (the event loop exits the process), so this never returns `Ok`.
pub fn run_window(url: &str, title: &str) -> Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 900.0))
        .build(&event_loop)
        .context("creating the preview window")?;
    let _webview = WebViewBuilder::new()
        .with_url(url)
        .build(&window)
        .context("creating the webview")?;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
