//! Fixed official App endpoints and their declared free CDN resources.
use crate::{model::Result, online};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

const API: &str = "https://api.midureader.com/fiction/";
const CDN: &str = "https://book.midureader.com/book/";

pub(crate) fn book_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn checked_id(args: &Value) -> Result<&str> {
    args["book_id"]
        .as_str()
        .filter(|s| book_id(s))
        .ok_or("invalid_source_book")
}
fn request(url: &str, form: Option<&[(&str, &str)]>, cancel: &AtomicBool) -> Result<Value> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    let agent = online::agent();
    let response = match form {
        Some(fields) => agent.post(url).send_form(fields.iter().copied()),
        None => agent.get(url).call(),
    }
    .map_err(online::network_error)?;
    let bytes = online::read_response(response, 4 * 1024 * 1024, cancel)?;
    if bytes.is_empty() {
        return Err("source_empty_response");
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid_source_response")
}
fn api(path: &str, fields: &[(&str, &str)], cancel: &AtomicBool) -> Result<Value> {
    let value = request(&format!("{API}{path}"), Some(fields), cancel)?;
    if value["code"] != 0 {
        return Err("source_request_rejected");
    }
    if value["data"].is_null() {
        return Err("invalid_source_response");
    }
    Ok(value["data"].clone())
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let text = online::text(v, key, max)?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("invalid_source_response");
    }
    Ok(text)
}
fn zero(v: &Value) -> bool {
    v == 0 || v == "0"
}
fn free_book(v: &Value) -> bool {
    (v["free_type"] == 1 || v["free_type"] == "1")
        && zero(&v["free_dead_line"])
        && zero(&v["free_expiration_sec"])
}
fn search_page(items: &Value, cursor: u64) -> Result<Value> {
    let items = items
        .as_array()
        .filter(|a| a.len() <= 50)
        .ok_or("invalid_source_response")?;
    let mut entries = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let book = &item["bookData"];
        if book.is_null() {
            continue;
        }
        if book["free_type"] != 1 && book["free_type"] != "1" {
            continue;
        }
        let id = text(book, "book_id", 32)?;
        let title = text(book, "title", 300)?;
        if !book_id(id) || title.trim().is_empty() || !seen.insert(id) {
            return Err("invalid_source_response");
        }
        let author = item["emAuthor"].as_str().unwrap_or("");
        if author.len() > 2000 {
            return Err("invalid_source_response");
        }
        let author = scraper::Html::parse_fragment(author)
            .root_element()
            .text()
            .collect::<String>();
        if author.chars().count() > 200 {
            return Err("invalid_source_response");
        }
        entries.push(json!({"id":id,"title":title,"author":author,"description":text(book,"description",10000)?}));
    }
    Ok(
        json!({"source":"midu","entries":entries,"cursor":cursor,"next_cursor":if items.len()>=10 {cursor.checked_add(1).filter(|n| *n<=u32::MAX as u64)}else{None}}),
    )
}
pub(crate) fn start(id: u64, method: &str, args: &Value) -> Result<online::Search> {
    if method == "online.search" {
        let (query, cursor) = online::parameters(args)?;
        Ok(online::Search::worker(id, move |cancel| {
            let page = cursor.to_string();
            search_page(
                &api(
                    "search/searchV2",
                    &[("app", "midu"), ("keyword", &query), ("page", &page)],
                    cancel,
                )?,
                cursor,
            )
        }))
    } else {
        let book = checked_id(args)?.to_owned();
        Ok(online::Search::worker(id, move |cancel| {
            let (meta, chapters) = directory(&book, cancel)?;
            Ok(
                json!({"source":"midu","id":book,"title":text(&meta,"title",300)?,"author":text(&meta,"author",200)?,"description":text(&meta,"description",10000)?,"chapters":chapters.iter().map(|c|json!({"id":c["chapterId"],"title":c["title"]})).collect::<Vec<_>>()}),
            )
        }))
    }
}
fn catalog_url(pointer: &Value, book: &str) -> Result<String> {
    if pointer["freeChapterCount"] != -1 {
        return Err("source_chapter_locked");
    }
    let url = pointer["url"].as_str().ok_or("invalid_source_response")?;
    let prefix = format!("{CDN}chapter_list/100/{book}/");
    let digest = url
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix(".txt"))
        .filter(|s| book_id(s))
        .ok_or("invalid_source_response")?;
    Ok(format!("{prefix}{digest}.txt"))
}
fn chapters(value: Value, book: &str) -> Result<Vec<Value>> {
    let items = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 10000)
        .ok_or("invalid_source_response")?;
    let mut seen = std::collections::BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let id = text(item, "chapterId", 24)?;
        let ordinal = text(item, "no", 10)?
            .parse::<usize>()
            .map_err(|_| "invalid_source_response")?;
        if ordinal != index + 1 {
            return Err("incomplete_source_directory");
        }
        if item["bookId"] != book
            || !online::valid_id(id)
            || !seen.insert(id)
            || text(item, "title", 300)?.trim().is_empty()
            || !book_id(text(item, "content_md5", 32)?)
        {
            return Err("invalid_source_response");
        }
        // isFree is "0" even on the verified free App path; use its grant and actual costs.
        if !zero(&item["price"]) || !zero(&item["coin_amount"]) {
            return Err("source_chapter_locked");
        }
    }
    Ok(items.clone())
}
fn directory(book: &str, cancel: &AtomicBool) -> Result<(Value, Vec<Value>)> {
    let meta = api(
        "book/getDetail",
        &[("app", "midu"), ("book_id", book)],
        cancel,
    )?;
    if meta["book_id"] != book || text(&meta, "title", 300)?.trim().is_empty() {
        return Err("invalid_source_response");
    }
    if !free_book(&meta) {
        return Err("source_chapter_locked");
    }
    let pointer = api(
        "book/getChaptersCdn",
        &[("app", "midu"), ("lastUpdatedTime", ""), ("bookId", book)],
        cancel,
    )?;
    let url = catalog_url(&pointer, book)?;
    let chapters = chapters(request(&url, None, cancel)?, book)?;
    Ok((meta, chapters))
}
fn body(value: &Value) -> Result<String> {
    let items = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 4096)
        .ok_or("invalid_source_response")?;
    let mut content = String::new();
    for item in items {
        let text = text(item, "content", 100000)?;
        if text.is_empty() {
            continue;
        }
        content.push_str("<p>");
        for c in text.chars() {
            match c {
                '&' => content.push_str("&amp;"),
                '<' => content.push_str("&lt;"),
                '>' => content.push_str("&gt;"),
                '\n' => content.push_str("<br>"),
                _ => content.push(c),
            }
        }
        content.push_str("</p>");
        if content.len() > 1024 * 1024 {
            return Err("source_response_too_large");
        }
    }
    if content.is_empty() {
        return Err("source_empty_chapter");
    }
    Ok(content)
}
pub(crate) fn chapter(book: &str, chapter: &str, cancel: &AtomicBool) -> Result<String> {
    if !book_id(book) || !online::valid_id(chapter) {
        return Err("invalid_source_book");
    }
    let (_, items) = directory(book, cancel)?;
    let item = items
        .iter()
        .find(|c| c["chapterId"] == chapter)
        .ok_or("source_chapter_missing")?;
    let digest = text(item, "content_md5", 32)?;
    let url = format!("{CDN}chapter/segment/master/{book}/{chapter}/{digest}.txt");
    body(&request(&url, None, cancel)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    const BOOK: &str = "377b09d943409656e97d09f8f73a393b";
    fn item() -> Value {
        json!({"bookId":BOOK,"chapterId":"123","title":"第一章","no":"1","content_md5":BOOK,"price":"0","coin_amount":0,"isFree":"0"})
    }
    #[test]
    fn source_ids_are_bounded() {
        assert!(book_id(BOOK));
        for id in ["123", "../chapter", "377b09d943409656e97d09f8f73a393B"] {
            assert!(!book_id(id));
        }
    }
    #[test]
    fn paid_or_temporary_books_are_rejected() {
        let mut v = json!({"free_type":1,"free_dead_line":0,"free_expiration_sec":0});
        assert!(free_book(&v));
        v["free_expiration_sec"] = json!(60);
        assert!(!free_book(&v));
        v["free_expiration_sec"] = json!(0);
        v["free_type"] = json!(2);
        assert!(!free_book(&v));
        assert_eq!(
            catalog_url(&json!({"freeChapterCount":10}), BOOK),
            Err("source_chapter_locked")
        );
    }
    #[test]
    fn only_declared_fixed_cdn_snapshot_is_accepted() {
        let mut p =
            json!({"freeChapterCount":-1,"url":format!("{CDN}chapter_list/100/{BOOK}/{BOOK}.txt")});
        assert!(catalog_url(&p, BOOK).is_ok());
        for url in [
            "https://evil.test/file.txt",
            &format!("{CDN}chapter_list/100/{BOOK}/../{BOOK}.txt"),
            &format!("{CDN}chapter_list/100/{BOOK}/{BOOK}.txt?token=x"),
        ] {
            p["url"] = json!(url);
            assert!(catalog_url(&p, BOOK).is_err());
        }
    }
    #[test]
    fn full_order_and_costs_checked_independently_of_isfree() {
        assert!(chapters(json!([item()]), BOOK).is_ok());
        let mut c = item();
        c["price"] = json!(1);
        assert_eq!(chapters(json!([c]), BOOK), Err("source_chapter_locked"));
        let mut c = item();
        c["no"] = json!("2");
        assert_eq!(
            chapters(json!([c]), BOOK),
            Err("incomplete_source_directory")
        );
        assert!(chapters(json!([item(), item()]), BOOK).is_err());
        let mut c = item();
        c["bookId"] = json!("other");
        assert!(chapters(json!([c]), BOOK).is_err());
    }
    #[test]
    fn search_keeps_server_pagination_and_plain_metadata() {
        let book = json!({"bookData":{"book_id":BOOK,"title":"同名书","description":"简介","free_type":1},"emAuthor":"<em>作者</em>"});
        let items = Value::Array(
            (0..10)
                .map(|_| json!({"itemType":"recommendation"}))
                .collect(),
        );
        assert_eq!(search_page(&items, 2).unwrap()["next_cursor"], 3);
        let result = search_page(&json!([book]), 0).unwrap();
        assert_eq!(result["entries"][0]["author"], "作者");
        assert_eq!(result["next_cursor"], Value::Null);
    }
    #[test]
    fn segmented_text_is_bounded_and_never_html() {
        assert_eq!(
            body(&json!([{"content":"正文 <script>\n下一段"}])).unwrap(),
            "<p>正文 &lt;script&gt;<br>下一段</p>"
        );
        assert_eq!(body(&json!([])), Err("invalid_source_response"));
        assert_eq!(body(&json!([{"content":""}])), Err("source_empty_chapter"));
    }
}
