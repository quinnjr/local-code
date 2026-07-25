//! A hand-rolled HTTP/1.1 static-file server for agent-created artifacts.
//!
//! Deliberately dependency-free (tokio only): the request surface is tiny —
//! GET/HEAD of files under one directory — and every response sets
//! `Cache-Control: no-store`, because an agent iterating on a design between
//! turns must never have the user's browser show a stale cached copy.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::url::encode_path_segment;

/// Largest request head read before giving up — a browser's GET head is a few
/// hundred bytes; 32 KiB is generous while still bounding a peer that streams
/// headers forever to hold the connection open.
const MAX_HEAD_BYTES: usize = 32 * 1024;

/// A running artifact server: the address it is bound to and the bearer token
/// its URLs require. Handed out by [`ensure_server`]; the tool builds base
/// URLs from it.
#[derive(Debug, Clone)]
pub struct ServerHandle {
    pub addr: SocketAddr,
    pub token: String,
}

/// Running servers, keyed by canonical artifacts-directory path. Process-wide
/// — not held on the tool instance — because `register_all_tools` runs on
/// every agent rebuild (`/model`, `/resume`, `/mcp add`): a fresh tool per
/// rebuild must find the already-running server here rather than bind a new
/// port and orphan every URL shared so far.
static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, ServerHandle>>> = OnceLock::new();

/// Serves `root` (created if missing) over HTTP on 127.0.0.1, returning the
/// server's handle — OS-assigned address plus unguessable URL token on first
/// call, the existing server's handle on every later call for the same
/// directory. Localhost-only by design: artifacts are for the user's own
/// browser, not the network.
pub async fn ensure_server(root: &Path) -> std::io::Result<ServerHandle> {
    tokio::fs::create_dir_all(root).await?;
    let canonical = tokio::fs::canonicalize(root).await?;

    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = registry.lock().await;
    if let Some(handle) = guard.get(&canonical) {
        return Ok(handle.clone());
    }

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let handle = ServerHandle {
        addr: listener.local_addr()?,
        token: generate_token(),
    };
    tokio::spawn(accept_loop(listener, canonical.clone(), handle.clone()));
    guard.insert(canonical, handle.clone());
    Ok(handle)
}

/// 32 lowercase hex chars derived from a std `RandomState` (which std seeds
/// randomly per process) hashing the current time, the pid, and the state
/// object's own address. The result is unguessable to blind/coarse attackers
/// — a remote website probing 127.0.0.1, a port scanner — but it is NOT a
/// cryptographic secret: a same-user process could read the session files
/// the token appears in anyway.
fn generate_token() -> String {
    use std::hash::{BuildHasher, Hasher};

    let state = std::collections::hash_map::RandomState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let state_addr = &state as *const _ as usize;

    let mut first = state.build_hasher();
    first.write_u128(nanos);
    first.write_u32(pid);
    first.write_usize(state_addr);
    // Second pass over the same inputs in a different order, so the token
    // is 32 hex chars rather than 16.
    let mut second = state.build_hasher();
    second.write_usize(state_addr);
    second.write_u128(nanos);
    second.write_u32(pid);
    format!("{:016x}{:016x}", first.finish(), second.finish())
}

/// Byte-wise constant-time comparison, so a wrong-token 404's timing doesn't
/// reveal how many leading characters were right.
fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}

