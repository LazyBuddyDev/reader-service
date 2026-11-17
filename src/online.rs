//! Anonymous official App metadata search only; no device registration or verification bypass.
use crate::model::Result;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

pub struct Search {
    pub id: u64,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Value>>>,
    result: Option<Result<Value>>,
}

fn parameters(args: &Value) -> Result<(String, u64)> {
    if args["source"] != "fanqie" {
        return Err("unsupported_source");
    }
    let query = args["query"].as_str().unwrap_or("").trim();
    if query.is_empty() || query.chars().count() > 100 || query.chars().any(char::is_control) {
        return Err("invalid_search_query");
    }
    let cursor = match args.get("cursor") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|p| *p <= u32::MAX as u64)
            .ok_or("invalid_search_cursor")?,
    };
    Ok((query.to_owned(), cursor))
}

impl Search {
    pub fn start(id: u64, args: &Value) -> Result<Self> {
        let (query, cursor) = parameters(args)?;
        Ok(Self::worker(id, move |cancel| {
            fetch(&query, cursor, cancel)
        }))
    }
    pub fn detail(id: u64, args: &Value) -> Result<Self> {
        if args["source"] != "fanqie" {
            return Err("unsupported_source");
        }
        let book_id = args["book_id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or("invalid_source_book")?
            .to_owned();
        Ok(Self::worker(id, move |cancel| {
            fetch_detail(&book_id, cancel)
        }))
    }
    fn worker(id: u64, work: impl FnOnce(&AtomicBool) -> Result<Value> + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || work(&worker_cancel));
        Self {
            id,
            cancel,
            worker: Some(worker),
            result: None,
        }
    }
    pub fn running(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn status(&mut self) -> Value {
        if self.cancel.load(Ordering::Acquire) {
            return json!({"status":"cancelled"});
        }
        if self.running() {
            return json!({"status":"pending"});
        }
        if let Some(worker) = self.worker.take() {
            self.result = Some(worker.join().unwrap_or(Err("search_worker_failed")));
        }
        match self.result.as_ref().unwrap() {
            Ok(result) => json!({"status":"complete","result":result}),
            Err(code) => json!({"status":"failed","error":code}),
        }
    }
}
impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn network_error(error: ureq::Error) -> &'static str {
    match error {
        ureq::Error::StatusCode(401 | 403) => "source_access_denied",
        ureq::Error::StatusCode(429) => "source_rate_limited",
        ureq::Error::Timeout(_) => "source_timeout",
        ureq::Error::TooManyRedirects => "source_access_denied",
        ureq::Error::BodyExceedsLimit(_) => "source_response_too_large",
        _ => "source_network_failed",
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_per_read(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent()
}
fn fetch(query: &str, cursor: u64, cancel: &AtomicBool) -> Result<Value> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    let response = agent()
        .get("https://novel.snssdk.com/api/novel/channel/homepage/search/search/v1/")
        .query("device_platform", "android")
        .query("parent_enterfrom", "novel_channel_search.tab.")
        .query("aid", "1967")
        .query("q", query)
        .query("offset", cursor.to_string())
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "LazyBuddy/reader (+https://github.com/LazyBuddyDev/reader-service)",
        )
        .call()
        .map_err(network_error)?;
    decode(&read_response(response, 1024 * 1024, cancel)?, cursor)
}

fn read_response(
    mut response: ureq::http::Response<ureq::Body>,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    if response.headers().contains_key("bdturing-verify")
        || response.headers().contains_key("x-vc-bdturing-parameters")
    {
        return Err("source_verification_required");
    }
    if response.status().is_redirection() {
        return Err("source_access_denied");
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(network_error)?;
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    Ok(bytes)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.bytes().all(|b| b.is_ascii_digit())
}

fn fetch_detail(book_id: &str, cancel: &AtomicBool) -> Result<Value> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    let response = agent()
        .get(format!("https://fanqienovel.com/page/{book_id}"))
        .header("Accept", "text/html")
        .header(
            "User-Agent",
            "LazyBuddy/reader (+https://github.com/LazyBuddyDev/reader-service)",
        )
        .call()
        .map_err(network_error)?;
    let bytes = read_response(response, 4 * 1024 * 1024, cancel)?;
    detail(&bytes, book_id)
}

