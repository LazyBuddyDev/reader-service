//! GPL-3.0-only. Gateway protocol/table adapted from Maicy0609/fanqie-lite-api,
//! commit ecd99f44fdbc984320a0595b9bbbbfead3a20f6d, upstream/shuqi/{crypto,params,request}.
//! Anonymous sentinel independently verified on the official mobile reader; no copied user/device or membership.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, &'static str>;
const USER: &str = "8000000";
const PUBLIC: &[(&str, &str)] = &[
    ("soft_id", "1"),
    ("user_id", USER),
    ("userId", USER),
    ("ver", "261013"),
    ("subVer", "sqrelease"),
    ("appVer", "12.7.0.268"),
    ("theme", "day"),
    ("platform", "an"),
    ("placeid", "1069"),
    ("sdk", "32"),
    ("cpu", "x86_64"),
    ("pkg_cpu", "arm64-v8a"),
    ("wh", "900x1600"),
    ("msv", "3"),
    ("enc", ""),
    ("vc", "5d73"),
    ("mod", "BVL-AN16"),
    ("manufacturer", "HONOR"),
    ("brand", "HONOR"),
    ("net_type", "wifi"),
    ("net_type_str", "wifi"),
    ("first_placeid", "src1069"),
    ("aak", "a3a840"),
    ("net", "4"),
    ("net_env", "4"),
    ("permissionType", "0"),
    ("personalized", "1"),
    ("contentRecom", "1"),
    ("scene_code", ""),
    ("rom", "12"),
    ("sqUniqDeviceId", ""),
    ("utdid", ""),
    ("sn", ""),
    ("umidtoken", ""),
    ("otdd", ""),
];
fn md5(bytes: &[u8]) -> String {
    format!("{:x}", Md5::digest(bytes))
}
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn encrypted(data: &[u8]) -> Result<String> {
    let ks = STANDARD
        .decode(include_str!("shuqi-keystream.b64").trim())
        .map_err(|_| "source_runtime_failed")?;
    if ks.len() != 2048 || data.len() > ks.len() {
        return Err("source_request_too_large");
    }
    Ok(STANDARD.encode(data.iter().zip(ks).map(|(a, b)| a ^ b).collect::<Vec<_>>()))
}
fn gateway(fields: &[(&str, String)], ts: &str) -> Result<(String, String)> {
    if ts.len() != 13 || !ts.bytes().all(|b| b.is_ascii_digit()) {
        return Err("source_runtime_failed");
    }
    let key = [1, 3, 5, 8, 6]
        .iter()
        .map(|i| ts.as_bytes()[*i] as char)
        .collect::<String>()
        .parse::<u64>()
        .map_err(|_| "source_runtime_failed")?;
    let product = ((if key == 0 { 12347 } else { key }) * 8000000).to_string();
    let enc = format!("{}{ts}", &product[product.len().saturating_sub(5)..]);
    let mut p = PUBLIC
        .iter()
        .map(|(k, v)| (*k, v.to_string()))
        .collect::<BTreeMap<_, _>>();
    p.insert("enc", enc);
    p.insert("timestamp", ts.into());
    p.insert("isTeenMode", "0".into());
    p.insert("sqSv", "1.0".into());
    for (k, v) in fields {
        p.insert(k, v.clone());
    }
    let sig = p
        .iter()
        .map(|(k, v)| format!("{k}={}&", encode(v)))
        .collect::<String>()
        + "skey=467694bd8912441cae8498b3c7e4282c";
    p.insert("sign", md5(sig.as_bytes()));
    p.insert("key", "sq_app_gateway".into());
    let public = PUBLIC
        .iter()
        .map(|(k, _)| {
            format!(
                "{k}={}&",
                encode(if *k == "platform" { "2" } else { &p[k] }).replace('*', "%2A")
            )
        })
        .collect::<String>();
    Ok((
        encrypted(&serde_json::to_vec(&p).map_err(|_| "source_runtime_failed")?)?,
        encrypted(public.as_bytes())?,
    ))
}
fn response(mut r: ureq::http::Response<ureq::Body>) -> Result<Value> {
    if r.status().is_redirection() {
        return Err("source_access_denied");
    }
    let bytes = r
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .map_err(super::qimao::network_error)?;
    if bytes.is_empty() {
        return Err("source_empty_response");
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid_source_response")
}
fn success(v: Value, key: &str) -> Result<Value> {
    if v[key] != 200 && v[key] != "200" || v["success"] == false {
        return Err("source_request_rejected");
    }
    if !v["data"].is_object() {
        return Err("invalid_source_response");
    }
    Ok(v["data"].clone())
}
fn app(method: &str, fields: &[(&str, String)]) -> Result<Value> {
    let path = match method {
        "search" => "/sqan/render/render/search/native_v3",
        "book" => "/sqan/sqapi/bff/api/v1/reader/titlepage",
        "chapter" => "/sqan/sqapi/bff/api/v1/reader/chapterinfo",
        _ => return Err("invalid_arguments"),
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "source_runtime_failed")?
        .as_millis()
        .to_string();
    let (data, public) = gateway(fields, &ts)?;
    let r = super::qimao::agent()
        .post(format!("https://ocean.shuqireader.com{path}"))
        .header("User-Agent", "okhttp/3.12.13")
        .header("x-sq-req-platform", "an")
        .header("x-sq-s-key", "23011413")
        .header("x-sq-sv", "3.0")
        .header("x-sq-public", public)
        .header("x-sq-req-encrypt", "sdk")
        .header("x-sq-req-encrypt-param", "x-sq-public")
        .header("x-sq-api-encrypt", "1")
        .send_form([("data", data)])
        .map_err(super::qimao::network_error)?;
    success(response(r)?, "status")
}
fn id(v: &Value) -> Result<String> {
    let id = if let Some(s) = v.as_str() {
        s.to_owned()
    } else {
        v.as_u64().ok_or("invalid_source_response")?.to_string()
    };
    if !super::valid_id(&id) {
        return Err("invalid_source_response");
    }
    Ok(id)
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| {
            s.chars().count() <= max
                && !s
                    .chars()
                    .any(|c| matches!(c as u32, 0xe000..=0xf8ff | 0xfffd))
        })
        .ok_or("invalid_source_response")
}
fn free_meta(data: Value, book: &str) -> Result<Value> {
    let m = data["bookInfo"].clone();
    if id(&m["bookId"])? != book || text(&m, "bookName", 300)?.trim().is_empty() {
        return Err("invalid_source_response");
    }
    if m["showVipTag"] == true || m["isVipPriorityRead"] == true {
        return Err("source_chapter_locked");
    }
    if m["showVipTag"] != false || m["isVipPriorityRead"] != false || m["formats"] != 1 {
        return Err("unsupported_source_book");
    }
    if !m["chapterTotalNum"]
        .as_u64()
        .is_some_and(|n| n > 0 && n <= 10000)
    {
        return Err("invalid_source_response");
    }
    Ok(m)
}
fn metadata(book: &str) -> Result<Value> {
    free_meta(app("book", &[("bookId", book.into())])?, book)
}
fn catalog(data: &Value, book: &str, count: usize) -> Result<Vec<Value>> {
    if id(&data["bookId"])? != book {
        return Err("invalid_source_response");
    }
    let declared = id(&data["chapterNum"])?
        .parse::<usize>()
        .map_err(|_| "invalid_source_response")?;
    let volumes = data["chapterList"]
        .as_array()
        .filter(|a| a.len() <= 10000)
        .ok_or("invalid_source_response")?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for volume in volumes {
        for item in volume["volumeList"]
            .as_array()
            .filter(|a| a.len() <= 10000)
            .ok_or("invalid_source_response")?
        {
            let cid = id(&item["chapterId"])?;
            let title = text(item, "chapterName", 300)?;
            if out.len() >= 10000 || title.trim().is_empty() || !seen.insert(cid.clone()) {
                return Err("invalid_source_response");
            }
            // This endpoint supplies identities/order only; its free/VIP flags never grant or deny App access.
            out.push(json!({"id":cid,"title":title}));
        }
    }
    if out.len() != count || declared != count {
        return Err("incomplete_source_directory");
    }
    Ok(out)
}
fn directory(book: &str, meta: &Value) -> Result<Vec<Value>> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "source_runtime_failed")?
        .as_secs()
        .to_string();
    let sign = md5(format!("{book}{ts}{USER}37e81a9d8f02596e1b895d07c171d5c9").as_bytes());
    let r = super::qimao::agent()
        .get("https://content.shuqireader.com/openapi/book/chapterlist")
        .query("bookId", book)
        .query("timestamp", ts)
        .query("user_id", USER)
        .query("sign", sign)
        .query("platform", "0")
        .query("shuqi_h5", "205")
        .call()
        .map_err(super::qimao::network_error)?;
    catalog(
        &success(response(r)?, "state")?,
        book,
        meta["chapterTotalNum"]
            .as_u64()
            .ok_or("invalid_source_response")? as usize,
    )
}
fn decode(data: &Value, chapter: &str) -> Result<String> {
    let c = &data["chapter"];
    if id(&c["chapterId"])? != chapter {
        return Err("invalid_source_response");
    }
    if c["isFreeRead"] != true {
        return Err("source_chapter_locked");
    }
    let encoded = text(c, "content", 2 * 1024 * 1024)?;
    let rotated = encoded
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' => (b - b'a' + 13) % 26 + b'a',
            b'A'..=b'Z' => (b - b'A' + 13) % 26 + b'A',
            _ => b,
        })
        .collect::<Vec<_>>();
    let raw = STANDARD
        .decode(rotated)
        .map_err(|_| "source_decode_failed")?;
    if raw.len() > 1024 * 1024 {
        return Err("source_response_too_large");
    }
    let plain = String::from_utf8(raw).map_err(|_| "source_decode_failed")?;
    if plain.trim().is_empty() {
        return Err("source_empty_chapter");
    }
    Ok(plain)
}
fn collect(
    v: &Value,
    depth: usize,
    out: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    match v {
        Value::Array(a) => {
            for item in a {
                collect(item, depth + 1, out, seen)?;
            }
        }
        Value::Object(m) => {
            if m.get("formats") == Some(&json!(1))
                && m.contains_key("bookName")
                && m.contains_key("authorName")
                && m.contains_key("bookId")
            {
                let bid = id(&v["bookId"])?;
                if seen.insert(bid.clone()) {
                    if out.len() >= 100 {
                        return Err("source_response_too_large");
                    }
                    out.push(bid);
                }
            } else {
                for item in m.values() {
                    collect(item, depth + 1, out, seen)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) fn request(args: &Value) -> Result<Value> {
    match args["method"].as_str() {
        Some("shuqi.search") => {
            let query = text(args, "query", 100)?.trim();
            if query.is_empty() || query.chars().any(char::is_control) {
                return Err("invalid_arguments");
            }
            let offset = args["cursor"]
                .as_u64()
                .filter(|n| *n <= u32::MAX as u64)
                .ok_or("invalid_arguments")? as usize;
            let data = app(
                "search",
                &[("page", "searchResultV3".into()), ("query", query.into())],
            )?;
            let mut candidates = Vec::new();
            collect(&data, 0, &mut candidates, &mut BTreeSet::new())?;
            let end = offset.saturating_add(3).min(candidates.len());
            let mut entries = Vec::new();
            for book in candidates.iter().skip(offset).take(3) {
                match metadata(book) {
                    Ok(meta)=>entries.push(json!({"id":book,"title":text(&meta,"bookName",300)?,"author":text(&meta,"authorName",200)?,"description":text(&meta,"briefIntro",10000)?})),
                    Err("source_chapter_locked"|"unsupported_source_book")=>{},Err(e)=>return Err(e),
                }
            }
            Ok(
                json!({"source":"shuqi_app","entries":entries,"cursor":offset,"next_cursor":if end<candidates.len(){Some(end)}else{None}}),
            )
        }
        Some("shuqi.detail" | "shuqi.chapter") => {
            let book = text(args, "book_id", 24)?;
            if !super::valid_id(book) {
                return Err("invalid_arguments");
            }
            let meta = metadata(book)?;
            let chapters = directory(book, &meta)?;
            if args["method"] == "shuqi.detail" {
                Ok(
                    json!({"source":"shuqi_app","id":book,"title":text(&meta,"bookName",300)?,"author":text(&meta,"authorName",200)?,"description":text(&meta,"briefIntro",10000)?,"chapters":chapters}),
                )
            } else {
                let chapter = text(args, "chapter_id", 24)?;
                if !super::valid_id(chapter) || !chapters.iter().any(|c| c["id"] == chapter) {
                    return Err("invalid_arguments");
                }
                Ok(
                    json!({"content":decode(&app("chapter",&[("bookId",book.into()),("chapterId",chapter.into())])?,chapter)?}),
                )
            }
        }
        _ => Err("invalid_arguments"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_keeps_anonymous_identity_and_excludes_member_or_device_claims() {
        let (data, public) = gateway(&[("bookId", "9031364".into())], "1795000000123").unwrap();
        let decrypt = |s: &str| {
            let a = STANDARD.decode(s).unwrap();
            let k = STANDARD
                .decode(include_str!("shuqi-keystream.b64").trim())
                .unwrap();
            String::from_utf8(a.iter().zip(k).map(|(x, y)| x ^ y).collect()).unwrap()
        };
        let body: Value = serde_json::from_str(&decrypt(&data)).unwrap();
        assert_eq!(body["user_id"], USER);
        assert_eq!(body["platform"], "an");
        assert!(body.get("utype").is_none());
        assert_eq!(body["umidtoken"], "");
        let p = decrypt(&public);
        assert!(p.contains("platform=2&"));
        assert!(!p.contains("utype="));
        assert!(encrypted(&vec![0; 2049]).is_err());
    }
    #[test]
    fn app_permissions_override_web_flags_and_trials_are_rejected() {
        let d = json!({"bookId":"123","chapterNum":"1","chapterList":[{"volumeList":[{"chapterId":456,"chapterName":"首章","isFreeRead":false,"chapterPrice":10}]}]});
        assert!(catalog(&d, "123", 1).is_ok());
        assert_eq!(catalog(&d, "123", 2), Err("incomplete_source_directory"));
        let mut v =
            json!({"chapter":{"chapterId":456,"isFreeRead":false,"content":"paid preview"}});
        assert_eq!(decode(&v, "456"), Err("source_chapter_locked"));
        let b = STANDARD.encode("正文<br>完整段落");
        let rot = b
            .bytes()
            .map(|b| match b {
                b'a'..=b'z' => (b - b'a' + 13) % 26 + b'a',
                b'A'..=b'Z' => (b - b'A' + 13) % 26 + b'A',
                _ => b,
            })
            .map(char::from)
            .collect::<String>();
        v["chapter"]["isFreeRead"] = json!(true);
        v["chapter"]["content"] = json!(rot);
        assert_eq!(decode(&v, "456").unwrap(), "正文<br>完整段落");
        assert!(decode(&v, "457").is_err());
        v["chapter"]["content"] = json!("bad base64");
        assert_eq!(decode(&v, "456"), Err("source_decode_failed"));
    }
    #[test]
    fn app_free_candidates_require_explicit_native_flags() {
        let mut d = json!({"bookInfo":{"bookId":123,"bookName":"测试","showVipTag":false,"isVipPriorityRead":false,"formats":1,"chapterTotalNum":1}});
        assert!(free_meta(d.clone(), "123").is_ok());
        d["bookInfo"]["showVipTag"] = json!(true);
        assert_eq!(free_meta(d.clone(), "123"), Err("source_chapter_locked"));
        d["bookInfo"].as_object_mut().unwrap().remove("showVipTag");
        assert!(free_meta(d, "123").is_err());
    }
    #[test]
    fn successful_envelopes_keep_native_payload_not_book_serial_state() {
        assert_eq!(
            success(json!({"state":200,"data":{"state":"1"}}), "state").unwrap()["state"],
            "1"
        );
        assert!(success(json!({"status":"200","data":{}}), "status").is_ok());
        assert!(success(json!({"status":403,"data":{}}), "status").is_err());
    }
}