async fn accept_loop(listener: TcpListener, root: PathBuf, handle: ServerHandle) {
    let port = handle.addr.port();
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let root = root.clone();
                let token = handle.token.clone();
                // Per-connection outcomes (peer resets, malformed requests)
                // are answered with 4xx inside `handle_connection` where
                // possible and otherwise dropped here — one bad request must
                // never take down the server task. The 120s whole-connection
                // bound is generous for localhost + mockup-scale files and
                // covers slow head-drips AND slow body-readers alike, so no
                // connection-count semaphore is needed (KISS).
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(120),
                        handle_connection(stream, root, port, &token),
                    )
                    .await;
                });
            }
            Err(e) => {
                // A failed accept (transient EMFILE/ECONNABORTED, ...) must
                // not kill the server — log and keep accepting.
                tracing::warn!("artifact server accept failed (continuing): {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    // The loop above never exits in normal operation; if it ever did, drop
    // our registry entry — but only if it still names THIS server, so a
    // successor's entry is never deleted.
    #[allow(unreachable_code)]
    {
        let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = registry.lock().await;
        if guard.get(&root).map(|h| h.addr) == Some(handle.addr) {
            guard.remove(&root);
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    root: PathBuf,
    port: u16,
    token: &str,
) -> std::io::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    // `take` makes the MAX_HEAD_BYTES bound effective mid-line, not just at
    // line boundaries: an endless single header line is cut off at
    // MAX_HEAD_BYTES + 1, where the accumulator check below fires the 400.
    let mut reader = BufReader::new(read_half.take((MAX_HEAD_BYTES + 1) as u64));

    // Read the request head (request line + headers) up to the blank line,
    // capturing the Host header for the DNS-rebinding check below.
    let mut head = String::new();
    let mut host: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(()); // peer went away mid-head
        }
        head.push_str(&line);
        if head.len() > MAX_HEAD_BYTES {
            return respond_simple(
                &mut writer,
                "400 Bad Request",
                "request head too large",
                false,
            )
            .await;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if host.is_none()
            && let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("host")
        {
            host = Some(value.trim().to_string());
        }
    }

    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return respond_simple(
            &mut writer,
            "400 Bad Request",
            "malformed request line",
            false,
        )
        .await;
    };

    // DNS-rebinding guard: only answer clients that believe they're talking
    // to this loopback server. HTTP/1.1 requires Host; a request naming any
    // other authority (evil.com after its DNS flips to 127.0.0.1) is refused.
    let Some(host) = host else {
        return respond_simple(&mut writer, "400 Bad Request", "missing Host header", false).await;
    };
    let allowed_hosts = [
        format!("127.0.0.1:{port}"),
        "127.0.0.1".to_string(),
        format!("localhost:{port}"),
        "localhost".to_string(),
    ];
    if !allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return respond_simple(&mut writer, "403 Forbidden", "forbidden", false).await;
    }

    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            return respond_with(
                &mut writer,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                b"only GET and HEAD are supported",
                false,
                "Allow: GET, HEAD\r\n",
            )
            .await;
        }
    };

    // The token prefix is the only auth: a missing or wrong token gets the
    // same 404 as a missing file, so the response never confirms that a
    // token exists. The query string never reaches path resolution.
    let raw_path = target.split('?').next().unwrap_or("/");
    let Some(first_segment) = raw_path.strip_prefix('/') else {
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    };
    let (request_token, resource) = match first_segment.find('/') {
        Some(at) => (&first_segment[..at], &first_segment[at..]),
        None => (first_segment, "/"),
    };
    if !token_eq(request_token, token) {
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    }

    let Some(candidate) = resolve_requested_path(&root, resource) else {
        return respond_simple(
            &mut writer,
            "400 Bad Request",
            "invalid request path",
            head_only,
        )
        .await;
    };

    // canonicalize resolves any symlinks planted inside the artifacts dir;
    // the containment check then refuses to serve anything outside it.
    let Ok(resolved) = tokio::fs::canonicalize(&candidate).await else {
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    };
    if !resolved.starts_with(&root) {
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    }

    let Ok(meta) = tokio::fs::metadata(&resolved).await else {
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    };
    if !meta.is_dir() && !meta.is_file() {
        // FIFOs, sockets, and device nodes must never reach
        // `tokio::fs::read` — opening a FIFO would block the connection
        // task on a writer that never comes.
        return respond_simple(&mut writer, "404 Not Found", "not found", head_only).await;
    }

    if meta.is_dir() {
        // Relative links in a directory listing only resolve correctly when
        // the URL ends in '/' — send the browser to the canonical form
        // first. The Location is REBUILT from the resolved directory's path
        // relative to the served root (never the raw request target), so a
        // crafted request can't turn the 301 into an open redirect.
        if !resource.ends_with('/') {
            let location = redirect_location(&root, &resolved, token);
            return redirect(&mut writer, &location).await;
        }
        // A symlinked index.html pointing outside the root is refused the
        // same way any other escaping symlink is.
        let index = match tokio::fs::canonicalize(resolved.join("index.html")).await {
            Ok(canonical) if canonical.starts_with(&root) => {
                match tokio::fs::metadata(&canonical).await {
                    Ok(index_meta) if index_meta.is_file() => Some(canonical),
                    _ => None,
                }
            }
            _ => None,
        };
        match index {
            Some(index) => {
                serve_file(&mut writer, &index, "text/html; charset=utf-8", head_only).await
            }
            None => {
                let listing = match render_listing(&resolved, resource).await {
                    Ok(listing) => listing,
                    Err(_) => {
                        return respond_simple(
                            &mut writer,
                            "404 Not Found",
                            "not found",
                            head_only,
                        )
                        .await;
                    }
                };
                // The listing is generated HTML that needs no scripts or
                // styles; user artifacts are intentionally script-capable
                // and get no CSP.
                respond_with(
                    &mut writer,
                    "200 OK",
                    "text/html; charset=utf-8",
                    listing.as_bytes(),
                    head_only,
                    "Content-Security-Policy: default-src 'none'\r\n",
                )
                .await
            }
        }
    } else {
        serve_file(
            &mut writer,
            &resolved,
            content_type_for(&resolved),
            head_only,
        )
        .await
    }
}

