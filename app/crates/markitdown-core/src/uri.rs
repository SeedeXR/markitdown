//! Input acquisition: local paths and `file:` / `data:` / `http(s):` URIs.
//! Port of `packages/markitdown/src/markitdown/_uri_utils.py`.

use crate::{ConvertError, StreamInfo};
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};

/// Fetch the bytes behind `src` (path or URI) plus everything we learned
/// about the stream along the way.
pub fn read_source(src: &str) -> Result<(Vec<u8>, StreamInfo), ConvertError> {
    if let Some(rest) = src.strip_prefix("data:") {
        return read_data_uri(rest, src);
    }
    if let Some(rest) = src.strip_prefix("file://") {
        let path = file_uri_to_path(rest)?;
        return read_path(&path);
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return read_http(src);
    }
    read_path(Path::new(src))
}

/// Read a local file into memory with path-derived stream info.
pub fn read_path(path: &Path) -> Result<(Vec<u8>, StreamInfo), ConvertError> {
    let data = std::fs::read(path)?;
    let mut info = StreamInfo::new();
    info.local_path = Some(path.to_path_buf());
    if let Some(name) = path.file_name() {
        info.filename = Some(name.to_string_lossy().into_owned());
    }
    Ok((data, info))
}

fn file_uri_to_path(rest: &str) -> Result<PathBuf, ConvertError> {
    // file://host/path — we only support empty/localhost hosts.
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => ("", rest),
    };
    if !host.is_empty() && host != "localhost" {
        return Err(ConvertError::InvalidInput(format!(
            "unsupported file URI host: {host}"
        )));
    }
    let decoded = percent_decode_str(path)
        .decode_utf8()
        .map_err(|e| ConvertError::InvalidInput(format!("bad file URI encoding: {e}")))?;
    Ok(PathBuf::from(decoded.into_owned()))
}

fn read_data_uri(rest: &str, full: &str) -> Result<(Vec<u8>, StreamInfo), ConvertError> {
    // data:[<mediatype>][;base64],<data>
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| ConvertError::InvalidInput("malformed data: URI (no comma)".into()))?;

    let mut is_base64 = false;
    let mut mimetype: Option<String> = None;
    let mut charset: Option<String> = None;
    for (i, part) in meta.split(';').enumerate() {
        let part = part.trim();
        if part.eq_ignore_ascii_case("base64") {
            is_base64 = true;
        } else if let Some(cs) = part.strip_prefix("charset=") {
            charset = Some(cs.to_ascii_lowercase());
        } else if i == 0 && !part.is_empty() {
            mimetype = Some(part.to_ascii_lowercase());
        }
    }

    let data = if is_base64 {
        base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|e| ConvertError::InvalidInput(format!("bad base64 in data URI: {e}")))?
    } else {
        percent_decode_str(payload).collect()
    };

    let mut info = StreamInfo::new().with_url(full);
    info.mimetype = mimetype;
    info.charset = charset;
    Ok((data, info))
}

/// Environment override for the outbound User-Agent.
#[cfg(feature = "net")]
pub const USER_AGENT_ENV: &str = "MARKITDOWN_USER_AGENT";

/// The User-Agent used for page fetches.
///
/// The default names this tool honestly but leads with the conventional
/// `Mozilla/5.0` token, because a meaningful number of sites reject requests
/// from unrecognized agents outright — which shows up as an unexplained 403 on
/// a URL that opens fine in a browser. Override with `MARKITDOWN_USER_AGENT`.
#[cfg(feature = "net")]
fn user_agent() -> String {
    std::env::var(USER_AGENT_ENV).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
        concat!(
            "Mozilla/5.0 (compatible; markitdown-rs/",
            env!("CARGO_PKG_VERSION"),
            "; +https://github.com/microsoft/markitdown)"
        )
        .to_string()
    })
}

/// Client-side redirect hops to follow. Enough for the usual "page moved" stub
/// chain, bounded so a pair of pages pointing at each other terminates.
#[cfg(feature = "net")]
const MAX_META_REFRESH_HOPS: usize = 3;

/// Largest body still treated as a redirect stub. A full article that happens
/// to carry a `meta refresh` keeps its own content.
#[cfg(feature = "net")]
const MAX_REFRESH_STUB_BYTES: usize = 32 * 1024;