fn detail(bytes: &[u8], book_id: &str) -> Result<Value> {
    if bytes.is_empty() {
        return Err("source_empty_response");
    }
    let html = std::str::from_utf8(bytes).map_err(|_| "invalid_source_response")?;
    let json = html
        .split_once("window.__INITIAL_STATE__")
        .and_then(|(_, s)| s.trim_start().strip_prefix('='))
        .ok_or("invalid_source_response")?;
    // Parse one JSON value; never execute the surrounding page's JavaScript.
    let state = serde_json::Deserializer::from_str(json)
        .into_iter::<Value>()
        .next()
        .ok_or("invalid_source_response")?
        .map_err(|_| "invalid_source_response")?;
    let page = &state["page"];
    if page["bookId"] != book_id {
        return Err("invalid_source_response");
    }
    let title = text(page, "bookName", 300)?;
    if title.trim().is_empty() {
        return Err("invalid_source_response");
    }
    let count = page["chapterTotal"]
        .as_u64()
        .filter(|n| *n <= 10000)
        .ok_or("invalid_source_response")?;
    let volumes = page["chapterListWithVolume"]
        .as_array()
        .filter(|v| v.len() <= 10000)
        .ok_or("invalid_source_response")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut chapters = Vec::new();
    for volume in volumes {
        for item in volume.as_array().ok_or("invalid_source_response")? {
            let id = text(item, "itemId", 24)?;
            let title = text(item, "title", 300)?;
            if !valid_id(id)
                || title.trim().is_empty()
                || !seen.insert(id)
                || chapters.len() >= 10000
            {
                return Err("invalid_source_response");
            }
            chapters.push(json!({"id":id,"title":title}));
        }
    }
    if chapters.len() as u64 != count {
        return Err("incomplete_source_directory");
    }
    Ok(
        json!({"source":"fanqie","id":book_id,"title":title,"author":text(page,"author",200)?,"description":text(page,"abstract",10000)?,"chapters":chapters}),
    )
}