/// Maps a resource path (`/some/file.css`, query string already stripped) to
/// a path under `root`, percent-decoding it and rejecting anything that
/// escapes `root` via `..` or names a dotfile. (Symlink escapes are caught
/// separately by the canonicalize containment check in `handle_connection`.)
fn resolve_requested_path(root: &Path, resource: &str) -> Option<PathBuf> {
    let decoded = percent_decode(resource)?;
    let mut path = root.to_path_buf();
    for component in Path::new(&decoded).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(segment) => {
                // The served dir is agent-writable and shown to the user's
                // browser; dotfiles (.env and friends) must never leave it.
                if segment.to_str().is_some_and(|name| name.starts_with('.')) {
                    return None;
                }
                path.push(segment);
            }
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

/// Rebuilds the canonical `/`-suffixed URL for a directory redirect from the
/// RESOLVED directory's path relative to the served root — never from the
/// raw request target, so a crafted request can't turn the 301 into an open
/// redirect.
fn redirect_location(root: &Path, resolved_dir: &Path, token: &str) -> String {
    let mut location = format!("/{token}");
    if let Ok(relative) = resolved_dir.strip_prefix(root) {
        for component in relative.components() {
            if let Component::Normal(segment) = component {
                location.push('/');
                location.push_str(&encode_path_segment(&segment.to_string_lossy()));
            }
        }
    }
    location.push('/');
    location
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = input.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A minimal HTML index of `dir`'s immediate entries — what the user sees at
/// the base URL when no `index.html` exists, so "showcase my work" works with
/// zero setup beyond dropping files in the directory.
async fn render_listing(dir: &Path, resource: &str) -> std::io::Result<String> {
    let mut entries: Vec<(String, bool)> = Vec::new();
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Never list dotfiles: the served dir is agent-writable and shown
        // to the user's browser, so .env and friends must not leave it.
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        entries.push((name, is_dir));
    }
    entries.sort();

    let title = percent_decode(resource).unwrap_or_else(|| "/".to_string());
    let mut html = format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <title>local-code artifacts — {0}</title></head><body>\
         <h1>local-code artifacts — {0}</h1><ul>",
        html_escape(&title)
    );
    for (name, is_dir) in entries {
        let suffix = if is_dir { "/" } else { "" };
        html.push_str(&format!(
            "<li><a href=\"{}{}\">{}{}</a></li>",
            encode_path_segment(&name),
            suffix,
            html_escape(&name),
            suffix
        ));
    }
    html.push_str("</ul></body></html>\n");
    Ok(html)
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("map") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Buffers the whole file, then responds. Artifacts are mockup-scale, and
/// buffering makes Content-Length and body atomic with respect to the agent
/// rewriting the file mid-fetch (a streamed file could change length between
/// the header and the last byte). A file that vanished between canonicalize
/// and read becomes a 404, not a dropped connection.
async fn serve_file<W: AsyncWrite + Unpin>(
    writer: &mut W,
    path: &Path,
    content_type: &str,
    head_only: bool,
) -> std::io::Result<()> {
    let body = match tokio::fs::read(path).await {
        Ok(body) => body,
        Err(_) => return respond_simple(writer, "404 Not Found", "not found", head_only).await,
    };
    respond(writer, "200 OK", content_type, &body, head_only).await
}

async fn respond<W: AsyncWrite + Unpin>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    respond_with(writer, status, content_type, body, head_only, "").await
}

