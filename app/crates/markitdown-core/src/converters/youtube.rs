//! YouTube watch-page converter. Port of `_youtube_converter.py`.
//!
//! Accepts HTML only when the source URL is a YouTube watch page. Extracts the
//! title, metadata (views / keywords / runtime) and description from meta tags
//! plus the embedded `ytInitialData` JSON.
//!
//! Transcripts are fetched natively (no Python needed) when the crate is built
//! with the `net` feature: we read the `captionTracks[]` out of the embedded
//! `ytInitialPlayerResponse` JSON — exactly what `youtube-transcript-api` does —
//! pick a track (preferring an English, manually-created one), GET its
//! `baseUrl` timedtext endpoint, and parse the `<text>` lines. If captions are
//! absent or the build is offline, the result is flagged degraded so the
//! Python engine (which ships `youtube-transcript-api`) can try instead.
use super::html::extract_title_doc;
use crate::{text::decode_text, Converter, ConvertError, ConvertOptions, ConvertResult, StreamInfo};
use scraper::{Html, Selector};
use std::collections::HashMap;

pub struct YouTubeConverter;

impl Converter for YouTubeConverter {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        let url = info.url.as_deref().unwrap_or("");
        // Mirror Python's unescaping of `\?` / `\=` before the prefix check.
        let url = url.replace("\\?", "?").replace("\\=", "=");
        url.starts_with("https://www.youtube.com/watch?")
    }

    fn convert(
        &self,
        data: &[u8],
        info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        let html = decode_text(data, info);
        let doc = Html::parse_document(&html);

        let mut metadata: HashMap<String, String> = HashMap::new();

        // <title>
        if let Some(t) = extract_title_doc(&doc) {
            metadata.insert("title".to_string(), t);
        }

        // <meta> tags keyed by itemprop / property / name.
        if let Ok(meta_sel) = Selector::parse("meta") {
            for meta in doc.select(&meta_sel) {
                let attrs = meta.value();
                let content = match attrs.attr("content") {
                    Some(c) if !c.is_empty() => c,
                    _ => continue,
                };
                for key_attr in ["itemprop", "property", "name"] {
                    if let Some(key) = attrs.attr(key_attr) {
                        if !key.is_empty() {
                            metadata
                                .entry(key.to_string())
                                .or_insert_with(|| content.to_string());
                        }
                        break;
                    }
                }
            }
        }

        // Description from ytInitialData (best-effort).
        if !metadata.contains_key("description") {
            if let Some(desc) = description_from_yt_initial_data(&doc) {
                metadata.insert("description".to_string(), desc);
            }
        }

        // Build the page.
        let mut webpage_text = String::from("# YouTube\n");

        let title = get(&metadata, &["title", "og:title", "name"]).unwrap_or_default();
        if !title.is_empty() {
            webpage_text.push_str(&format!("\n## {title}\n"));
        }

        let mut stats = String::new();
        if let Some(views) = get(&metadata, &["interactionCount"]) {
            stats.push_str(&format!("- **Views:** {views}\n"));
        }
        if let Some(keywords) = get(&metadata, &["keywords"]) {
            stats.push_str(&format!("- **Keywords:** {keywords}\n"));
        }
        if let Some(runtime) = get(&metadata, &["duration"]) {
            stats.push_str(&format!("- **Runtime:** {runtime}\n"));
        }
        if !stats.is_empty() {
            webpage_text.push_str(&format!("\n### Video Metadata\n{stats}\n"));
        }

        if let Some(description) = get(&metadata, &["description", "og:description"]) {
            webpage_text.push_str(&format!("\n### Description\n{description}\n"));
        }

        // Transcript: read captionTracks from ytInitialPlayerResponse and fetch
        // the timedtext endpoint (when built with `net`). Falls back to a
        // degraded result so the Python engine can try when we can't.
        let mut degraded = false;
        match fetch_transcript(&doc, opts) {
            Some(transcript) => {
                webpage_text.push_str(&format!("\n### Transcript\n{transcript}\n"));
            }
            None => {
                webpage_text.push_str(
                    "\n<!-- Transcript unavailable (no captions found, or an offline build); the Python engine may fetch it. -->\n",
                );
                degraded = true;
            }
        }

        let mut result = ConvertResult::new(webpage_text);
        if degraded {
            result = result.with_degraded();
        }
        if !title.is_empty() {
            result = result.with_title(title);
        }
        Ok(result)
    }
}