/// Fetch `url`, transparently following `<meta http-equiv="refresh">` stubs.
#[cfg(feature = "net")]
fn read_http(url: &str) -> Result<(Vec<u8>, StreamInfo), ConvertError> {
    let mut current = url.to_string();
    let mut seen = vec![current.clone()];
    for _ in 0..MAX_META_REFRESH_HOPS {
        let (data, info, final_url) = read_http_once(&current)?;
        // Resolve against where the HTTP redirect chain actually LANDED, not
        // what we asked for; otherwise a relative target after a cross-host
        // 301 is joined onto the wrong origin.
        let current_base = final_url.unwrap_or_else(|| current.clone());
        let is_html = info
            .mimetype
            .as_deref()
            .is_some_and(|m| m.starts_with("text/html") || m.starts_with("application/xhtml"));
        if !is_html || data.len() > MAX_REFRESH_STUB_BYTES {
            return Ok((data, info));
        }
        let body = crate::text::decode_text(&data, &info);
        let Some(target) = crate::converters::html::meta_refresh_target(&body) else {
            return Ok((data, info));
        };
        let Some(next) = join_url(&current_base, &target) else {
            return Ok((data, info));
        };
        // Only http(s); never chase javascript:/data:/file: out of a page.
        // The fetch itself stays behind the SSRF-guarded agent either way.
        if !(next.starts_with("http://") || next.starts_with("https://")) || seen.contains(&next) {
            return Ok((data, info));
        }
        seen.push(next.clone());
        current = next;
    }
    read_http_once(&current).map(|(d, i, _)| (d, i))
}

/// Resolve `target` against `base`: absolute, scheme-relative (`//host/x`),
/// root-relative (`/x`), query/fragment-only (`?q=1`, `#f`) and plain relative
/// references, with `.`/`..` segments collapsed. Enough for redirect stubs
/// without pulling in a URL-parsing dependency.
#[cfg(feature = "net")]
fn join_url(base: &str, target: &str) -> Option<String> {
    // Scheme comparison is case-insensitive per RFC 3986. Getting this wrong
    // treats "HTTP://evil.test/x" as a relative path and builds a nonsense URL
    // that still looks absolute to a `starts_with` check downstream.
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(target.to_string());
    }
    // Any other explicit scheme (javascript:, data:, mailto:) is not ours to
    // resolve; reject rather than mangle it into a path.
    if let Some(colon) = lower.find(':') {
        let is_scheme = !lower[..colon].is_empty()
            && lower[..colon]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && lower[..colon].starts_with(|c: char| c.is_ascii_alphabetic())
            && !lower[..colon].contains('/');
        if is_scheme {
            return None;
        }
    }

    let scheme_end = base.find("://")? + 3;
    let (scheme, rest) = base.split_at(scheme_end);
    if let Some(hostless) = target.strip_prefix("//") {
        return Some(format!("{scheme}{hostless}"));
    }
    // Authority and path of the base, dropping its own query/fragment.
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // Query- or fragment-only targets replace that component and keep the
    // whole base path, including its final segment.
    if target.starts_with('?') || target.starts_with('#') {
        return Some(format!("{scheme}{authority}{path}{target}"));
    }
    let joined = if let Some(abs) = target.strip_prefix('/') {
        format!("/{abs}")
    } else {
        let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(1)];
        format!("{dir}{target}")
    };
    Some(format!("{scheme}{authority}{}", normalize_path(&joined)))
}

/// Collapse `.` and `..` segments, leaving any query/fragment untouched.
#[cfg(feature = "net")]
fn normalize_path(path: &str) -> String {
    let split = path.find(['?', '#']).unwrap_or(path.len());
    let (raw, suffix) = path.split_at(split);
    let trailing_slash = raw.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut joined = String::from("/");
    joined.push_str(&out.join("/"));
    if trailing_slash && !out.is_empty() {
        joined.push('/');
    }
    joined.push_str(suffix);
    joined
}

/// Fetch one URL. Also returns the URL the HTTP redirect chain finally landed
/// on, which is the correct base for resolving anything inside the response.
#[cfg(feature = "net")]
#[allow(clippy::type_complexity)]
fn read_http_once(url: &str) -> Result<(Vec<u8>, StreamInfo, Option<String>), ConvertError> {
    // crate::net::agent() applies a request timeout and an SSRF guard that
    // refuses private/loopback/link-local targets on every redirect hop.
    let mut resp = crate::net::agent()
        .get(url)
        .header("User-Agent", user_agent())
        // Without an Accept header a fair number of sites serve an API/JSON
        // representation — or refuse outright — instead of the page a browser
        // would get.
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en;q=0.9,*;q=0.5")
        .call()
        .map_err(|e| ConvertError::Network(format!("could not fetch {url}: {e}")))?;

    // Where the redirect chain actually landed; used as the base for resolving
    // links found inside the response.
    let final_url = {
        use ureq::ResponseExt as _;
        let landed = resp.get_uri().to_string();
        (landed != url).then_some(landed)
    };

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let data = resp
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024) // hard cap: 512 MiB
        .read_to_vec()
        .map_err(|e| ConvertError::Network(e.to_string()))?;

    let mut info = StreamInfo::new().with_url(url);
    if let Some(ct) = content_type {
        let mut parts = ct.split(';');
        if let Some(mt) = parts.next() {
            let mt = mt.trim();
            if !mt.is_empty() {
                info.mimetype = Some(mt.to_ascii_lowercase());
            }
        }
        for p in parts {
            if let Some(cs) = p.trim().strip_prefix("charset=") {
                info.charset = Some(cs.trim_matches('"').to_ascii_lowercase());
            }
        }
    }
    // Filename from the last path segment, for extension hints. Taken from the
    // final URL so a redirect to `.../report.pdf` still hints the extension.
    let for_name = final_url.as_deref().unwrap_or(url);
    if let Some(path_part) = for_name
        .splitn(4, '/')
        .nth(3)
        .map(|p| p.split(['?', '#']).next().unwrap_or(""))
    {
        if let Some(seg) = path_part.rsplit('/').next() {
            if !seg.is_empty() {
                info.filename = Some(seg.to_string());
            }
        }
    }
    Ok((data, info, final_url))
}