#[allow(clippy::too_many_arguments)]
async fn respond_with<W: AsyncWrite + Unpin>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    extra_headers: &str,
) -> std::io::Result<()> {
    write_head(
        writer,
        status,
        content_type,
        body.len() as u64,
        extra_headers,
    )
    .await?;
    if !head_only {
        writer.write_all(body).await?;
    }
    writer.flush().await
}

async fn respond_simple<W: AsyncWrite + Unpin>(
    writer: &mut W,
    status: &str,
    message: &str,
    head_only: bool,
) -> std::io::Result<()> {
    respond(
        writer,
        status,
        "text/plain; charset=utf-8",
        message.as_bytes(),
        head_only,
    )
    .await
}

/// A 301 to `location` (always a server-rebuilt path, never the raw request
/// target) with no body. Kept separate from `respond` because a redirect
/// carries no Content-Type — and the module-wide `no-store` rule matters
/// double here: a cached 301 would pin the browser to a stale location.
async fn redirect<W: AsyncWrite + Unpin>(writer: &mut W, location: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\n\
         X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    writer.write_all(head.as_bytes()).await?;
    writer.flush().await
}

async fn write_head<W: AsyncWrite + Unpin>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    len: u64,
    extra_headers: &str,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\n\
         {extra_headers}X-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    writer.write_all(head.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Starts a server for `dir` and returns its base URL
    /// (`http://127.0.0.1:<port>/<token>`) plus the handle.
    async fn start(dir: &Path) -> (String, ServerHandle) {
        let handle = ensure_server(dir).await.expect("server should start");
        (format!("http://{}/{}", handle.addr, handle.token), handle)
    }

    async fn get(url: &str) -> reqwest::Response {
        reqwest::get(url).await.expect("GET should succeed")
    }

    /// Speaks raw HTTP to the server: writes `request` verbatim, reads to
    /// EOF, returns the response as a string. A reset after the response was
    /// delivered (the server closes with our request bytes still unread) is
    /// tolerated — whatever arrived is returned.
    async fn raw_request(addr: SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request).await.expect("write");
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8(response).expect("utf8 response")
    }

    #[tokio::test]
    async fn ensure_server_creates_the_dir_and_reuses_the_port() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("artifacts");
        let handle = ensure_server(&root).await.unwrap();
        assert!(handle.addr.port() > 0);
        assert_eq!(handle.token.len(), 32);
        assert!(root.is_dir());
        assert_eq!(ensure_server(&root).await.unwrap().addr, handle.addr);
        // A different spelling of the same directory reuses the server too
        // (the registry is keyed by canonical path).
        let spelled_differently = dir.path().join("./artifacts");
        assert_eq!(
            ensure_server(&spelled_differently).await.unwrap().addr,
            handle.addr
        );
    }

    #[tokio::test]
    async fn serves_a_file_with_its_content_type() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("mockup.html"), "<h1>hello</h1>").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/mockup.html")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        // CSP is for the generated listing only; user artifacts are
        // intentionally script-capable.
        assert!(response.headers().get("content-security-policy").is_none());
        assert_eq!(response.text().await.unwrap(), "<h1>hello</h1>");
    }

    #[tokio::test]
    async fn serves_index_html_at_the_directory_url() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<h1>index</h1>").unwrap();
        std::fs::write(dir.path().join("other.txt"), "not the index").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "<h1>index</h1>");
    }

    #[tokio::test]
    async fn renders_a_listing_when_no_index_html_exists() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("design v2.html"), "<p>x</p>").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/")).await;
        assert_eq!(response.status(), 200);
        // The generated listing needs no scripts or styles.
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "default-src 'none'"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("design v2.html"), "{body}");
        // The link must be URL-encoded so a name with a space stays clickable.
        assert!(body.contains("href=\"design%20v2.html\""), "{body}");
    }

    #[tokio::test]
    async fn an_empty_dir_gets_an_empty_listing() {
        let dir = tempdir().unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/")).await;
        assert_eq!(response.status(), 200);
        assert!(response.text().await.unwrap().contains("<ul></ul>"));
    }

    #[tokio::test]
    async fn head_on_a_listing_returns_headers_without_a_body() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let client = reqwest::Client::new();
        let response = client.head(format!("{base}/")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "default-src 'none'"
        );
        assert_eq!(response.text().await.unwrap(), "");
    }

    #[tokio::test]
    async fn percent_decodes_request_paths() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("my file.txt"), "spaced").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/my%20file.txt")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "spaced");
    }

    #[tokio::test]
    async fn serves_files_from_nested_subdirectories() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested").join("my report.md"), "# hi").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/nested/my%20report.md")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/markdown; charset=utf-8"
        );
        assert_eq!(response.text().await.unwrap(), "# hi");
    }

    #[tokio::test]
    async fn query_strings_are_ignored() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("mockup.html"), "<h1>v1</h1>").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (base, handle) = start(dir.path()).await;

        let response = get(&format!("{base}/mockup.html?x=1")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "<h1>v1</h1>");

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = client.get(format!("{base}/sub?x=1")).send().await.unwrap();
        assert_eq!(response.status(), 301);
        assert_eq!(
            response.headers().get("location").unwrap(),
            format!("/{}/sub/", handle.token).as_str()
        );
    }

    #[tokio::test]
    async fn missing_files_are_a_404() {
        let dir = tempdir().unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/nope.txt")).await;
        assert_eq!(response.status(), 404);
    }

    #[tokio::test]
    async fn requests_without_the_token_are_a_404() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("mockup.html"), "<h1>x</h1>").unwrap();
        let (_base, handle) = start(dir.path()).await;

        // No token at all: the base URL's first segment isn't the token.
        let response = get(&format!("http://{}/mockup.html", handle.addr)).await;
        assert_eq!(response.status(), 404);
        // A wrong token of the right shape gets the same 404 — the response
        // never confirms that a token exists.
        let response = get(&format!(
            "http://{}/{}/mockup.html",
            handle.addr,
            "0".repeat(32)
        ))
        .await;
        assert_eq!(response.status(), 404);
    }

    #[tokio::test]
    async fn dot_dot_traversal_is_rejected() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();
        let served = dir.path().join("artifacts");
        let (_base, handle) = start(&served).await;

        // Encoded attempts must be refused by the server itself with a 400.
        // Sent over a raw stream because reqwest's WHATWG URL parser
        // normalizes %2e%2e away client-side before the request is sent.
        for attempt in ["/..%2Fsecret.txt", "/%2e%2e/secret.txt", "/%zz"] {
            let request = format!(
                "GET /{}{attempt} HTTP/1.1\r\nHost: {}\r\n\r\n",
                handle.token, handle.addr
            );
            let response = raw_request(handle.addr, request.as_bytes()).await;
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "{attempt} must be a 400: {response}"
            );
            assert!(
                !response.contains("top secret"),
                "{attempt} leaked the file"
            );
        }
        // reqwest normalizes a literal `/../` out of the URL before sending,
        // which is itself a refusal — but it must never produce a 200 either.
        let response = get(&format!(
            "http://{}/{}/../../secret.txt",
            handle.addr, handle.token
        ))
        .await;
        assert_ne!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("top secret"));
    }

    #[tokio::test]
    async fn symlinks_escaping_the_served_dir_are_a_404() {
        let dir = tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();
        let served = dir.path().join("artifacts");
        std::fs::create_dir(&served).unwrap();
        std::os::unix::fs::symlink(&secret, served.join("leak.txt")).unwrap();
        let (base, _handle) = start(&served).await;

        let response = get(&format!("{base}/leak.txt")).await;
        assert_eq!(response.status(), 404);
        assert!(!response.text().await.unwrap().contains("top secret"));
    }

    #[tokio::test]
    async fn symlinked_index_html_pointing_outside_is_not_served() {
        let dir = tempdir().unwrap();
        let secret = dir.path().join("real-index.html");
        std::fs::write(&secret, "top secret").unwrap();
        let served = dir.path().join("artifacts");
        std::fs::create_dir(&served).unwrap();
        std::os::unix::fs::symlink(&secret, served.join("index.html")).unwrap();
        let (base, _handle) = start(&served).await;

        // The index symlink is refused, so the directory listing is served
        // instead — a 200 whose body never contains the secret.
        let response = get(&format!("{base}/")).await;
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        assert!(body.contains("<ul>"), "expected the listing: {body}");
        assert!(!body.contains("top secret"));
    }

    #[tokio::test]
    async fn dotfiles_are_neither_served_nor_listed() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".secret"), "shh").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "hi").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/.secret")).await;
        assert_eq!(response.status(), 400);

        let response = get(&format!("{base}/")).await;
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        assert!(!body.contains(".secret"), "{body}");
        assert!(body.contains("visible.txt"), "{body}");
    }

    #[tokio::test]
    async fn head_returns_headers_without_a_body() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "twelve bytes").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let client = reqwest::Client::new();
        let response = client.head(format!("{base}/a.txt")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers().get("content-length").unwrap(), "12");
        assert_eq!(response.text().await.unwrap(), "");
    }

    #[tokio::test]
    async fn head_on_a_missing_file_returns_headers_without_a_body() {
        let dir = tempdir().unwrap();
        let (base, _handle) = start(dir.path()).await;

        let client = reqwest::Client::new();
        let response = client.head(format!("{base}/missing")).send().await.unwrap();
        assert_eq!(response.status(), 404);
        // "not found" — headers (incl. Content-Length) but no body.
        assert_eq!(response.headers().get("content-length").unwrap(), "9");
        assert_eq!(response.text().await.unwrap(), "");
    }

    #[tokio::test]
    async fn subdirectory_without_trailing_slash_redirects() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (base, handle) = start(dir.path()).await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = client.get(format!("{base}/sub")).send().await.unwrap();
        assert_eq!(response.status(), 301);
        assert_eq!(
            response.headers().get("location").unwrap(),
            format!("/{}/sub/", handle.token).as_str()
        );
        // A cached 301 would pin the browser to a stale location.
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    }

    #[tokio::test]
    async fn a_garbled_request_line_is_a_400() {
        let dir = tempdir().unwrap();
        let (_base, handle) = start(dir.path()).await;

        let response = raw_request(handle.addr, b"gibberish\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[tokio::test]
    async fn an_oversized_request_head_is_a_400() {
        let dir = tempdir().unwrap();
        let (_base, handle) = start(dir.path()).await;

        let mut request = b"GET / ".to_vec();
        request.extend_from_slice(&vec![b'a'; 40 * 1024]);
        request.extend_from_slice(b"\r\n\r\n");
        let response = raw_request(handle.addr, &request).await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[tokio::test]
    async fn a_post_is_a_405_with_an_allow_header() {
        let dir = tempdir().unwrap();
        let (base, _handle) = start(dir.path()).await;

        let client = reqwest::Client::new();
        let response = client.post(format!("{base}/x")).send().await.unwrap();
        assert_eq!(response.status(), 405);
        assert_eq!(response.headers().get("allow").unwrap(), "GET, HEAD");
    }

    #[tokio::test]
    async fn a_request_with_a_foreign_host_header_is_a_403() {
        let dir = tempdir().unwrap();
        let (_base, handle) = start(dir.path()).await;

        let request = format!("GET /{}/ HTTP/1.1\r\nHost: evil.com\r\n\r\n", handle.token);
        let response = raw_request(handle.addr, request.as_bytes()).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }

    #[tokio::test]
    async fn a_request_without_a_host_header_is_a_400() {
        let dir = tempdir().unwrap();
        let (_base, handle) = start(dir.path()).await;

        let request = format!("GET /{}/ HTTP/1.1\r\n\r\n", handle.token);
        let response = raw_request(handle.addr, request.as_bytes()).await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[tokio::test]
    async fn css_is_served_as_text_css() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("style.css"), "body{}").unwrap();
        let (base, _handle) = start(dir.path()).await;

        let response = get(&format!("{base}/style.css")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
    }

    #[test]
    fn content_type_for_maps_extensions_case_insensitively() {
        assert_eq!(
            content_type_for(Path::new("a.HTML")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("a.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type_for(Path::new("a.PNG")), "image/png");
        assert_eq!(
            content_type_for(Path::new("a.xyz")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for(Path::new("noext")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for(Path::new("a.tar.gz")),
            "application/octet-stream"
        );
    }

    #[test]
    fn percent_decode_rejects_invalid_sequences() {
        assert_eq!(percent_decode("/a%20b"), Some("/a b".to_string()));
        assert_eq!(percent_decode("/%zz"), None);
        assert_eq!(percent_decode("/%1"), None);
        assert_eq!(percent_decode("/%FF"), None); // invalid UTF-8
        assert_eq!(percent_decode("/plain"), Some("/plain".to_string()));
    }

    #[test]
    fn token_eq_compares_constant_time_style() {
        assert!(token_eq("abc", "abc"));
        assert!(!token_eq("abc", "abd"));
        assert!(!token_eq("abc", "abcd"));
        assert!(!token_eq("", "a"));
    }
}
