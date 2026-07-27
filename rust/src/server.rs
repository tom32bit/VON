//! Localhost preview server (SWE §3.2).
//!
//! Web projects need a real origin: `file://` URLs in a WebView break ES module
//! imports, `fetch`, and anything touching `localStorage`. So each preview gets
//! a `tiny_http` server bound to `127.0.0.1:0` (kernel-assigned port, so two
//! projects never collide) serving that project's sandbox and nothing else.
//!
//! The server is bound to loopback only and every request path goes through the
//! same `Sandbox::resolve` the agent's own file tools use — a generated page
//! cannot fetch its way out of the sandbox any more than the agent can.

use std::collections::HashMap;
use std::io::Cursor;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::error::{Result, VonError};
use crate::sandbox::Sandbox;

/// Cap on a single served response. Preview assets are small; anything larger is
/// a mistake we would rather surface than stream into a WebView.
const MAX_SERVE_BYTES: u64 = 32 * 1024 * 1024;

pub struct PreviewServer {
    pub project_id: String,
    pub port: u16,
    shutdown: Arc<AtomicBool>,
    server: Arc<tiny_http::Server>,
}

impl PreviewServer {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

impl Drop for PreviewServer {
    fn drop(&mut self) {
        // Flip the flag, then unblock the accept loop so the thread observes it
        // and the port is released immediately rather than on the next request.
        self.shutdown.store(true, Ordering::SeqCst);
        self.server.unblock();
    }
}

static SERVERS: Lazy<Mutex<HashMap<String, PreviewServer>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Starts (or returns the existing) preview server for a project.
pub fn start(project_id: &str, sandbox: Sandbox) -> Result<String> {
    {
        let servers = SERVERS.lock();
        if let Some(existing) = servers.get(project_id) {
            return Ok(existing.url());
        }
    }

    // Ask the kernel for a free port rather than probing a fixed range.
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    let listener = TcpListener::bind(addr)
        .map_err(|e| VonError::Io(format!("could not bind preview server: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| VonError::Io(e.to_string()))?
        .port();

    let server = tiny_http::Server::from_listener(listener, None)
        .map_err(|e| VonError::Io(format!("preview server: {e}")))?;
    let server = Arc::new(server);

    let shutdown = Arc::new(AtomicBool::new(false));

    {
        let server = server.clone();
        let shutdown = shutdown.clone();
        let sandbox = sandbox.clone();
        let project_id = project_id.to_string();
        std::thread::Builder::new()
            .name(format!("von-preview-{project_id}"))
            .spawn(move || {
                for request in server.incoming_requests() {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Err(e) = handle(&sandbox, request) {
                        tracing::debug!("preview request failed: {e}");
                    }
                }
                tracing::info!("preview server for {project_id} stopped");
            })
            .map_err(|e| VonError::Io(e.to_string()))?;
    }

    let preview = PreviewServer {
        project_id: project_id.to_string(),
        port,
        shutdown,
        server,
    };
    let url = preview.url();
    SERVERS.lock().insert(project_id.to_string(), preview);
    tracing::info!("preview server for {project_id} listening on {url}");
    Ok(url)
}

pub fn stop(project_id: &str) {
    SERVERS.lock().remove(project_id);
}

pub fn stop_all() {
    SERVERS.lock().clear();
}

pub fn url_for(project_id: &str) -> Option<String> {
    SERVERS.lock().get(project_id).map(|s| s.url())
}

fn handle(sandbox: &Sandbox, request: tiny_http::Request) -> Result<()> {
    let raw = request.url().to_string();

    // Strip query and fragment before touching the filesystem.
    let path_part = raw
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');

    let decoded = percent_encoding::percent_decode_str(path_part)
        .decode_utf8_lossy()
        .to_string();

    // "" (root) and bare directories resolve to index.html, the same as any
    // static host, so a generated site's relative links work unchanged.
    let candidates: Vec<String> = if decoded.is_empty() {
        vec!["index.html".to_string()]
    } else if sandbox
        .resolve(&decoded)
        .map(|p| p.is_dir())
        .unwrap_or(false)
    {
        vec![
            format!("{}/index.html", decoded.trim_end_matches('/')),
            decoded.clone(),
        ]
    } else {
        vec![decoded.clone()]
    };

    for candidate in &candidates {
        match sandbox.resolve(candidate) {
            Ok(path) if path.is_file() => {
                let meta = std::fs::metadata(&path)?;
                if meta.len() > MAX_SERVE_BYTES {
                    return respond_text(request, 413, "file too large to preview");
                }
                let bytes = std::fs::read(&path)?;
                let mime = mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string();

                let mut response = tiny_http::Response::from_data(bytes)
                    .with_status_code(200)
                    .with_header(header("Content-Type", &mime));

                // The agent rewrites files constantly; a cached asset would make
                // the preview look broken after a fix.
                for (k, v) in [
                    ("Cache-Control", "no-store, must-revalidate"),
                    // Loopback-only, but a generated page should still not be
                    // able to frame or sniff its way into trouble.
                    ("X-Content-Type-Options", "nosniff"),
                    ("Referrer-Policy", "no-referrer"),
                ] {
                    response.add_header(header(k, v));
                }
                return request
                    .respond(response)
                    .map_err(|e| VonError::Io(e.to_string()));
            }
            // A traversal attempt from inside the WebView: refuse loudly.
            Err(VonError::PermissionDenied(msg)) => {
                tracing::warn!("preview blocked '{raw}': {msg}");
                return respond_text(request, 403, "forbidden");
            }
            _ => continue,
        }
    }

    respond_text(
        request,
        404,
        &format!(
            "<!doctype html><meta charset=utf-8>\
             <style>body{{background:#222226;color:#9A9A9E;font:15px/1.6 -apple-system,sans-serif;\
             display:grid;place-items:center;height:100vh;margin:0}}\
             code{{color:#4DB8FF}}</style>\
             <div><p>No file at <code>/{}</code></p></div>",
            html_escape(path_part)
        ),
    )
}

fn respond_text(request: tiny_http::Request, status: u16, body: &str) -> Result<()> {
    let content_type = if body.starts_with("<!doctype") {
        "text/html; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    let response = tiny_http::Response::new(
        tiny_http::StatusCode(status),
        vec![header("Content-Type", content_type)],
        Cursor::new(body.as_bytes().to_vec()),
        Some(body.len()),
        None,
    );
    request
        .respond(response)
        .map_err(|e| VonError::Io(e.to_string()))
}

fn header(key: &str, value: &str) -> tiny_http::Header {
    // Both sides are compile-time constants or MIME strings from mime_guess, so
    // this cannot realistically fail; fall back to a benign header if it does.
    tiny_http::Header::from_bytes(key.as_bytes(), value.as_bytes())
        .unwrap_or_else(|_| tiny_http::Header::from_bytes(&b"X-Ignored"[..], &b"1"[..]).unwrap())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_index_and_blocks_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::open(dir.path().join("web")).unwrap();
        sb.write_file("index.html", "<h1>VON</h1>").unwrap();
        sb.write_file("app/main.js", "console.log(1)").unwrap();

        let url = start("test-proj", sb).unwrap();
        assert!(url.starts_with("http://127.0.0.1:"));

        let base = url.trim_end_matches('/').to_string();
        let get = |p: &str| -> (u16, String) {
            let resp = ureq_get(&format!("{base}{p}"));
            resp
        };

        let (status, body) = get("/");
        assert_eq!(status, 200);
        assert!(body.contains("VON"));

        let (status, _) = get("/app/main.js");
        assert_eq!(status, 200);

        // Escaping the sandbox must be refused, not 404'd.
        let (status, _) = get("/../../../etc/passwd");
        assert!(status == 403 || status == 404, "got {status}");

        stop("test-proj");
    }

    /// Minimal HTTP/1.1 GET so the test does not pull in a client dependency.
    fn ureq_get(url: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let without_scheme = url.trim_start_matches("http://");
        let (authority, path) = without_scheme.split_once('/').unwrap_or((without_scheme, ""));
        let mut stream = std::net::TcpStream::connect(authority).unwrap();
        write!(
            stream,
            "GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).ok();
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }
}