/// First non-empty metadata value matching any of `keys`.
fn get(metadata: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(v) = metadata.get(*k) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Find a `<script>`-embedded JSON object assigned to `marker` (e.g.
/// `var ytInitialData = {…};`) and parse it into a [`serde_json::Value`].
fn json_blob(doc: &Html, marker: &str) -> Option<serde_json::Value> {
    let script_sel = Selector::parse("script").ok()?;
    for script in doc.select(&script_sel) {
        let text: String = script.text().collect();
        let Some(start) = text.find(marker) else {
            continue;
        };
        let after = &text[start..];
        let Some(brace) = after.find('{') else {
            continue;
        };
        if let Some(json_str) = balanced_json(&after[brace..]) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
                return Some(value);
            }
        }
    }
    None
}

/// Best-effort extraction of `attributedDescriptionBodyText.content` from the
/// embedded `ytInitialData` JSON inside a `<script>` tag.
fn description_from_yt_initial_data(doc: &Html) -> Option<String> {
    let value = json_blob(doc, "ytInitialData")?;
    let node = find_key(&value, "attributedDescriptionBodyText")?;
    node.get("content")?.as_str().map(str::to_string)
}

/// A single caption track from `ytInitialPlayerResponse`.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
struct CaptionTrack {
    base_url: String,
    lang: String,
    /// `true` when auto-generated (`kind == "asr"`).
    asr: bool,
}

/// Read the `captionTracks[]` array out of `ytInitialPlayerResponse`.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn caption_tracks(doc: &Html) -> Vec<CaptionTrack> {
    let Some(value) = json_blob(doc, "ytInitialPlayerResponse") else {
        return Vec::new();
    };
    let Some(arr) = find_key(&value, "captionTracks").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            // serde_json already un-escapes `&` etc. in baseUrl.
            let base_url = t.get("baseUrl")?.as_str()?.to_string();
            let lang = t
                .get("languageCode")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let asr = t.get("kind").and_then(|v| v.as_str()) == Some("asr");
            Some(CaptionTrack { base_url, lang, asr })
        })
        .collect()
}

/// Pick the best track: an English manually-created one first, then any
/// English, then any manual, then whatever is available.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn choose_track(tracks: &[CaptionTrack]) -> Option<&str> {
    tracks
        .iter()
        .find(|t| t.lang.starts_with("en") && !t.asr)
        .or_else(|| tracks.iter().find(|t| t.lang.starts_with("en")))
        .or_else(|| tracks.iter().find(|t| !t.asr))
        .or_else(|| tracks.first())
        .map(|t| t.base_url.as_str())
}

/// Fetch and parse the transcript, when available and the `net` feature is on.
#[cfg(feature = "net")]
fn fetch_transcript(doc: &Html, opts: &ConvertOptions) -> Option<String> {
    let tracks = caption_tracks(doc);
    let url = choose_track(&tracks)?;
    opts.report(crate::Progress::msg("youtube", "fetching transcript…"));
    let xml = http_get_string(url)?;
    let text = parse_timedtext(&xml);
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(not(feature = "net"))]
fn fetch_transcript(_doc: &Html, _opts: &ConvertOptions) -> Option<String> {
    None
}

