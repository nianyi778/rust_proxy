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

    if let Ok(origin) = target.origin().ascii_serialization().parse::<HeaderValue>() {
        headers.insert(hyper::header::ORIGIN, origin);
    }
    if let Ok(referer) =
        format!("{}/", target.origin().ascii_serialization()).parse::<HeaderValue>()
    {
        headers.insert(hyper::header::REFERER, referer);
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
        let rewritten = rewrite_m3u8(&text, &q.url, &proxy_origin, proxy_path).map_err(|e| {
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
) -> anyhow::Result<String> {
    let base = Url::parse(base_url)?;
    let mut out = Vec::new();

    for line in content.lines() {
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
