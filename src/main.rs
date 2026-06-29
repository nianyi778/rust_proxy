use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Method, Request, Response, StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use hyper::client::HttpConnector;
use hyper::header::HeaderValue;
use hyper_rustls::HttpsConnectorBuilder;
use serde::Deserialize;
use tracing::{debug, info, warn};
use url::Url;

#[derive(Clone)]
struct AppState {
    client: hyper::Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listen_addr: SocketAddr = listen_addr
        .parse()
        .context("LISTEN_ADDR must be in the form host:port")?;

    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(Duration::from_secs(10)));

    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);

    let client = hyper::Client::builder()
        .http2_adaptive_window(true)
        .build::<_, Body>(https);

    let state = Arc::new(AppState { client });

    let app = Router::new()
        .route("/health", get(health))
        .route("/stream", any(stream))
        .route("/api/proxy/stream", any(stream))
        .with_state(state);

    info!(%listen_addr, "starting video stream proxy");

    axum::Server::bind(&listen_addr)
        .serve(app.into_make_service())
        .await
        .context("server error")?;

    Ok(())
}

async fn health() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    url: String,
    referer: Option<String>, // 自定义 referer，优先使用
}

async fn stream(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StreamQuery>,
    req: Request<Body>,
) -> Result<Response<Body>, (StatusCode, String)> {
    if req.method() == Method::OPTIONS {
        return Ok(cors_response(StatusCode::NO_CONTENT, Body::empty()));
    }

    match *req.method() {
        Method::GET | Method::HEAD => {}
        _ => return Err((StatusCode::METHOD_NOT_ALLOWED, "method not allowed".into())),
    }

    if q.url.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "URL is required".into()));
    }
    if q.url.len() > 6000 {
        return Err((StatusCode::BAD_REQUEST, "URL is too long".into()));
    }

    let target =
        Url::parse(&q.url).map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid URL: {e}")))?;
    match target.scheme() {
        "http" | "https" => {}
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid URL".into())),
    }
    if target.host_str().is_none() {
        return Err((StatusCode::BAD_REQUEST, "Invalid URL".into()));
    }

    let proxy_origin = request_origin(&req);
    let self_path = req.uri().path().to_string();
    let range_header = req
        .headers()
        .get(hyper::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let target_uri: Uri = target
        .as_str()
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid URL: {e}")))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        hyper::header::USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        ),
    );

    // 使用自定义 referer（从 query 参数）或从 target URL 自动提取
    let referer_value = q.referer.as_deref().map(|r| {
        if r.ends_with('/') {
            r.to_string()
        } else {
            format!("{}/", r)
        }
    });

    // 如果没有自定义 referer，尝试从 target URL 提取
    let referer_value = referer_value.or_else(|| {
        let origin = target.origin().ascii_serialization();
        if !origin.is_empty() {
            Some(format!("{}/", origin))
        } else {
            None
        }
    });

    if let Some(referer) = referer_value {
        if let Ok(referer_header) = referer.parse::<HeaderValue>() {
            headers.insert(hyper::header::REFERER, referer_header);
            debug!(referer = %referer, "using referer");
        }
    }

    if let Ok(origin) = target.origin().ascii_serialization().parse::<HeaderValue>() {
        headers.insert(hyper::header::ORIGIN, origin);
    }

    if let Some(range) = range_header.as_deref() {
        if let Ok(v) = range.parse::<HeaderValue>() {
            headers.insert(hyper::header::RANGE, v);
        }
    }

    let mut upstream_resp = fetch_with_timeout(
        &state,
        req.method().clone(),
        target_uri.clone(),
        headers.clone(),
        15,
    )
    .await?;

    if upstream_resp.status() == StatusCode::FORBIDDEN {
        debug!(target = %target, "403 with Referer/Origin, retrying without them");
        headers.remove(hyper::header::REFERER);
        headers.remove(hyper::header::ORIGIN);
        upstream_resp =
            fetch_with_timeout(&state, req.method().clone(), target_uri, headers, 10).await?;
    }

    if !upstream_resp.status().is_success() && upstream_resp.status() != StatusCode::PARTIAL_CONTENT
    {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("upstream error: {}", upstream_resp.status()),
        ));
    }

    let content_type = upstream_resp
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let url_lower = q.url.to_ascii_lowercase();

    let is_m3u8 = content_type.contains("application/vnd.apple.mpegurl")
        || content_type.contains("application/x-mpegurl")
        || url_lower.contains(".m3u8");

    if is_m3u8 && upstream_resp.status().is_success() {
        let (parts, body) = upstream_resp.into_parts();
        let bytes = hyper::body::to_bytes(body)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("failed to read m3u8: {e}")))?;
        let text = String::from_utf8_lossy(&bytes).to_string();

        let proxy_path = if self_path == "/api/proxy/stream" {
            "/api/proxy/stream"
        } else {
            "/stream"
        };
        let max_ad_secs = ad_filter_max_secs();
        let rewritten = rewrite_m3u8(&text, &q.url, &proxy_origin, proxy_path, max_ad_secs)
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("failed to rewrite m3u8: {e}"),
                )
            })?;

        let mut resp = Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(rewritten))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;

        *resp.headers_mut() = parts.headers;
        resp.headers_mut().remove(hyper::header::CONTENT_LENGTH);
        resp.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.apple.mpegurl"),
        );
        resp.headers_mut().insert(
            hyper::header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=10, stale-while-revalidate=30"),
        );
        add_stream_cors_headers(resp.headers_mut());
        return Ok(resp);
    }

    add_stream_cors_headers(upstream_resp.headers_mut());
    apply_cache_policy(upstream_resp.headers_mut(), &url_lower);
    Ok(upstream_resp)
}