fn text<'a>(item: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let value = item[key]
        .as_str()
        .filter(|s| s.chars().count() <= max)
        .ok_or("invalid_source_response")?;
    // Reject obfuscated fallback data; the verified App endpoint returns normal Unicode.
    if value
        .chars()
        .any(|c| matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd))
    {
        return Err("source_text_obfuscated");
    }
    Ok(value)
}
fn decode(bytes: &[u8], cursor: u64) -> Result<Value> {
    if bytes.is_empty() {
        return Err("source_empty_response");
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_source_response")?;
    match value["code"].as_i64() {
        Some(0) => (),
        Some(_) => return Err("source_request_rejected"),
        None => return Err("invalid_source_response"),
    }
    let data = &value["data"];
    let items = data["ret_data"]
        .as_array()
        .filter(|a| a.len() <= 50)
        .ok_or("invalid_source_response")?;
    let has_more = data["has_more"]
        .as_bool()
        .ok_or("invalid_source_response")?;
    let offset = data["offset"]
        .as_u64()
        .filter(|n| *n <= u32::MAX as u64)
        .ok_or("invalid_source_response")?;
    if has_more && (offset <= cursor || items.is_empty()) {
        return Err("invalid_source_response");
    }
    let mut entries = Vec::new();
    for item in items {
        let id = text(item, "book_id", 24)?;
        if !valid_id(id) {
            return Err("invalid_source_response");
        }
        let title = text(item, "title", 300)?;
        if title.trim().is_empty() || entries.iter().any(|e: &Value| e["id"] == id) {
            return Err("invalid_source_response");
        }
        entries.push(json!({"id":id,"title":title,"author":text(item,"author",200)?,"description":text(item,"abstract",10000)?}));
    }
    Ok(
        json!({"source":"fanqie","entries":entries,"cursor":cursor,"next_cursor":if has_more {Some(offset)} else {None}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn detail_page(page: Value) -> Vec<u8> {
        format!(
            "<script>window.__INITIAL_STATE__ = {};window.untrustedScript();</script>",
            json!({"page":page})
        )
        .into_bytes()
    }
    fn sample_detail() -> Value {
        json!({"bookId":"123","bookName":"示例书","author":"作者","abstract":"<script>只是简介</script>","thumbUrl":"https://private.invalid/?secret","chapterTotal":2,"chapterListWithVolume":[[{"itemId":"901","title":"第一章","isChapterLock":true}],[{"itemId":"902","title":"第二章"}]]})
    }
    #[test]
    fn public_metadata_preserves_complete_directory_order_and_excludes_unrelated_fields() {
        let value = detail(&detail_page(sample_detail()), "123").unwrap();
        assert_eq!(value["title"], "示例书");
        assert_eq!(
            value["chapters"],
            json!([{"id":"901","title":"第一章"},{"id":"902","title":"第二章"}])
        );
        assert_eq!(value["description"], "<script>只是简介</script>");
        assert!(!value.to_string().contains("secret"));
        assert!(!value.to_string().contains("isChapterLock"));
    }
    #[test]
    fn metadata_cannot_silently_accept_partial_or_mismatched_directories() {
        let mut page = sample_detail();
        page["chapterTotal"] = json!(3);
        assert_eq!(
            detail(&detail_page(page), "123"),
            Err("incomplete_source_directory")
        );
        let mut page = sample_detail();
        page["chapterListWithVolume"][1][0]["itemId"] = json!("901");
        assert_eq!(
            detail(&detail_page(page), "123"),
            Err("invalid_source_response")
        );
        assert_eq!(
            detail(&detail_page(sample_detail()), "124"),
            Err("invalid_source_response")
        );
        let mut page = sample_detail();
        page["chapterTotal"] = json!(10001);
        assert_eq!(
            detail(&detail_page(page), "123"),
            Err("invalid_source_response")
        );
        assert_eq!(
            detail(b"window.__INITIAL_STATE__ = {page:undefined}", "123"),
            Err("invalid_source_response")
        );
        assert_eq!(detail(b"", "123"), Err("source_empty_response"));
    }
    #[test]
    fn detail_identifiers_are_checked_before_spawning_network_work() {
        for id in [
            "",
            "../123",
            "123?x=y",
            "https://example.invalid",
            "1234567890123456789012345",
        ] {
            assert!(matches!(
                Search::detail(1, &json!({"source":"fanqie","book_id":id})),
                Err("invalid_source_book")
            ));
        }
        assert!(matches!(
            Search::detail(1, &json!({"source":"other","book_id":"123"})),
            Err("unsupported_source")
        ));
    }
    #[test]
    fn verification_redirects_limits_and_cancellation_are_checked_before_decoding() {
        let cancel = AtomicBool::new(false);
        let reply = || ureq::http::Response::builder();
        for header in ["bdturing-verify", "x-vc-bdturing-parameters"] {
            let response = reply()
                .header(header, "private-challenge")
                .body(ureq::Body::builder().data(b"".to_vec()))
                .unwrap();
            assert_eq!(
                read_response(response, 1024 * 1024, &cancel),
                Err("source_verification_required")
            );
        }
        let response = reply()
            .status(302)
            .header("Location", "https://private.invalid/")
            .body(ureq::Body::builder().data(b"".to_vec()))
            .unwrap();
        assert_eq!(
            read_response(response, 1024 * 1024, &cancel),
            Err("source_access_denied")
        );
        assert_eq!(
            network_error(ureq::Error::TooManyRedirects),
            "source_access_denied"
        );
        let response = reply()
            .body(ureq::Body::builder().data(vec![b'x'; 1024 * 1024 + 1]))
            .unwrap();
        assert_eq!(
            read_response(response, 1024 * 1024, &cancel),
            Err("source_response_too_large")
        );
        cancel.store(true, Ordering::Release);
        let response = reply()
            .body(ureq::Body::builder().data(b"invalid".to_vec()))
            .unwrap();
        assert_eq!(
            read_response(response, 1024 * 1024, &cancel),
            Err("cancelled")
        );
    }
    #[test]
    fn private_font_characters_are_not_presented_as_readable_metadata() {
        for c in ['\u{e000}', '\u{f8ff}', '\u{f0000}', '\u{10fffd}'] {
            assert_eq!(
                text(&json!({"title":format!("书{c}")}), "title", 300),
                Err("source_text_obfuscated")
            );
        }
        assert_eq!(text(&json!({"title":"中文😀"}), "title", 300), Ok("中文😀"));
    }
    #[test]
    fn validates_source_query_and_cursor_before_network() {
        assert_eq!(
            parameters(&json!({"source":"other","query":"书"})),
            Err("unsupported_source")
        );
        for query in [
            "".to_owned(),
            "  ".to_owned(),
            "a\nb".to_owned(),
            "书".repeat(101),
        ] {
            assert_eq!(
                parameters(&json!({"source":"fanqie","query":query})),
                Err("invalid_search_query")
            );
        }
        for page in [json!(-1), json!(4294967296u64), json!("1"), Value::Null] {
            assert_eq!(
                parameters(&json!({"source":"fanqie","query":"书","cursor":page})),
                Err("invalid_search_cursor")
            );
        }
        assert_eq!(
            parameters(&json!({"source":"fanqie","query":" 惊鸿 "})),
            Ok(("惊鸿".into(), 0))
        );
    }
    #[test]
    fn parses_official_app_search_shape_without_exposing_upstream_fields() {
        let entry = json!({"book_id":"6883748331202284558","title":"惊鸿","author":"一夕烟雨","abstract":"简介","thumb_url":"https://private.invalid/?secret"});
        let reply =
            json!({"code":0,"data":{"ret_data":[entry.clone()],"has_more":true,"offset":10}});
        let result = decode(&serde_json::to_vec(&reply).unwrap(), 0).unwrap();
        assert_eq!(result["entries"][0]["id"], entry["book_id"]);
        assert_eq!(result["next_cursor"], 10);
        assert!(!result.to_string().contains("secret"));
        let mut invalid = reply;
        invalid["data"]["ret_data"][0]["book_id"] = json!("../x");
        assert_eq!(
            decode(&serde_json::to_vec(&invalid).unwrap(), 0),
            Err("invalid_source_response")
        );
    }
    #[test]
    fn malformed_or_blocked_responses_are_not_empty_results() {
        for bytes in [
            b"".as_slice(),
            b"<html>verify</html>",
            br#"{"code":0}"#,
            br#"{"code":0,"data":{"ret_data":[],"has_more":true,"offset":0}}"#,
            br#"{"code":1}"#,
        ] {
            assert!(decode(bytes, 0).is_err());
        }
        assert_eq!(
            decode(
                br#"{"code":0,"data":{"ret_data":[],"has_more":false,"offset":0}}"#,
                0
            )
            .unwrap()["entries"],
            json!([])
        );
        assert_eq!(
            network_error(ureq::Error::StatusCode(403)),
            "source_access_denied"
        );
        assert_eq!(
            network_error(ureq::Error::StatusCode(429)),
            "source_rate_limited"
        );
    }
    #[test]
    fn cancelled_worker_still_counts_as_running_and_cannot_deliver_late_results() {
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            Ok(json!({"entries":["late"]}))
        });
        let mut search = Search {
            id: 1,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: Some(worker),
            result: None,
        };
        search.cancel();
        assert!(search.running());
        assert_eq!(search.status()["status"], "cancelled");
        release.send(()).unwrap();
        search.worker.take().unwrap().join().unwrap().unwrap();
        assert_eq!(search.status()["status"], "cancelled");
    }
    #[test]
    fn engine_suspend_cancels_search_and_old_job_cannot_cancel_current_job() {
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            Ok(json!({}))
        });
        let mut engine = crate::Engine {
            root: Default::default(),
            library: Default::default(),
            job: None,
            next_job: 1,
            search: Some(Search {
                id: 1,
                cancel: Arc::new(AtomicBool::new(false)),
                worker: Some(worker),
                result: None,
            }),
        };
        assert_eq!(
            engine.call("online.status", json!({"job":2})),
            Err("job_not_found")
        );
        assert_eq!(
            engine.call("online.cancel", json!({"job":2})),
            Err("job_not_found")
        );
        assert_eq!(
            engine.call("online.status", json!({"job":1})).unwrap()["status"],
            "pending"
        );
        engine.call("lifecycle.suspend", json!({})).unwrap();
        assert_eq!(
            engine.call("online.status", json!({"job":1})).unwrap()["status"],
            "cancelled"
        );
        assert_eq!(
            engine.call("online.search", json!({"source":"fanqie","query":"新词"})),
            Err("search_busy")
        );
        assert_eq!(
            engine.call("online.detail", json!({"source":"fanqie","book_id":"123"})),
            Err("search_busy")
        );
        release.send(()).unwrap();
        engine
            .search
            .as_mut()
            .unwrap()
            .worker
            .take()
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(
            engine.call("online.status", json!({"job":1})).unwrap()["status"],
            "cancelled"
        );
    }
}
