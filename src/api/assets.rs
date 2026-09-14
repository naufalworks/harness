//! Static asset delivery: cache validators and content-encoding negotiation.
//!
//! Moved verbatim from `main.rs` by P12-T01; behaviour is unchanged.
use crate::BUILD_COMMIT;
use axum::{
    body::Bytes,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
// P11-T05: the static assets are compiled into this binary, so the build commit identifies
// their bytes exactly and is a sound strong validator. `index.html` requests `/app.js` and
// `/style.css` with a `?v=<commit>` fingerprint, which is why those two may be cached
// immutably: a new build changes the URL. The document itself must revalidate on every load,
// otherwise a cached page would keep pointing at a retired build and trip "Stale UI detected".
const ASSET_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const ASSET_REVALIDATE: &str = "no-cache";
// P11-T06: gzip bytes produced by build.rs. Empty means the build had no gzip available, in
// which case negotiation is skipped and identity bytes are served.
pub(crate) const INDEX_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));
pub(crate) const APP_JS_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.js.gz"));
pub(crate) const STYLE_CSS_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/style.css.gz"));
// Accept only an explicit, non-rejected gzip token. `gzip;q=0` means "do not send gzip", and a
// client that says nothing must keep receiving identity bytes.
fn accepts_gzip(request_headers: &HeaderMap) -> bool {
    request_headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|candidate| {
                let mut parts = candidate.split(';');
                let token = parts.next().unwrap_or("").trim();
                if !token.eq_ignore_ascii_case("gzip") && token != "*" {
                    return false;
                }
                !parts.any(|parameter| {
                    let parameter = parameter.trim().replace(' ', "");
                    parameter == "q=0" || parameter.starts_with("q=0.0")
                })
            })
        })
}
fn asset(
    request_headers: &HeaderMap,
    content_type: &'static str,
    cache_control: &'static str,
    name: &str,
    body: String,
    gzipped: &'static [u8],
) -> Response {
    // Encodings are different representations, so they need different validators and a `Vary`,
    // otherwise a shared cache could hand gzip bytes to a client that cannot decode them.
    let gzip = !gzipped.is_empty() && accepts_gzip(request_headers);
    let etag = if gzip {
        format!("\"{BUILD_COMMIT}-{name}-gzip\"")
    } else {
        format!("\"{BUILD_COMMIT}-{name}\"")
    };
    // A conditional request may send several validators, and a proxy may weaken them.
    let unchanged = request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|candidate| candidate.trim().trim_start_matches("W/") == etag)
        });
    let validators = [
        (header::CACHE_CONTROL, cache_control.to_string()),
        (header::ETAG, etag),
        (header::VARY, header::ACCEPT_ENCODING.to_string()),
    ];
    if unchanged {
        return (StatusCode::NOT_MODIFIED, validators).into_response();
    }
    if gzip {
        return (
            validators,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_ENCODING, "gzip"),
            ],
            Bytes::from_static(gzipped),
        )
            .into_response();
    }
    (validators, [(header::CONTENT_TYPE, content_type)], body).into_response()
}
pub(crate) async fn index(request_headers: HeaderMap) -> Response {
    asset(
        &request_headers,
        "text/html; charset=utf-8",
        ASSET_REVALIDATE,
        "index.html",
        include_str!("../../static/index.html").replace("__HARNESS_BUILD_COMMIT__", BUILD_COMMIT),
        INDEX_GZ,
    )
}
pub(crate) async fn js(request_headers: HeaderMap) -> Response {
    asset(
        &request_headers,
        "text/javascript; charset=utf-8",
        ASSET_IMMUTABLE,
        "app.js",
        include_str!("../../static/app.js").replace("__HARNESS_BUILD_COMMIT__", BUILD_COMMIT),
        APP_JS_GZ,
    )
}
pub(crate) async fn css(request_headers: HeaderMap) -> Response {
    asset(
        &request_headers,
        "text/css; charset=utf-8",
        ASSET_IMMUTABLE,
        "style.css",
        include_str!("../../static/style.css").to_string(),
        STYLE_CSS_GZ,
    )
}