async fn fetch_with_timeout(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    timeout_secs: u64,
) -> Result<Response<Body>, (StatusCode, String)> {
    let mut builder = Request::builder().method(method).uri(uri);
    *builder.headers_mut().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "failed to build request".into(),
    ))? = headers;

    let req = builder
        .body(Body::empty())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;

    let fut = state.client.request(req);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => {
            warn!(error = %e, "upstream request failed");
            Err((
                StatusCode::BAD_GATEWAY,
                format!("upstream request failed: {e}"),
            ))
        }
        Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, "源服务器响应超时".into())),
    }
}

fn request_origin(req: &Request<Body>) -> String {
    // Prefer forwarded headers when behind reverse proxy.
    let proto = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = req
        .headers()
        .get("x-forwarded-host")
        .or_else(|| req.headers().get(hyper::header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{}", proto, host)
}

fn add_stream_cors_headers(headers: &mut HeaderMap) {
    headers.insert(
        "access-control-allow-origin",
        hyper::header::HeaderValue::from_static("*"),
    );
    headers.insert(
        "access-control-allow-methods",
        hyper::header::HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        "access-control-allow-headers",
        hyper::header::HeaderValue::from_static(
            "Range, Accept-Encoding, Origin, Content-Type, Accept",
        ),
    );
    headers.insert(
        "access-control-expose-headers",
        hyper::header::HeaderValue::from_static(
            "Content-Length, Content-Range, Content-Type, Accept-Ranges",
        ),
    );
}

fn cors_response(status: StatusCode, body: Body) -> Response<Body> {
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    add_stream_cors_headers(resp.headers_mut());
    resp
}

fn apply_cache_policy(headers: &mut HeaderMap, url_lower: &str) {
    if url_lower.contains(".ts") || url_lower.contains(".m4s") {
        headers.insert(
            hyper::header::CACHE_CONTROL,
            hyper::header::HeaderValue::from_static("public, max-age=86400, immutable"),
        );
    } else if url_lower.contains(".mp4") || url_lower.contains(".mkv") {
        headers.insert(
            hyper::header::CACHE_CONTROL,
            hyper::header::HeaderValue::from_static("public, max-age=3600"),
        );
    }
}

fn rewrite_m3u8(
    content: &str,
    base_url: &str,
    proxy_origin: &str,
    proxy_path: &str,
    max_ad_secs: f64,
) -> anyhow::Result<String> {
    let base = Url::parse(base_url)?;
    // Strip inserted ads before rewriting segment URLs through the proxy.
    let filtered = filter_ad_breaks(content, max_ad_secs);
    let mut out = Vec::new();

    for line in filtered.lines() {
        if let Some(rewritten) = rewrite_ext_x_key_line(line, &base, proxy_origin, proxy_path)? {
            out.push(rewritten);
            continue;
        }

        if line.starts_with('#') || line.trim().is_empty() {
            out.push(line.to_string());
            continue;
        }

        let resolved = base.join(line.trim())?;
        out.push(format!(
            "{}{}?url={}",
            proxy_origin,
            proxy_path,
            urlencoding::encode(resolved.as_str())
        ));
    }

    Ok(out.join("\n"))
}

/// Max duration (seconds) of an interior discontinuity break still treated as an
/// ad by rule 2. Configurable via `AD_FILTER_MAX_SECS`; `0` disables rule 2.
fn ad_filter_max_secs() -> f64 {
    std::env::var("AD_FILTER_MAX_SECS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(30.0)
}

/// Remove ads that upstream 采集站 splice into HLS playlists. Two passes:
/// 1. Explicit SCTE-35 ad breaks marked by `#EXT-X-CUE-OUT`/`#EXT-X-CUE-IN`.
/// 2. Discontinuity-delimited ad pods that carry no CUE markers, detected by
///    path/host divergence (preferred) or, when no such signal exists, by the
///    short duration of an interior break.
fn filter_ad_breaks(content: &str, max_break_secs: f64) -> String {
    let cue_filtered = strip_cue_breaks(content);
    strip_ad_runs(&cue_filtered, max_break_secs)
}

/// Drop everything between `#EXT-X-CUE-OUT` and `#EXT-X-CUE-IN`, leaving a single
/// `#EXT-X-DISCONTINUITY` at each splice so the player resets its decoder.
fn strip_cue_breaks(content: &str) -> String {
    let mut out = Vec::new();
    let mut in_ad_block = false;
    let mut pending_discontinuity = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if is_cue_out(trimmed) {
            in_ad_block = true;
            continue;
        }
        if is_cue_in(trimmed) {
            in_ad_block = false;
            pending_discontinuity = true;
            continue;
        }
        if in_ad_block {
            continue;
        }

        if pending_discontinuity {
            if trimmed.is_empty() {
                continue;
            }
            out.push("#EXT-X-DISCONTINUITY".to_string());
            pending_discontinuity = false;
            if trimmed.starts_with("#EXT-X-DISCONTINUITY") {
                continue;
            }
        }

        out.push(line.to_string());
    }

    out.join("\n")
}

/// A run of playlist lines bounded by `#EXT-X-DISCONTINUITY` markers.
#[derive(Default)]
struct M3u8Run {
    lines: Vec<String>,
    duration: f64,
    seg_count: usize,
    base: Option<String>,
}

/// Remove discontinuity-delimited ad pods. A run is an ad if its segments sit on
/// a non-dominant host/path (rule 1), or — only when no such path signal exists —
/// if it is a short interior break (rule 2). Rule 2 is suppressed whenever rule 1
/// applies, so legitimately short content chunks between discontinuities survive.
fn strip_ad_runs(content: &str, max_break_secs: f64) -> String {
    let mut runs: Vec<M3u8Run> = vec![M3u8Run::default()];
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-DISCONTINUITY") {
            runs.push(M3u8Run::default());
            continue;
        }
        let cur = runs.last_mut().expect("runs is never empty");
        cur.lines.push(line.to_string());
        if let Some(secs) = extinf_secs(trimmed) {
            cur.duration += secs;
        }
        if is_segment_line(trimmed) {
            cur.seg_count += 1;
            if cur.base.is_none() {
                cur.base = Some(segment_dir(trimmed));
            }
        }
    }

    let total_segs: usize = runs.iter().map(|r| r.seg_count).sum();
    let dominant = dominant_base(&runs, total_segs);
    let has_off_path = match &dominant {
        Some(dom) => runs
            .iter()
            .any(|r| r.base.as_deref().is_some_and(|b| b != dom)),
        None => false,
    };

    let last = runs.len().saturating_sub(1);
    let mut kept: Vec<&M3u8Run> = Vec::new();
    let mut dropped_endlist = false;

    for (i, run) in runs.iter().enumerate() {
        let off_path = match (&dominant, &run.base) {
            (Some(dom), Some(b)) => b != dom,
            _ => false,
        };
        let short = max_break_secs > 0.0 && run.duration <= max_break_secs;
        // Rule 2 (only when no path signal disambiguates ads): a short interior
        // pod wedged between two runs that are far larger is an inserted ad —
        // single or looped. The large-neighbour test spares packagers that chop
        // *all* content into uniform ~20s parts (every block is small, so no
        // block stands out) and avoids the false positives that a duration-only
        // or signature-repeat heuristic produces on such streams.
        let anomaly_pod = !has_off_path && short && i > 0 && i < last && run.seg_count <= 15 && {
            let floor = 25.max(run.seg_count.saturating_mul(4));
            runs[i - 1].seg_count >= floor && runs[i + 1].seg_count >= floor
        };
        let is_ad = run.seg_count > 0 && (off_path || anomaly_pod);

        if is_ad {
            if run.lines.iter().any(|l| is_endlist(l)) {
                dropped_endlist = true;
            }
            continue;
        }
        if !run.lines.is_empty() {
            kept.push(run);
        }
    }

    let mut out: Vec<String> = Vec::new();
    for (idx, run) in kept.iter().enumerate() {
        if idx > 0 {
            out.push("#EXT-X-DISCONTINUITY".to_string());
        }
        out.extend(run.lines.iter().cloned());
    }
    // The playlist terminator may have lived inside a dropped trailing ad pod.
    if dropped_endlist && !out.iter().any(|l| is_endlist(l)) {
        out.push("#EXT-X-ENDLIST".to_string());
    }

    out.join("\n")
}

