//! Anonymous official web search only; no device registration or verification bypass.
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
    let page = match args.get("page") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|p| *p < 100)
            .ok_or("invalid_search_page")?,
    };
    Ok((query.to_owned(), page))
}

impl Search {
    pub fn start(id: u64, args: &Value) -> Result<Self> {
        let (query, page) = parameters(args)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || fetch(&query, page, &worker_cancel));
        Ok(Self {
            id,
            cancel,
            worker: Some(worker),
            result: None,
        })
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
        _ => "source_network_failed",
    }
}

fn fetch(query: &str, page: u64, cancel: &AtomicBool) -> Result<Value> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    let agent = ureq::Agent::config_builder()
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
        .new_agent();
    let mut response = agent
        .get("https://fanqienovel.com/api/author/search/search_book/v1")
        .query("query_word", query)
        .query("page_index", page.to_string())
        .query("page_count", "10")
        .query("query_type", "0")
        .query("filter", "127,127,127,127")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "LazyBuddy/reader (+https://github.com/LazyBuddyDev/reader-service)",
        )
        .call()
        .map_err(network_error)?;
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
        .limit(1024 * 1024)
        .read_to_vec()
        .map_err(network_error)?;
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    decode(&bytes, page)
}

fn text<'a>(item: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    item[key]
        .as_str()
        .filter(|s| s.chars().count() <= max)
        .ok_or("invalid_source_response")
}
fn decode(bytes: &[u8], page: u64) -> Result<Value> {
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
    let items = data["search_book_data_list"]
        .as_array()
        .filter(|a| a.len() <= 10)
        .ok_or("invalid_source_response")?;
    let total = data["total_count"]
        .as_u64()
        .ok_or("invalid_source_response")?;
    let mut entries = Vec::new();
    for item in items {
        let id = text(item, "book_id", 24)?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid_source_response");
        }
        let title = text(item, "book_name", 300)?;
        if title.trim().is_empty() || entries.iter().any(|e: &Value| e["id"] == id) {
            return Err("invalid_source_response");
        }
        entries.push(json!({"id":id,"title":title,"author":text(item,"author",200)?,"description":text(item,"book_abstract",10000)?}));
    }
    if entries.is_empty() && total > page * 10 {
        return Err("invalid_source_response");
    }
    Ok(
        json!({"source":"fanqie","entries":entries,"page":page,"has_more":(page+1)*10<total && page<99}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_source_query_and_page_before_network() {
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
        for page in [json!(-1), json!(100), json!("1"), Value::Null] {
            assert_eq!(
                parameters(&json!({"source":"fanqie","query":"书","page":page})),
                Err("invalid_search_page")
            );
        }
        assert_eq!(
            parameters(&json!({"source":"fanqie","query":" 惊鸿 "})),
            Ok(("惊鸿".into(), 0))
        );
    }
    #[test]
    fn parses_official_web_search_shape_without_exposing_upstream_fields() {
        let entry = json!({"book_id":"6883748331202284558","book_name":"惊鸿","author":"一夕烟雨","book_abstract":"简介","thumb_url":"https://private.invalid/?secret"});
        let reply =
            json!({"code":0,"data":{"search_book_data_list":[entry.clone()],"total_count":11}});
        let result = decode(&serde_json::to_vec(&reply).unwrap(), 0).unwrap();
        assert_eq!(result["entries"][0]["id"], entry["book_id"]);
        assert_eq!(result["has_more"], true);
        assert!(!result.to_string().contains("secret"));
        let mut invalid = reply;
        invalid["data"]["search_book_data_list"][0]["book_id"] = json!("../x");
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
            br#"{"code":0,"data":{"search_book_data_list":[],"total_count":1}}"#,
            br#"{"code":1}"#,
        ] {
            assert!(decode(bytes, 0).is_err());
        }
        assert_eq!(
            decode(
                br#"{"code":0,"data":{"search_book_data_list":[],"total_count":0}}"#,
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