#[cfg(not(feature = "net"))]
fn read_http(_url: &str) -> Result<(Vec<u8>, StreamInfo), ConvertError> {
    Err(ConvertError::MissingDependency(
        "http(s) inputs require the `net` feature".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_uri_base64() {
        let (data, info) = read_source("data:text/plain;base64,aGVsbG8=").unwrap();
        assert_eq!(data, b"hello");
        assert_eq!(info.mimetype.as_deref(), Some("text/plain"));
    }

    #[test]
    fn data_uri_percent_encoded() {
        let (data, info) = read_source("data:text/plain;charset=utf-8,hi%20there").unwrap();
        assert_eq!(data, b"hi there");
        assert_eq!(info.charset.as_deref(), Some("utf-8"));
    }

    #[test]
    fn file_uri_decodes_percent_escapes() {
        let p = file_uri_to_path("/tmp/my%20file.txt").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/my file.txt"));
    }

    #[test]
    fn rejects_remote_file_uri_host() {
        assert!(file_uri_to_path("evilhost/share/x").is_err());
    }

    #[cfg(feature = "net")]
    #[test]
    fn join_url_resolves_every_reference_form() {
        let base = "https://example.com/docs/guide/page.html";
        // Absolute
        assert_eq!(
            join_url(base, "https://other.test/x").as_deref(),
            Some("https://other.test/x")
        );
        // Scheme-relative
        assert_eq!(
            join_url(base, "//cdn.test/y").as_deref(),
            Some("https://cdn.test/y")
        );
        // Root-relative
        assert_eq!(
            join_url(base, "/top").as_deref(),
            Some("https://example.com/top")
        );
        // Plain relative resolves against the containing directory.
        assert_eq!(
            join_url(base, "next.html").as_deref(),
            Some("https://example.com/docs/guide/next.html")
        );
        // A base with no path still yields a valid URL.
        assert_eq!(
            join_url("https://example.com", "a.html").as_deref(),
            Some("https://example.com/a.html")
        );
        // Query strings on the base must not leak into the resolved path.
        assert_eq!(
            join_url("https://example.com/a/b?q=1", "c.html").as_deref(),
            Some("https://example.com/a/c.html")
        );
    }

    #[cfg(feature = "net")]
    #[test]
    fn join_url_rejects_or_normalizes_hostile_and_odd_targets() {
        let base = "https://example.com/a/b.html";
        // Scheme matching is case-insensitive; treating this as relative would
        // build a junk URL that still passes an "is it https://" check.
        assert_eq!(
            join_url(base, "HTTP://evil.test/x").as_deref(),
            Some("HTTP://evil.test/x")
        );
        // Non-http schemes are not ours to resolve.
        assert_eq!(join_url(base, "javascript:alert(1)"), None);
        assert_eq!(join_url(base, "data:text/html,x"), None);
        assert_eq!(join_url(base, "mailto:a@b.c"), None);
        // Query/fragment-only targets keep the full base path.
        assert_eq!(
            join_url(base, "?q=1").as_deref(),
            Some("https://example.com/a/b.html?q=1")
        );
        assert_eq!(
            join_url(base, "#f").as_deref(),
            Some("https://example.com/a/b.html#f")
        );
        // Dot segments collapse.
        assert_eq!(
            join_url("https://example.com/a/b/c.html", "../x").as_deref(),
            Some("https://example.com/a/x")
        );
        assert_eq!(
            join_url("https://example.com/a/b/c.html", "./d/../e").as_deref(),
            Some("https://example.com/a/b/e")
        );
        // `..` can never climb above the root.
        assert_eq!(
            join_url("https://example.com/a.html", "../../../etc").as_deref(),
            Some("https://example.com/etc")
        );
    }

    #[cfg(feature = "net")]
    #[test]
    fn user_agent_is_overridable_and_never_empty() {
        // std::env::set_var is process-global, so this test must not run
        // alongside the others in this binary; it holds a dedicated lock.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = std::env::var_os(USER_AGENT_ENV);

        std::env::remove_var(USER_AGENT_ENV);
        assert!(user_agent().contains("markitdown-rs"));
        // A blank override must not produce a header-less request.
        std::env::set_var(USER_AGENT_ENV, "   ");
        assert!(user_agent().contains("markitdown-rs"));
        std::env::set_var(USER_AGENT_ENV, "MyAgent/1.0");
        assert_eq!(user_agent(), "MyAgent/1.0");

        match restore {
            Some(v) => std::env::set_var(USER_AGENT_ENV, v),
            None => std::env::remove_var(USER_AGENT_ENV),
        }
    }
}