/// The host+directory shared by the most segments, but only if it covers a clear
/// majority (≥60%). Returns `None` when no single path dominates, which disables
/// path-based ad detection to avoid mis-classifying a genuinely split playlist.
fn dominant_base(runs: &[M3u8Run], total_segs: usize) -> Option<String> {
    if total_segs == 0 {
        return None;
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for run in runs {
        if let Some(base) = &run.base {
            *counts.entry(base.as_str()).or_default() += run.seg_count;
        }
    }
    let (base, count) = counts.into_iter().max_by_key(|&(_, c)| c)?;
    if (count as f64) / (total_segs as f64) >= 0.6 {
        Some(base.to_string())
    } else {
        None
    }
}

/// Directory portion of a segment URI (host included when absolute), used as the
/// key for grouping segments. Query strings are ignored.
fn segment_dir(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or(uri);
    match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => String::new(),
    }
}

fn extinf_secs(line: &str) -> Option<f64> {
    let rest = line.strip_prefix("#EXTINF:")?;
    rest.split(',').next()?.trim().parse::<f64>().ok()
}

fn is_segment_line(line: &str) -> bool {
    !line.is_empty() && !line.starts_with('#')
}

fn is_endlist(line: &str) -> bool {
    line.trim_start().starts_with("#EXT-X-ENDLIST")
}

