//! GPL-3.0-only. App protocol adapted from xing-yv's 7mao-novel-downloader
//! v3.0.0/src/public.py (blob ab5175d2f07c896d15ec3d0b9221760b1e035032).
//! Copyright (C) 2023 星隅 (xing-yv). No version/identity rotation or retries.
use md5::{Digest, Md5};
use mlua::{Lua, Table};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
type Result<T> = std::result::Result<T, &'static str>;
const SIGN_KEY: &str = "d3dGiJc651gSQ8w1";
const MAX: u64 = 4 * 1024 * 1024;

fn sign(fields: &BTreeMap<&str, String>) -> String {
    let mut value = fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<String>();
    value.push_str(SIGN_KEY);
    format!("{:x}", Md5::digest(value.as_bytes()))
}
fn fetch(endpoint: &str, fields: &[(&str, String)]) -> Result<Value> {
    let url = match endpoint {
        "search" => "https://api-bc.wtzw.com/search/v1/words",
        "detail" => "https://api-bc.wtzw.com/api/v1/reader/detail",
        "catalog" => "https://api-ks.wtzw.com/api/v1/chapter/chapter-list",
        "chapter" => "https://api-ks.wtzw.com/api/v1/chapter/content",
        _ => return Err("invalid_arguments"),
    };
    let agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_per_read(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let headers = BTreeMap::from([
        ("AUTHORIZATION", String::new()),
        ("app-version", "73720".into()),
        ("application-id", "com.kmxs.reader".into()),
        ("channel", "unknown".into()),
        ("net-env", "1".into()),
        ("platform", "android".into()),
        ("qm-params", String::new()),
        ("reg", "0".into()),
    ]);
    let mut request = agent.get(url).header("sign", sign(&headers));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let fields = fields.iter().cloned().collect::<BTreeMap<_, _>>();
    request = request.query("sign", sign(&fields));
    for (name, value) in fields {
        request = request.query(name, value);
    }
    let mut response = request.call().map_err(network_error)?;
    if response.status().is_redirection() {
        return Err("source_access_denied");
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX)
        .read_to_vec()
        .map_err(network_error)?;
    if bytes.is_empty() {
        return Err("source_empty_response");
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_source_response")?;
    if value.get("code").is_some_and(|c| c != 0) {
        return Err("source_request_rejected");
    }
    if !value["data"].is_object() {
        return Err("invalid_source_response");
    }
    Ok(value["data"].clone())
}
fn network_error(error: ureq::Error) -> &'static str {
    match error {
        ureq::Error::StatusCode(401 | 403) | ureq::Error::TooManyRedirects => {
            "source_access_denied"
        }
        ureq::Error::StatusCode(429) => "source_rate_limited",
        ureq::Error::Timeout(_) => "source_timeout",
        ureq::Error::BodyExceedsLimit(_) => "source_response_too_large",
        _ => "source_network_failed",
    }
}
fn text<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    value[key].as_str().filter(|s|s.chars().count()<=max && !s.chars().any(|c|matches!(c as u32,0xe000..=0xf8ff|0xf0000..=0xffffd|0x100000..=0x10fffd|0xfffd))).ok_or("invalid_source_response")
}
fn number(value: &Value, key: &str) -> Result<u64> {
    text(value, key, 10)?
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= u32::MAX as u64)
        .ok_or("invalid_source_response")
}
pub fn request(args: &Value) -> Result<Value> {
    match args["method"].as_str() {
        Some("qimao.search") => {
            let query = args["query"]
                .as_str()
                .filter(|s| {
                    !s.trim().is_empty()
                        && s.chars().count() <= 100
                        && !s.chars().any(char::is_control)
                })
                .ok_or("invalid_search_query")?;
            let cursor = args["cursor"]
                .as_u64()
                .filter(|n| *n < u32::MAX as u64)
                .ok_or("invalid_search_cursor")?;
            let data = fetch(
                "search",
                &[
                    ("extend", "".into()),
                    ("tab", "0".into()),
                    ("gender", "0".into()),
                    ("refresh_state", "8".into()),
                    ("page", (cursor + 1).to_string()),
                    ("wd", query.trim().into()),
                    ("is_short_story_user", "0".into()),
                ],
            )?;
            search(&data, cursor)
        }
        Some("qimao.detail") => {
            let book = text(args, "book_id", 24)?;
            if !super::valid_id(book) {
                return Err("invalid_source_book");
            }
            let data = fetch("detail", &[("id", book.into())])?;
            let catalog = fetch(
                "catalog",
                &[("id", book.into()), ("chapter_ver", "0".into())],
            )?;
            detail(&data, &catalog, book)
        }
        Some("qimao.chapter") => {
            let book = text(args, "book_id", 24)?;
            let chapter = text(args, "chapter_id", 24)?;
            if !super::valid_id(book) || !super::valid_id(chapter) {
                return Err("invalid_source_book");
            }
            let data = fetch(
                "chapter",
                &[("id", book.into()), ("chapterId", chapter.into())],
            )?;
            Ok(json!({"content":body(&data,chapter)?}))
        }
        _ => Err("invalid_arguments"),
    }
}
fn search(data: &Value, cursor: u64) -> Result<Value> {
    if number(&data["meta"], "page")? != cursor + 1 {
        return Err("invalid_source_response");
    }
    let next = number(&data["meta"], "next_page")?;
    let rows = data["books"]
        .as_array()
        .filter(|v| v.len() <= 50)
        .ok_or("invalid_source_response")?;
    if next != 0 && next <= cursor + 1 || rows.is_empty() && next != 0 {
        return Err("invalid_source_response");
    }
    let mut entries = vec![];
    let mut seen = BTreeSet::new();
    for row in rows {
        // Only the standard text reader has a verified full App reading path.
        if row["reader_type"] != "0" || row["is_audio"] != "0" {
            continue;
        }
        let id = text(row, "id", 24)?;
        let title = text(row, "title", 300)?;
        if !super::valid_id(id) || title.trim().is_empty() || !seen.insert(id) {
            return Err("invalid_source_response");
        }
        entries.push(json!({"id":id,"title":title,"author":text(row,"author",200)?,"description":text(row,"intro",10000)?}));
    }
    Ok(
        json!({"source":"qimao","entries":entries,"cursor":cursor,"next_cursor":if next==0 {None}else{Some(next-1)}}),
    )
}
fn detail(data: &Value, catalog: &Value, book: &str) -> Result<Value> {
    if data["id"] != book || catalog["id"] != book {
        return Err("invalid_source_response");
    }
    if data["reader_type"] != "0" {
        return Err("source_body_unavailable");
    }
    let title = text(data, "title", 300)?;
    if title.trim().is_empty() {
        return Err("invalid_source_response");
    }
    let count = number(data, "chapters")?;
    let rows = catalog["chapter_lists"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 10000)
        .ok_or("incomplete_source_directory")?;
    if count != rows.len() as u64 {
        return Err("incomplete_source_directory");
    }
    let mut chapters = vec![];
    let mut seen = BTreeSet::new();
    for (i, row) in rows.iter().enumerate() {
        let id = text(row, "id", 24)?;
        let title = text(row, "title", 300)?;
        if !super::valid_id(id)
            || title.trim().is_empty()
            || !seen.insert(id)
            || row["chapter_sort"].as_u64() != Some(i as u64 + 1)
        {
            return Err("invalid_source_response");
        }
        chapters.push(json!({"id":id,"title":title}));
    }
    Ok(
        json!({"source":"qimao","id":book,"title":title,"author":text(data,"author",200)?,"description":text(data,"intro",10000)?,"chapters":chapters}),
    )
}
fn body(data: &Value, chapter: &str) -> Result<String> {
    if data["id"] != chapter {
        return Err("invalid_source_response");
    }
    let encoded = text(data, "content", 2 * 1024 * 1024)?;
    if encoded.len() % 4 != 0
        || !encoded
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
    {
        return Err("source_decode_failed");
    }
    let lua = Lua::new();
    lua.set_memory_limit(64 * 1024 * 1024)
        .map_err(|_| "source_runtime_failed")?;
    lua.load("package.path='';package.cpath=''")
        .exec()
        .map_err(|_| "source_runtime_failed")?;
    let aes: Table = lua
        .load(include_str!("../vendor/fanqie/reading/aes.lua"))
        .eval()
        .map_err(|_| "source_runtime_failed")?;
    let text: Table = lua
        .load(include_str!("../vendor/fanqie/reading/text.lua"))
        .eval()
        .map_err(|_| "source_runtime_failed")?;
    let raw: mlua::LuaString = text
        .get::<mlua::Function>("base64Decode")
        .and_then(|f| f.call(encoded))
        .map_err(|_| "source_decode_failed")?;
    let raw = raw.as_bytes();
    if raw.len() < 32 || raw.len() % 16 != 0 {
        return Err("source_decode_failed");
    }
    let plain: mlua::LuaString = aes
        .get::<mlua::Function>("cbc_decrypt")
        .and_then(|f| {
            f.call((
                lua.create_string(&raw[16..])?,
                "242ccb8230d709e1",
                lua.create_string(&raw[..16])?,
                false,
            ))
        })
        .map_err(|_| "source_decode_failed")?;
    let bytes = plain.as_bytes();
    let plain = std::str::from_utf8(unpad(&bytes)?).map_err(|_| "source_decode_failed")?;
    if plain.len() > 1024 * 1024 {
        return Err("source_response_too_large");
    }
    let mut output = String::new();
    let mut count = 0;
    for line in plain.lines().filter(|s| !s.trim().is_empty()) {
        count += 1;
        if count > 4096 {
            return Err("source_response_too_large");
        }
        output.push_str("<p>");
        output.push_str(
            &line
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        );
        output.push_str("</p>");
    }
    if output.is_empty() {
        return Err("source_empty_chapter");
    }
    Ok(output)
}
fn unpad(bytes: &[u8]) -> Result<&[u8]> {
    let count = bytes.last().copied().unwrap_or(0) as usize;
    if bytes.is_empty()
        || bytes.len() % 16 != 0
        || !(1..=16).contains(&count)
        || !bytes[bytes.len() - count..]
            .iter()
            .all(|b| *b as usize == count)
    {
        return Err("source_decode_failed");
    }
    Ok(&bytes[..bytes.len() - count])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_padding_checks_every_byte_before_decoding_utf8() {
        let mut bytes = [b'a'; 16];
        bytes[15] = 1;
        assert_eq!(unpad(&bytes).unwrap(), &bytes[..15]);
        bytes[15] = 2;
        assert!(unpad(&bytes).is_err());
        bytes[14] = 2;
        assert_eq!(unpad(&bytes).unwrap(), &bytes[..14]);
        assert!(unpad(&bytes[..15]).is_err());
        assert!(unpad(&[]).is_err());
        assert_eq!(unpad(&[16; 16]).unwrap(), b"");
    }
    #[test]
    fn app_catalog_never_uses_web_vip_flags_or_accepts_incomplete_data() {
        let data = json!({"id":"12","reader_type":"0","title":"书","author":"作者","intro":"简介","chapters":"2"});
        let mut catalog = json!({"id":"12","chapter_lists":[{"id":"34","title":"第一章","chapter_sort":1,"is_vip":"1"},{"id":"35","title":"第二章","chapter_sort":2}]});
        assert_eq!(
            detail(&data, &catalog, "12").unwrap()["chapters"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        catalog["chapter_lists"].as_array_mut().unwrap().pop();
        assert_eq!(
            detail(&data, &catalog, "12"),
            Err("incomplete_source_directory")
        );
        assert!(detail(&data, &catalog, "99").is_err());
    }
    #[test]
    fn app_search_has_its_own_cursor_and_only_verified_text_types() {
        let mut data = json!({"meta":{"page":"2","next_page":"3"},"books":[{"id":"12","title":"书","author":"作者","intro":"简介","reader_type":"0","is_audio":"0"},{"id":"13","reader_type":"3","is_audio":"0"}]});
        let value = search(&data, 1).unwrap();
        assert_eq!(value["entries"].as_array().unwrap().len(), 1);
        assert_eq!(value["next_cursor"], 2);
        assert!(search(&data, 0).is_err());
        data["meta"]["next_page"] = json!("2");
        assert!(search(&data, 1).is_err());
        assert!(body(&json!({"id":"34","content":"invalid!"}), "34").is_err());
        assert!(body(&json!({"id":"99","content":"AAAA"}), "34").is_err());
    }
}
