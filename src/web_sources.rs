//! Public guest reading only; paid chapters and preview text are never substituted.
use crate::{
    model::Result,
    online::{agent, network_error, read_response, text},
};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

pub fn supported(source: &str) -> bool {
    matches!(source, "fanqie" | "qimao" | "shuqi")
}
pub fn valid_book(source: &str, id: &str) -> bool {
    let numeric = |s: &str| !s.is_empty() && s.len() <= 24 && s.bytes().all(|b| b.is_ascii_digit());
    if source == "qimao" {
        let parts: Vec<_> = id.split('_').collect();
        parts.len() <= 2 && parts.iter().all(|p| numeric(p))
    } else {
        numeric(id)
    }
}
fn selector(value: &str) -> Selector {
    Selector::parse(value).unwrap()
}
fn html(bytes: &[u8]) -> Result<Html> {
    let doc =
        Html::parse_document(std::str::from_utf8(bytes).map_err(|_| "invalid_source_response")?);
    if doc.tree.nodes().count() > 50000 {
        return Err("source_response_too_large");
    }
    Ok(doc)
}
fn selected(doc: &Html, css: &str, max: usize) -> Result<String> {
    let value = doc
        .select(&selector(css))
        .next()
        .ok_or("invalid_source_response")?
        .text()
        .collect::<String>();
    let value = value.trim();
    text(&json!({"text":value}), "text", max)?;
    Ok(value.to_owned())
}
fn get(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err("cancelled");
    }
    let response = agent()
        .get(url)
        .header(
            "User-Agent",
            "LazyBuddy/reader (+https://github.com/LazyBuddyDev/reader-service)",
        )
        .call()
        .map_err(network_error)?;
    read_response(response, 8 * 1024 * 1024, cancel)
}
fn json_bytes(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| "invalid_source_response")
}
pub fn search(source: &str, query: &str, cursor: u64, cancel: &AtomicBool) -> Result<Value> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err("cancelled");
    }
    let url = if source == "qimao" {
        "https://www.qimao.com/api/search/result"
    } else {
        "https://www.shuqi.com/search"
    };
    let response = agent()
        .get(url)
        .query("keyword", query)
        .query("page", (cursor + 1).to_string())
        .call()
        .map_err(network_error)?;
    let bytes = read_response(response, 2 * 1024 * 1024, cancel)?;
    decode_search(source, &bytes, cursor)
}
fn decode_search(source: &str, bytes: &[u8], cursor: u64) -> Result<Value> {
    let mut entries = vec![];
    let has_more;
    if source == "qimao" {
        let data = json_bytes(bytes)?;
        let data = &data["data"];
        let count = data["page_data"]["count"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("invalid_source_response")?;
        let size = data["page_data"]["page_size"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0 && *n <= 50)
            .ok_or("invalid_source_response")?;
        let page = data["page_data"]["page"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("invalid_source_response")?;
        if page != cursor + 1 {
            return Err("invalid_source_response");
        }
        let items = data["search_list"]
            .as_array()
            .filter(|v| v.len() <= 50)
            .ok_or("invalid_source_response")?;
        for item in items {
            entries.push(json!({"id":text(item,"book_id",49)?,"title":text(item,"title",300)?,"author":text(item,"author",200)?,"description":text(item,"intro",10000)?}));
        }
        has_more = page * size < count;
    } else {
        let doc = html(bytes)?;
        if doc.select(&selector(".searchlist-main")).next().is_none() {
            return Err("invalid_source_response");
        }
        for row in doc.select(&selector(".searchlist-main li")) {
            let title = row
                .select(&selector("a.ellipsis"))
                .next()
                .ok_or("invalid_source_response")?;
            let id = title
                .value()
                .attr("href")
                .and_then(|s| s.strip_prefix("/book/"))
                .and_then(|s| s.strip_suffix(".html"))
                .ok_or("invalid_source_response")?;
            let field = |css: &str| {
                row.select(&selector(css))
                    .next()
                    .map(|e| e.text().collect::<String>().trim().to_owned())
                    .ok_or("invalid_source_response")
            };
            entries.push(json!({"id":id,"title":title.text().collect::<String>().trim(),"author":field(".book-autor")?.trim_start_matches("作者："),"description":field(".book-newc")?}));
        }
        has_more = doc
            .select(&selector(".comp-web-pages a"))
            .any(|e| e.text().collect::<String>() == "下一页");
    }
    let mut seen = BTreeSet::new();
    if entries.len() > 50 || has_more && entries.is_empty() {
        return Err("invalid_source_response");
    }
    for entry in &entries {
        let id = text(entry, "id", 49)?;
        if !valid_book(source, id)
            || !seen.insert(id)
            || text(entry, "title", 300)?.trim().is_empty()
        {
            return Err("invalid_source_response");
        }
        text(entry, "author", 200)?;
        text(entry, "description", 10000)?;
    }
    Ok(
        json!({"source":source,"entries":entries,"cursor":cursor,"next_cursor":if has_more {Some(cursor+1)} else {None}}),
    )
}
fn qimao_directory(book: &str, cancel: &AtomicBool) -> Result<Value> {
    let bytes = get(
        &format!("https://www.qimao.com/api/book/chapter-list?book_id={book}"),
        cancel,
    )?;
    let value = json_bytes(&bytes)?;
    let chapters = value["data"]["chapters"]
        .as_array()
        .ok_or("invalid_source_response")?;
    validate_chapters("qimao", chapters, "id", "title")?;
    Ok(json!(chapters))
}
fn validate_chapters(source: &str, chapters: &[Value], id: &str, title: &str) -> Result<()> {
    if chapters.is_empty() || chapters.len() > 10000 {
        return Err("incomplete_source_directory");
    }
    let mut seen = BTreeSet::new();
    for c in chapters {
        let id = text(c, id, 24)?;
        if !valid_book(source, id)
            || id.contains('_')
            || !seen.insert(id)
            || text(c, title, 300)?.trim().is_empty()
        {
            return Err("invalid_source_response");
        }
    }
    Ok(())
}
fn shuqi_directory(bytes: &[u8], book: &str) -> Result<Value> {
    let doc = html(bytes)?;
    let data = selected(&doc, ".js-dataChapters", 8 * 1024 * 1024)?;
    let value = json_bytes(data.as_bytes())?;
    if value["bookId"] != book || value["readIsOpen"] != true {
        return Err("source_access_denied");
    }
    let mut chapters = vec![];
    for volume in value["chapterList"]
        .as_array()
        .filter(|v| v.len() <= 10000)
        .ok_or("invalid_source_response")?
    {
        for chapter in volume["volumeList"]
            .as_array()
            .ok_or("invalid_source_response")?
        {
            if chapters.len() >= 10000 {
                return Err("incomplete_source_directory");
            }
            chapters.push(chapter.clone());
        }
    }
    validate_chapters("shuqi", &chapters, "chapterId", "chapterName")?;
    if value["chapterNum"].as_u64() != Some(chapters.len() as u64) {
        return Err("incomplete_source_directory");
    }
    Ok(json!({"data":value,"chapters":chapters}))
}
pub fn detail(source: &str, book: &str, cancel: &AtomicBool) -> Result<Value> {
    if source == "qimao" {
        let chapters = qimao_directory(book, cancel)?;
        let info = json_bytes(&get(
            &format!("https://www.qimao.com/api/book-detail/main-info?book_id={book}"),
            cancel,
        )?)?;
        let intro = json_bytes(&get(
            &format!("https://www.qimao.com/api/book-detail/intro?book_id={book}"),
            cancel,
        )?)?;
        let info = &info["data"]["book_detail"];
        if info["book_id"] != book {
            return Err("invalid_source_response");
        }
        if info["catalogue_num"]
            .as_str()
            .and_then(|n| n.parse::<usize>().ok())
            != Some(chapters.as_array().unwrap().len())
        {
            return Err("incomplete_source_directory");
        }
        Ok(
            json!({"source":source,"id":book,"title":text(info,"title",300)?,"author":text(info,"author",200)?,"description":text(&intro["data"],"intro",10000)?,"chapters":chapters.as_array().unwrap().iter().map(|c|json!({"id":c["id"],"title":c["title"]})).collect::<Vec<_>>()}),
        )
    } else {
        let value = shuqi_directory(
            &get(&format!("https://www.shuqi.com/reader?bid={book}"), cancel)?,
            book,
        )?;
        let data = &value["data"];
        Ok(
            json!({"source":source,"id":book,"title":text(data,"bookName",300)?,"author":text(data,"authorName",200)?,"description":text(data,"intro",10000)?,"chapters":value["chapters"].as_array().unwrap().iter().map(|c|json!({"id":c["chapterId"],"title":c["chapterName"]})).collect::<Vec<_>>()}),
        )
    }
}
pub fn chapter(source: &str, book: &str, chapter: &str, cancel: &AtomicBool) -> Result<String> {
    if !valid_book(source, book) || !valid_book(source, chapter) || chapter.contains('_') {
        return Err("invalid_source_book");
    }
    if source == "qimao" {
        let chapters = qimao_directory(book, cancel)?;
        let item = chapters
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == chapter)
            .ok_or("chapter_not_found")?;
        if item["is_vip"] != "0" {
            return Err("source_chapter_locked");
        }
        let response = agent()
            .post("https://www.qimao.com/api/book/chapter")
            .send_form([("book_id", book), ("chapter_id", chapter)])
            .map_err(network_error)?;
        qimao_body(
            &read_response(response, 2 * 1024 * 1024, cancel)?,
            book,
            chapter,
        )
    } else {
        let value = shuqi_directory(
            &get(
                &format!("https://www.shuqi.com/reader?bid={book}&cid={chapter}"),
                cancel,
            )?,
            book,
        )?;
        let item = value["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["chapterId"] == chapter)
            .ok_or("chapter_not_found")?;
        let url = shuqi_free_url(&value["data"], item, book, chapter)?;
        decode_shuqi_body(&get(&url, cancel)?)
    }
}
fn qimao_body(bytes: &[u8], book: &str, chapter: &str) -> Result<String> {
    let value = json_bytes(bytes)?;
    let data = &value["data"];
    if data["chapter"]["book_id"] != book || data["chapter"]["chapter_id"] != chapter {
        return Err("invalid_source_response");
    }
    // The API can return a different book's recommendation on a locked chapter.
    if data["chapter"]["is_vip"] != "0" {
        return Err("source_chapter_locked");
    }
    if !text(data, "key", 10000)?.trim().is_empty() {
        return Err("source_text_obfuscated");
    }
    Ok(text(data, "content", 1024 * 1024)?.to_owned())
}
fn shuqi_free_url(data: &Value, item: &Value, book: &str, chapter: &str) -> Result<String> {
    if item["isFreeRead"] != true {
        return Err("source_chapter_locked");
    }
    let prefix = "https://c13.shuqireader.com/pcapi/chapter/contentfree/";
    if data["freeContUrlPrefix"] != prefix {
        return Err("source_access_denied");
    }
    let suffix = item["contUrlSuffix"]
        .as_str()
        .filter(|s| {
            s.len() <= 1024
                && s.starts_with('?')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"?=&_".contains(&b))
        })
        .ok_or("invalid_source_response")?;
    let fields: Vec<_> = suffix[1..].split('&').collect();
    if fields
        .iter()
        .filter(|f| f.starts_with("bookId="))
        .copied()
        .collect::<Vec<_>>()
        != [format!("bookId={book}")]
        || fields
            .iter()
            .filter(|f| f.starts_with("chapterId="))
            .copied()
            .collect::<Vec<_>>()
            != [format!("chapterId={chapter}")]
    {
        return Err("invalid_source_response");
    }
    Ok(format!("{prefix}{suffix}"))
}
fn decode_shuqi_body(bytes: &[u8]) -> Result<String> {
    use base64::Engine;
    let value = json_bytes(bytes)?;
    if value["state"] != "200" {
        return Err("source_request_rejected");
    }
    let encoded = value["ChapterContent"]
        .as_str()
        .filter(|s| s.len() <= 2 * 1024 * 1024)
        .ok_or("invalid_source_response")?;
    let rotated: String = encoded
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                let shift = (c.to_ascii_lowercase() as u8 - b'a' + 13) % 26;
                (if c.is_ascii_uppercase() { b'A' } else { b'a' } + shift) as char
            } else {
                c
            }
        })
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(rotated)
        .map_err(|_| "source_decode_failed")?;
    let content = String::from_utf8(bytes).map_err(|_| "source_decode_failed")?;
    if content.trim().is_empty() {
        return Err("source_empty_chapter");
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locked_or_mismatched_chapters_never_deliver_recommendations() {
        let mut value = json!({"data":{"chapter":{"book_id":"12","chapter_id":"34","is_vip":"0"},"content":"<p>正文</p>","key":""}});
        assert_eq!(
            qimao_body(&serde_json::to_vec(&value).unwrap(), "12", "34").unwrap(),
            "<p>正文</p>"
        );
        assert_eq!(
            qimao_body(&serde_json::to_vec(&value).unwrap(), "99", "34"),
            Err("invalid_source_response")
        );
        value["data"]["chapter"]["is_vip"] = json!("1");
        value["data"]["content"] = json!("<p>另一书籍的推荐正文</p>");
        assert_eq!(
            qimao_body(&serde_json::to_vec(&value).unwrap(), "12", "34"),
            Err("source_chapter_locked")
        );
        value["data"]["chapter"]["is_vip"] = json!("0");
        value["data"]["key"] = json!("canvas-data");
        assert_eq!(
            qimao_body(&serde_json::to_vec(&value).unwrap(), "12", "34"),
            Err("source_text_obfuscated")
        );
    }
    #[test]
    fn public_search_parses_highlights_and_empty_results() {
        let page=br#"<div class="searchlist-main"><ul><li><a class="ellipsis" href="/book/12.html">A<i>B</i></a><p class="book-autor">author</p><p class="book-newc">description</p></li></ul></div><div class="comp-web-pages"></div>"#;
        let result = decode_search("shuqi", page, 0).unwrap();
        assert_eq!(result["entries"][0]["title"], "AB");
        assert_eq!(result["next_cursor"], Value::Null);
        assert_eq!(
            decode_search("shuqi", b"<div class='searchlist-main'></div>", 0).unwrap()["entries"],
            json!([])
        );
    }
    #[test]
    fn guest_content_never_uses_paid_preview_or_arbitrary_urls() {
        let mut data =
            json!({"freeContUrlPrefix":"https://c13.shuqireader.com/pcapi/chapter/contentfree/"});
        let mut item =
            json!({"isFreeRead":true,"contUrlSuffix":"?bookId=12&chapterId=34&sign=abc"});
        assert!(shuqi_free_url(&data, &item, "12", "34").is_ok());
        item["isFreeRead"] = json!(false);
        assert_eq!(
            shuqi_free_url(&data, &item, "12", "34"),
            Err("source_chapter_locked")
        );
        item["isFreeRead"] = json!(true);
        assert!(shuqi_free_url(&data, &item, "99", "34").is_err());
        item["contUrlSuffix"] = json!("?bookId=12&chapterId=34&bookId=99");
        assert!(shuqi_free_url(&data, &item, "12", "34").is_err());
        data["freeContUrlPrefix"] = json!("https://evil.invalid/");
        assert!(shuqi_free_url(&data, &item, "12", "34").is_err());
    }
    #[test]
    fn search_preserves_source_and_rejects_bad_metadata() {
        let value = json!({"data":{"page_data":{"count":"2","page":"1","page_size":"1"},"search_list":[{"book_id":"123_0","title":"书","author":"作者","intro":"简介"}]}});
        let result = decode_search("qimao", &serde_json::to_vec(&value).unwrap(), 0).unwrap();
        assert_eq!(result["source"], "qimao");
        assert_eq!(result["next_cursor"], 1);
        assert!(decode_search("qimao", &serde_json::to_vec(&value).unwrap(), 1).is_err());
        assert!(decode_search("shuqi", b"<html>verification</html>", 0).is_err());
        for id in ["../12", "12?x=1", "12_0_0", ""] {
            assert!(!valid_book("qimao", id));
        }
    }
    #[test]
    fn official_body_encoding_and_directory_completeness() {
        use base64::Engine;
        let content = "<p>中文正文</p>";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let encoded: String = encoded
            .chars()
            .map(|c| {
                if c.is_ascii_alphabetic() {
                    (if c.is_ascii_uppercase() { b'A' } else { b'a' }
                        + (c.to_ascii_lowercase() as u8 - b'a' + 13) % 26)
                        as char
                } else {
                    c
                }
            })
            .collect();
        assert_eq!(
            decode_shuqi_body(
                &serde_json::to_vec(&json!({"state":"200","ChapterContent":encoded})).unwrap()
            )
            .unwrap(),
            content
        );
        let value = json!({"bookId":"12","readIsOpen":true,"chapterNum":2,"chapterList":[{"volumeList":[{"chapterId":"34","chapterName":"第一章"}]}]});
        let page = format!("<i class='js-dataChapters'>{}</i>", value);
        assert_eq!(
            shuqi_directory(page.as_bytes(), "12"),
            Err("incomplete_source_directory")
        );
    }
}