/// Start of an ad break. Matches `#EXT-X-CUE-OUT`, `#EXT-X-CUE-OUT:30.0`, and the
/// live continuation tag `#EXT-X-CUE-OUT-CONT`.
fn is_cue_out(line: &str) -> bool {
    line.starts_with("#EXT-X-CUE-OUT")
}

/// End of an ad break. Matches `#EXT-X-CUE-IN`.
fn is_cue_in(line: &str) -> bool {
    line.starts_with("#EXT-X-CUE-IN")
}

fn rewrite_ext_x_key_line(
    line: &str,
    base: &Url,
    proxy_origin: &str,
    proxy_path: &str,
) -> anyhow::Result<Option<String>> {
    if !line.starts_with("#EXT-X-KEY:") {
        return Ok(None);
    }

    let Some(pos) = line.find("URI=") else {
        return Ok(Some(line.to_string()));
    };

    // Extract URI value (quoted or unquoted) up to comma/end
    let after = &line[(pos + 4)..];
    let (uri_value, start, end) = parse_attr_value(after);
    let Some(uri_value) = uri_value else {
        return Ok(Some(line.to_string()));
    };

    let resolved = base.join(&uri_value)?;
    let proxied = format!(
        "{}{}?url={}",
        proxy_origin,
        proxy_path,
        urlencoding::encode(resolved.as_str())
    );

    // Rebuild the line replacing only the URI value portion in `after`
    let mut new_after = String::new();
    new_after.push_str(&after[..start]);
    new_after.push('"');
    new_after.push_str(&proxied);
    new_after.push('"');
    new_after.push_str(&after[end..]);

    Ok(Some(format!("{}{}", &line[..(pos + 4)], new_after)))
}