/// GET a URL and return its body as a UTF-8 (lossy) string. The `baseUrl` comes
/// from the (untrusted) page JSON, so this uses the SSRF-guarded + timed agent —
/// an attacker-planted caption URL can't reach internal hosts or hang us.
#[cfg(feature = "net")]
fn http_get_string(url: &str) -> Option<String> {
    let mut resp = crate::net::agent()
        .get(url)
        .header("User-Agent", concat!("markitdown-rs/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024) // transcripts are small; cap defensively
        .read_to_vec()
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Parse YouTube timedtext XML (`<transcript><text …>line</text>…`) into a
/// single space-joined string, matching the Python converter's join.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn parse_timedtext(xml: &str) -> String {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"text" => {
                in_text = true;
                cur.clear();
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"text" => {
                in_text = false;
                // timedtext double-encodes entities (`&amp;#39;`), so the XML
                // pass leaves `&#39;` etc. — decode those too.
                let line = decode_entities(cur.trim());
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            Ok(Event::Text(e)) if in_text => {
                if let Ok(t) = e.decode() {
                    cur.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(e)) if in_text => {
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    cur.push(ch);
                } else if let Ok(name) = e.decode() {
                    match name.as_ref() {
                        "amp" => cur.push('&'),
                        "lt" => cur.push('<'),
                        "gt" => cur.push('>'),
                        "quot" => cur.push('"'),
                        "apos" => cur.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    lines.join(" ")
}

/// Decode the HTML entities that survive the XML pass (`&#39;`, `&#x41;`,
/// named entities).
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'&' {
            if let Some(rel) = s[i..].find(';') {
                let ent = &s[i + 1..i + rel];
                let resolved = if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else if let Some(dec) = ent.strip_prefix('#') {
                    dec.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    match ent {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        "nbsp" => Some(' '),
                        _ => None,
                    }
                };
                if let Some(c) = resolved {
                    out.push(c);
                    i += rel + 1;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Return the substring spanning a balanced `{...}` object starting at `s[0]`.
fn balanced_json(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Recursively search a JSON value for the first object holding `key`.
fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return Some(v);
            }
            for v in map.values() {
                if let Some(found) = find_key(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                if let Some(found) = find_key(v, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_object() {
        assert_eq!(balanced_json(r#"{"a":1}rest"#), Some(r#"{"a":1}"#));
        assert_eq!(balanced_json(r#"{"a":{"b":"}"}}x"#), Some(r#"{"a":{"b":"}"}}"#));
    }

    #[test]
    fn builds_page_from_meta() {
        let html = r#"<html><head>
            <title>My Video - YouTube</title>
            <meta property="og:title" content="My Video">
            <meta name="keywords" content="rust, test">
            <meta itemprop="duration" content="PT5M">
            <meta property="og:description" content="A great video.">
        </head><body></body></html>"#;
        let info = StreamInfo::new()
            .with_url("https://www.youtube.com/watch?v=abc")
            .with_extension(".html");
        let res = YouTubeConverter
            .convert(html.as_bytes(), &info, &ConvertOptions::default())
            .unwrap();
        assert!(res.markdown.contains("# YouTube"));
        assert!(res.markdown.contains("### Description\nA great video."));
        assert!(res.markdown.contains("- **Keywords:** rust, test"));
        assert!(res.markdown.contains("- **Runtime:** PT5M"));
    }

    #[test]
    fn parses_timedtext_xml() {
        let xml = r#"<?xml version="1.0"?><transcript><text start="0" dur="1">Hello &amp;#39;world&amp;#39;</text><text start="1" dur="1">second &amp;amp; line</text></transcript>"#;
        assert_eq!(parse_timedtext(xml), "Hello 'world' second & line");
    }

    #[test]
    fn decodes_surviving_entities() {
        assert_eq!(decode_entities("a&#39;b &amp; c &#x41;"), "a'b & c A");
        assert_eq!(decode_entities("no entities"), "no entities");
    }

    #[test]
    fn picks_english_manual_track() {
        let tracks = vec![
            CaptionTrack { base_url: "asr".into(), lang: "en".into(), asr: true },
            CaptionTrack { base_url: "man".into(), lang: "en".into(), asr: false },
            CaptionTrack { base_url: "fr".into(), lang: "fr".into(), asr: false },
        ];
        assert_eq!(choose_track(&tracks), Some("man"));

        // No English: prefer a manual track over an auto-generated one.
        let tracks = vec![
            CaptionTrack { base_url: "de-asr".into(), lang: "de".into(), asr: true },
            CaptionTrack { base_url: "de".into(), lang: "de".into(), asr: false },
        ];
        assert_eq!(choose_track(&tracks), Some("de"));
        assert_eq!(choose_track(&[]), None);
    }

    #[test]
    fn reads_caption_tracks_from_player_response() {
        let html = r#"<html><body><script>
            var ytInitialPlayerResponse = {"captions":{"playerCaptionsTracklistRenderer":{"captionTracks":[
              {"baseUrl":"https://x/api/timedtext?v=1&lang=en","languageCode":"en","kind":"asr"},
              {"baseUrl":"https://x/api/timedtext?v=1&lang=en-man","languageCode":"en"}
            ]}}};
        </script></body></html>"#;
        let doc = Html::parse_document(html);
        let tracks = caption_tracks(&doc);
        assert_eq!(tracks.len(), 2);
        // serde_json un-escaped the `&` to `&`.
        assert_eq!(choose_track(&tracks), Some("https://x/api/timedtext?v=1&lang=en-man"));
    }
}