fn parse_attr_value(s: &str) -> (Option<String>, usize, usize) {
    // Returns (value, start_idx, end_idx) relative to input `s`
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return (None, 0, 0);
    }
    if bytes[0] == b'"' || bytes[0] == b'\'' {
        let quote = bytes[0];
        let mut i = 1;
        while i < bytes.len() {
            if bytes[i] == quote {
                let val = &s[1..i];
                return (Some(val.to_string()), 0, i + 1);
            }
            i += 1;
        }
        return (None, 0, 0);
    }

    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            let val = &s[..i];
            return (Some(val.to_string()), 0, i);
        }
        i += 1;
    }
    (Some(s.to_string()), 0, s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY: &str = "https://proxy.test";
    const PATH: &str = "/stream";

    fn rewrite(input: &str, base: &str) -> String {
        rewrite_m3u8(input, base, PROXY, PATH, 30.0).expect("rewrite should succeed")
    }

    #[test]
    fn removes_segments_between_cue_out_and_cue_in() {
        let input = "\
#EXTM3U
#EXT-X-VERSION:3
#EXTINF:6.0,
content0.ts
#EXT-X-CUE-OUT:12.0
#EXT-X-DISCONTINUITY
#EXTINF:6.0,
http://ads.example.com/ad0.ts
#EXTINF:6.0,
http://ads.example.com/ad1.ts
#EXT-X-CUE-IN
#EXTINF:6.0,
content1.ts
";
        let out = rewrite(input, "https://src.example.com/playlist.m3u8");

        // Ad segments and their domain must be gone.
        assert!(!out.contains("ad0.ts"), "ad0 should be removed:\n{out}");
        assert!(!out.contains("ad1.ts"), "ad1 should be removed:\n{out}");
        assert!(!out.contains("ads.example.com"), "ad domain leaked:\n{out}");

        // The CUE markers themselves must not survive.
        assert!(!out.contains("CUE-OUT"), "CUE-OUT leaked:\n{out}");
        assert!(!out.contains("CUE-IN"), "CUE-IN leaked:\n{out}");

        // Real content survives and is still proxied.
        assert!(out.contains("content0.ts"), "content0 missing:\n{out}");
        assert!(out.contains("content1.ts"), "content1 missing:\n{out}");
    }

    #[test]
    fn inserts_single_discontinuity_at_ad_splice() {
        let input = "\
#EXTM3U
#EXTINF:6.0,
content0.ts
#EXT-X-CUE-OUT:6.0
#EXTINF:6.0,
ad0.ts
#EXT-X-CUE-IN
#EXT-X-DISCONTINUITY
#EXTINF:6.0,
content1.ts
";
        let out = rewrite(input, "https://src.example.com/playlist.m3u8");
        let count = out.matches("#EXT-X-DISCONTINUITY").count();
        assert_eq!(count, 1, "expected exactly one discontinuity:\n{out}");
    }

    #[test]
    fn handles_cue_out_without_duration() {
        let input = "\
#EXTM3U
#EXTINF:6.0,
content0.ts
#EXT-X-CUE-OUT
#EXTINF:6.0,
ad0.ts
#EXT-X-CUE-IN
#EXTINF:6.0,
content1.ts
";
        let out = rewrite(input, "https://src.example.com/playlist.m3u8");
        assert!(!out.contains("ad0.ts"), "ad0 should be removed:\n{out}");
        assert!(out.contains("content1.ts"), "content1 missing:\n{out}");
    }

    // ikzy 形态：广告分片在另一个视频路径下，正片本身也被不连续标记切成多段
    // （含很短的正片段）。规则①按路径删广告，且必须保留短正片段不被规则②误杀。
    #[test]
    fn drops_off_path_ad_pods_and_keeps_short_content_chunks() {
        let input = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:3,
https://cdn.test/V1/hls/c0.ts
#EXTINF:3,
https://cdn.test/V1/hls/c1.ts
#EXTINF:3,
https://cdn.test/V1/hls/c2.ts
#EXT-X-DISCONTINUITY
#EXT-X-KEY:METHOD=NONE
#EXTINF:3,
/ads/V2/hls/a0.ts
#EXTINF:3,
/ads/V2/hls/a1.ts
#EXT-X-DISCONTINUITY
#EXTINF:3,
https://cdn.test/V1/hls/c3.ts
#EXT-X-DISCONTINUITY
#EXTINF:3,
/ads/V2/hls/a2.ts
#EXT-X-DISCONTINUITY
#EXTINF:3,
https://cdn.test/V1/hls/c4.ts
#EXTINF:3,
https://cdn.test/V1/hls/c5.ts
#EXTINF:3,
https://cdn.test/V1/hls/c6.ts
#EXT-X-DISCONTINUITY
#EXTINF:3,
/ads/V2/hls/a3.ts
#EXT-X-ENDLIST
";
        let out = strip_ad_runs(&strip_cue_breaks(input), 30.0);

        for ad in ["a0.ts", "a1.ts", "a2.ts", "a3.ts", "/ads/V2"] {
            assert!(!out.contains(ad), "ad `{ad}` should be removed:\n{out}");
        }
        // Every content segment survives — including the 1-segment chunk c3.
        for c in [
            "c0.ts", "c1.ts", "c2.ts", "c3.ts", "c4.ts", "c5.ts", "c6.ts",
        ] {
            assert!(out.contains(c), "content `{c}` missing:\n{out}");
        }
        // The terminator lived inside the trailing ad pod; it must be preserved.
        assert!(out.contains("#EXT-X-ENDLIST"), "ENDLIST dropped:\n{out}");
    }

    // vip.ffzy 形态：打包器每 ~20s 插一个 DISCONTINUITY，正片本身被切成大量
    // 5 片/~20s 的短块，且每块逐段时长各不相同（无重复）。这些都是正片，
    // 一片都不能删——靠「重复签名」而非「短时长」区分广告。
    #[test]
    fn keeps_many_unique_short_chunks_chopped_by_discontinuities() {
        let mut input = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        // 25 个互不相同的 5 片短块（每块 ~20s，全部唯一）。
        for blk in 0..25 {
            input.push_str("#EXT-X-DISCONTINUITY\n");
            for seg in 0..5 {
                // 用 blk/seg 制造各不相同的时长，确保无两块签名相同。
                let dur = 3.0 + (blk as f64) * 0.07 + (seg as f64) * 0.31;
                input.push_str(&format!("#EXTINF:{dur:.3},\nseg_{blk}_{seg}.ts\n"));
            }
        }
        input.push_str("#EXT-X-ENDLIST\n");

        let out = strip_ad_runs(&strip_cue_breaks(&input), 30.0);
        // 全部 125 个正片分片必须保留。
        for blk in 0..25 {
            for seg in 0..5 {
                let name = format!("seg_{blk}_{seg}.ts");
                assert!(
                    out.contains(&name),
                    "content `{name}` wrongly removed:\n{out}"
                );
            }
        }
    }

    // super.ffzy 形态：同一广告片(逐段时长完全相同)插入 2 次 → 重复签名识别。
    #[test]
    fn drops_repeated_same_path_ad_pods() {
        let content_block = |tag: &str, out: &mut String| {
            for i in 0..40 {
                out.push_str(&format!("#EXTINF:6.0,\n{tag}{i}.ts\n"));
            }
        };
        let ad_block = |idx: usize, out: &mut String| {
            for (i, d) in [4.866667, 3.333333, 6.366667, 1.733333, 3.333333]
                .iter()
                .enumerate()
            {
                // 不同的文件名但完全相同的时长（广告换皮再编码的真实特征）。
                out.push_str(&format!("#EXTINF:{d},\nad_{idx}_{i}.ts\n"));
            }
        };
        let mut input = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-DISCONTINUITY\n");
        content_block("ca", &mut input);
        input.push_str("#EXT-X-DISCONTINUITY\n");
        ad_block(0, &mut input);
        input.push_str("#EXT-X-DISCONTINUITY\n");
        content_block("cb", &mut input);
        input.push_str("#EXT-X-DISCONTINUITY\n");
        ad_block(1, &mut input);
        input.push_str("#EXT-X-DISCONTINUITY\n");
        content_block("cc", &mut input);
        input.push_str("#EXT-X-ENDLIST\n");

        let out = strip_ad_runs(&strip_cue_breaks(&input), 30.0);
        for idx in 0..2 {
            for i in 0..5 {
                assert!(
                    !out.contains(&format!("ad_{idx}_{i}.ts")),
                    "ad_{idx}_{i} should be removed:\n{out}"
                );
            }
        }
        assert!(out.contains("ca0.ts") && out.contains("cb0.ts") && out.contains("cc0.ts"));
        assert!(out.contains("#EXT-X-ENDLIST"));
    }

    // vip.ffzy 真实陷阱：均匀打包用整数时长，多个正片块碰巧签名相同
    // （如 8 个「4.0×5」块）。绝不能因「签名重复」就删——它们是正片，
    // 且无大邻居（最大块也才 ~20 片）。必须 0 删除。
    #[test]
    fn keeps_content_blocks_that_share_round_duration_signatures() {
        let mut input = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        // 30 个块，其中 8 个是「4.0×5」相同签名，其余唯一；最大块 20 片(<25)。
        for blk in 0..30 {
            input.push_str("#EXT-X-DISCONTINUITY\n");
            if blk % 4 == 0 {
                for seg in 0..5 {
                    input.push_str(&format!("#EXTINF:4.000,\nr_{blk}_{seg}.ts\n"));
                }
            } else if blk == 7 {
                for seg in 0..20 {
                    input.push_str(&format!("#EXTINF:4.0,\nbig_{seg}.ts\n"));
                }
            } else {
                for seg in 0..5 {
                    let dur = 3.0 + (blk as f64) * 0.11 + (seg as f64) * 0.37;
                    input.push_str(&format!("#EXTINF:{dur:.3},\nu_{blk}_{seg}.ts\n"));
                }
            }
        }
        input.push_str("#EXT-X-ENDLIST\n");

        let before = input.matches(".ts").count();
        let out = strip_ad_runs(&strip_cue_breaks(&input), 30.0);
        let after = out.matches(".ts").count();
        assert_eq!(
            after, before,
            "content wrongly removed: {before} -> {after}\n{out}"
        );
    }

    // super.ffzy 形态：单次插入的同路径广告(不重复)，但短块夹在两个远大于它的
    // 正片块之间(89片 | 5片广告 | 28片)。靠「邻居异常巨大」识别(规则③)。
    #[test]
    fn drops_single_short_ad_between_much_larger_content() {
        let big = |tag: &str, n: usize, out: &mut String| {
            for i in 0..n {
                out.push_str(&format!("#EXTINF:4.0,\n{tag}{i}.ts\n"));
            }
        };
        let mut input = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-DISCONTINUITY\n");
        big("head", 89, &mut input);
        input.push_str("#EXT-X-DISCONTINUITY\n");
        // 单个 5 片 / ~20s 广告 pod（签名唯一，不重复）。
        for (i, d) in [4.866667, 3.333333, 6.366667, 1.733333, 3.333333]
            .iter()
            .enumerate()
        {
            input.push_str(&format!("#EXTINF:{d},\nadx{i}.ts\n"));
        }
        input.push_str("#EXT-X-DISCONTINUITY\n");
        big("tail", 28, &mut input);
        input.push_str("#EXT-X-ENDLIST\n");

        let out = strip_ad_runs(&strip_cue_breaks(&input), 30.0);
        for i in 0..5 {
            assert!(
                !out.contains(&format!("adx{i}.ts")),
                "single ad pod should be removed:\n{out}"
            );
        }
        assert!(out.contains("head0.ts") && out.contains("tail0.ts"));
    }

    // 无路径信号、且某个内部区段时长超过阈值 → 当正片保留（不误杀）。
    #[test]
    fn keeps_long_interior_break_between_discontinuities() {
        let mut input = String::from("#EXTM3U\n#EXT-X-DISCONTINUITY\n");
        for i in 0..4 {
            input.push_str(&format!("#EXTINF:6.0,\nhead{i}.ts\n"));
        }
        input.push_str("#EXT-X-DISCONTINUITY\n");
        // Interior run of 40s (> 30s threshold): legitimate content, must survive.
        for i in 0..8 {
            input.push_str(&format!("#EXTINF:5.0,\nmid{i}.ts\n"));
        }
        input.push_str("#EXT-X-DISCONTINUITY\n");
        for i in 0..4 {
            input.push_str(&format!("#EXTINF:6.0,\ntail{i}.ts\n"));
        }
        input.push_str("#EXT-X-ENDLIST\n");

        let out = strip_ad_runs(&strip_cue_breaks(&input), 30.0);
        for i in 0..8 {
            assert!(
                out.contains(&format!("mid{i}.ts")),
                "mid{i} wrongly removed:\n{out}"
            );
        }
    }

    #[test]
    fn playlist_without_ads_keeps_all_segments_proxied() {
        let input = "\
#EXTM3U
#EXT-X-VERSION:3
#EXTINF:6.0,
seg0.ts
#EXTINF:6.0,
seg1.ts
#EXT-X-ENDLIST
";
        let out = rewrite(input, "https://src.example.com/playlist.m3u8");
        assert!(out.contains("seg0.ts"), "seg0 missing:\n{out}");
        assert!(out.contains("seg1.ts"), "seg1 missing:\n{out}");
        // Both segments rewritten through the proxy.
        assert_eq!(
            out.matches(&format!("{PROXY}{PATH}?url=")).count(),
            2,
            "both segments should be proxied:\n{out}"
        );
    }
}
