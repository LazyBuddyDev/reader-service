use crate::model::*;
use roxmltree::{Document, Node};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct Parsed {
    pub book: Book,
    pub chapters: Vec<Vec<Block>>,
    pub resources: BTreeMap<String, Vec<u8>>,
    pub original: Vec<u8>,
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err("cancelled")
    } else {
        Ok(())
    }
}
fn block(kind: &str, text: String) -> Block {
    Block {
        id: String::new(),
        kind: kind.into(),
        text,
        runs: vec![],
        resource: None,
    }
}
fn push_text(blocks: &mut Vec<Block>, kind: &str, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    for part in chars.chunks(1024) {
        let text: String = part.iter().collect();
        if !text.trim().is_empty() {
            blocks.push(block(kind, text));
        }
    }
}
fn finish(
    title: &str,
    format: &str,
    encoding: &str,
    bytes: Vec<u8>,
    mut chapters: Vec<(String, Vec<Block>)>,
    resources: BTreeMap<String, Vec<u8>>,
    mimes: BTreeMap<String, String>,
    author: String,
    warnings: Vec<String>,
) -> Result<Parsed> {
    chapters.retain(|(_, blocks)| !blocks.is_empty());
    if chapters.is_empty() {
        return Err("empty_book");
    }
    if chapters.len() > 10000 || chapters.iter().map(|c| c.1.len()).sum::<usize>() > 200000 {
        return Err("content_too_large");
    }
    let source_id = hash(&bytes);
    let content_version = hash(format!("reader-content-2:{source_id}:{encoding}").as_bytes());
    let mut meta = Vec::new();
    let mut content = Vec::new();
    for (index, (title, mut blocks)) in chapters.into_iter().enumerate() {
        for (i, b) in blocks.iter_mut().enumerate() {
            b.id = format!("b{i}");
        }
        meta.push(Chapter {
            id: format!("c{index}"),
            title,
            blocks: blocks.len(),
        });
        content.push(blocks);
    }
    Ok(Parsed {
        book: Book {
            id: format!("book-{}", &content_version[..24]),
            title: title.chars().take(300).collect(),
            author,
            source: Source {
                kind: "local".into(),
                id: source_id,
                format: format.into(),
                encoding: encoding.into(),
            },
            content_version,
            chapters: meta,
            resources: mimes,
            warnings,
        },
        chapters: content,
        resources,
        original: bytes,
    })
}
pub fn parse(path: &Path, encoding: &str, cancel: &AtomicBool) -> Result<Parsed> {
    check(cancel)?;
    let file = std::fs::File::open(path).map_err(|_| "file_unreadable")?;
    if !file.metadata().map_err(|_| "file_unreadable")?.is_file() {
        return Err("not_a_file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_unreadable")?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("file_too_large");
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("未命名");
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" => txt(title, bytes, encoding, cancel),
        "epub" => epub(title, bytes, cancel),
        _ => Err("unsupported_format"),
    }
}
pub fn decode(bytes: &[u8], label: &str) -> Result<(String, String)> {
    use encoding_rs::*;
    let (codec, skip) = if label == "auto" {
        if let Some((codec, n)) = Encoding::for_bom(bytes) {
            (codec, n)
        } else if std::str::from_utf8(bytes).is_ok() {
            (UTF_8, 0)
        } else {
            return Err("encoding_required");
        }
    } else {
        let codec = match label {
            "utf-8" => UTF_8,
            "utf-16le" => UTF_16LE,
            "utf-16be" => UTF_16BE,
            "gb18030" => GB18030,
            "big5" => BIG5,
            _ => return Err("invalid_encoding"),
        };
        let skip = Encoding::for_bom(bytes)
            .filter(|(bom, _)| *bom == codec)
            .map(|(_, n)| n)
            .unwrap_or(0);
        (codec, skip)
    };
    let (text, errors) = codec.decode_without_bom_handling(&bytes[skip..]);
    if errors || text.contains('\0') {
        return Err("invalid_text_encoding");
    }
    Ok((
        text.replace("\r\n", "\n").replace('\r', "\n"),
        codec.name().to_ascii_lowercase(),
    ))
}
fn heading(line: &str) -> bool {
    let line = line.trim();
    if line.chars().count() > 60 {
        return false;
    }
    (line.starts_with('第')
        && line
            .chars()
            .skip(1)
            .take(24)
            .any(|c| "章节卷回部篇".contains(c)))
        || line.to_ascii_lowercase().starts_with("chapter ")
        || matches!(line, "序章" | "序言" | "楔子" | "尾声" | "后记")
}
fn txt(title: &str, bytes: Vec<u8>, encoding: &str, cancel: &AtomicBool) -> Result<Parsed> {
    let (text, codec) = decode(&bytes, encoding)?;
    let mut chapters = vec![];
    let mut name = "正文".to_owned();
    let mut blocks = vec![];
    for (i, line) in text.lines().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        if heading(line) {
            if !blocks.is_empty() {
                chapters.push((name, std::mem::take(&mut blocks)));
            }
            name = line.trim().to_owned();
        } else {
            push_text(&mut blocks, "paragraph", line.trim());
        }
    }
    chapters.push((name, blocks));
    finish(
        title,
        "txt",
        &codec,
        bytes,
        chapters,
        BTreeMap::new(),
        BTreeMap::new(),
        String::new(),
        vec![],
    )
}

// EPUB paths are archive identifiers, never filesystem destinations.
fn resolve(base: &str, href: &str) -> Result<String> {
    let href = href.split('#').next().unwrap_or("");
    let mut decoded = Vec::new();
    let mut encoded = href.bytes();
    while let Some(byte) = encoded.next() {
        if byte == b'%' {
            let high = (encoded.next().ok_or("unsafe_resource")? as char)
                .to_digit(16)
                .ok_or("unsafe_resource")?;
            let low = (encoded.next().ok_or("unsafe_resource")? as char)
                .to_digit(16)
                .ok_or("unsafe_resource")?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let href = std::str::from_utf8(&decoded).map_err(|_| "unsafe_resource")?;
    if href.is_empty() || href.contains([':', '\\', '?', '\0']) || href.starts_with('/') {
        return Err("unsafe_resource");
    }
    let mut parts: Vec<&str> = base
        .rsplit_once('/')
        .map(|(p, _)| p.split('/').collect())
        .unwrap_or_default();
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("unsafe_resource")?;
            }
            _ => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err("unsafe_resource");
    }
    Ok(parts.join("/"))
}
struct Archive {
    zip: zip::ZipArchive<Cursor<Vec<u8>>>,
}
impl Archive {
    fn new(bytes: Vec<u8>) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "invalid_epub")?;
        if zip.len() > 10000 {
            return Err("archive_too_large");
        }
        let mut total = 0u64;
        let mut names = BTreeSet::new();
        for i in 0..zip.len() {
            let f = zip.by_index(i).map_err(|_| "encrypted_or_invalid_epub")?;
            if f.enclosed_name().is_none()
                || f.name().contains(['\\', ':'])
                || f.is_symlink()
                || !names.insert(f.name().to_owned())
            {
                return Err("unsafe_archive");
            }
            total = total.checked_add(f.size()).ok_or("archive_too_large")?;
            if total > MAX_EXPANDED || f.size() > MAX_FILE {
                return Err("archive_too_large");
            }
        }
        if zip.by_name("META-INF/encryption.xml").is_ok() {
            return Err("encrypted_epub");
        }
        Ok(Self { zip })
    }
    fn bytes(&mut self, path: &str, max: u64) -> Result<Vec<u8>> {
        let f = self
            .zip
            .by_name(path)
            .map_err(|_| "missing_epub_resource")?;
        if f.size() > max {
            return Err("resource_too_large");
        }
        let mut bytes = Vec::new();
        f.take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "invalid_epub_resource")?;
        if bytes.len() as u64 > max {
            return Err("resource_too_large");
        }
        Ok(bytes)
    }
    fn xml(&mut self, path: &str) -> Result<String> {
        let bytes = self.bytes(path, 8 * 1024 * 1024)?;
        let text = String::from_utf8(bytes).map_err(|_| "unsupported_xml_encoding")?;
        // Accept the standard XHTML doctype, but never process declarations or entities.
        let text = if let Some(start) = text.find("<!DOCTYPE") {
            let end = text[start..].find('>').ok_or("invalid_xml")? + start;
            if text[start..=end].contains('[') {
                return Err("unsafe_xml");
            }
            format!("{}{}", &text[..start], &text[end + 1..])
        } else {
            text
        };
        Ok(text.replace("&nbsp;", "&#160;"))
    }
}
fn xml(text: &str) -> Result<Document<'_>> {
    Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 300000,
        },
    )
    .map_err(|_| "invalid_xml")
}
fn element<'a>(node: Node<'a, 'a>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}
fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<String>()
        .trim()
        .to_owned()
}
fn epub(fallback: &str, bytes: Vec<u8>, cancel: &AtomicBool) -> Result<Parsed> {
    let mut archive = Archive::new(bytes.clone())?;
    if archive.bytes("mimetype", 100)? != b"application/epub+zip" {
        return Err("invalid_epub");
    }
    let container = archive.xml("META-INF/container.xml")?;
    let doc = xml(&container)?;
    let root = doc
        .descendants()
        .find(|n| element(*n, "rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .ok_or("invalid_epub")?;
    let package = resolve("", root)?;
    let opf = archive.xml(&package)?;
    let doc = xml(&opf)?;
    if doc.descendants().any(|n| {
        element(n, "meta")
            && n.attribute("property") == Some("rendition:layout")
            && text(n) == "pre-paginated"
    }) {
        return Err("fixed_layout_epub");
    }
    let title = doc
        .descendants()
        .find(|n| element(*n, "title"))
        .map(text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.into());
    let author = doc
        .descendants()
        .find(|n| element(*n, "creator"))
        .map(text)
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    let mut items = BTreeMap::new();
    let mut nav = None;
    let mut ncx = None;
    for item in doc.descendants().filter(|n| element(*n, "item")) {
        let id = item.attribute("id").ok_or("invalid_epub")?;
        let path = resolve(&package, item.attribute("href").ok_or("invalid_epub")?)?;
        let mime = item.attribute("media-type").unwrap_or("").to_owned();
        if item
            .attribute("properties")
            .unwrap_or("")
            .split_whitespace()
            .any(|p| p == "nav")
        {
            nav = Some(path.clone());
        }
        if mime == "application/x-dtbncx+xml" {
            ncx = Some(path.clone());
        }
        items.insert(id.to_owned(), (path, mime));
    }
    let mut labels = BTreeMap::new();
    if let Some(path) = nav {
        let nav = archive.xml(&path)?;
        let nav = xml(&nav)?;
        for a in nav.descendants().filter(|n| element(*n, "a")) {
            if let Some(href) = a.attribute("href") {
                if let Ok(target) = resolve(&path, href) {
                    labels.entry(target).or_insert_with(|| text(a));
                }
            }
        }
    } else if let Some(path) = ncx {
        let ncx = archive.xml(&path)?;
        let ncx = xml(&ncx)?;
        for point in ncx.descendants().filter(|n| element(*n, "navPoint")) {
            let src = point
                .children()
                .find(|n| element(*n, "content"))
                .and_then(|n| n.attribute("src"));
            let label = point.children().find(|n| element(*n, "navLabel")).map(text);
            if let (Some(src), Some(label)) = (src, label) {
                labels.entry(resolve(&path, src)?).or_insert(label);
            }
        }
    }
    let mut chapters = vec![];
    let mut resources = BTreeMap::new();
    let mut mimes = BTreeMap::new();
    let mut warnings = BTreeSet::new();
    for reference in doc.descendants().filter(|n| element(*n, "itemref")) {
        check(cancel)?;
        if reference.attribute("linear") == Some("no") {
            continue;
        }
        if reference
            .attribute("properties")
            .unwrap_or("")
            .contains("pre-paginated")
        {
            return Err("fixed_layout_epub");
        }
        let (path, mime) = items
            .get(reference.attribute("idref").ok_or("invalid_epub")?)
            .ok_or("invalid_epub")?;
        if mime != "application/xhtml+xml" {
            return Err("unsupported_spine_format");
        }
        let content = archive.xml(path)?;
        let chapter = xml(&content)?;
        let body = chapter
            .descendants()
            .find(|n| element(*n, "body"))
            .ok_or("invalid_epub")?;
        let name = labels
            .get(path)
            .cloned()
            .or_else(|| {
                body.descendants()
                    .find(|n| n.is_element() && matches!(n.tag_name().name(), "h1" | "h2" | "h3"))
                    .map(text)
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1));
        let mut blocks = vec![];
        walk(
            body,
            path,
            &mut archive,
            &mut blocks,
            &mut resources,
            &mut mimes,
            &mut warnings,
            cancel,
        )?;
        chapters.push((name.chars().take(300).collect(), blocks));
    }
    finish(
        &title,
        "epub",
        "utf-8",
        bytes,
        chapters,
        resources,
        mimes,
        author,
        warnings.into_iter().collect(),
    )
}
fn walk(
    node: Node<'_, '_>,
    path: &str,
    archive: &mut Archive,
    blocks: &mut Vec<Block>,
    resources: &mut BTreeMap<String, Vec<u8>>,
    mimes: &mut BTreeMap<String, String>,
    warnings: &mut BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<()> {
    check(cancel)?;
    if node.ancestors().take(130).count() > 128 {
        return Err("content_too_deep");
    }
    let name = if node.is_element() {
        node.tag_name().name()
    } else {
        ""
    };
    if matches!(
        name,
        "script" | "style" | "iframe" | "object" | "head" | "svg" | "audio" | "video" | "form"
    ) {
        return Ok(());
    }
    if name == "img" {
        let alt = node
            .attribute("alt")
            .unwrap_or("插图")
            .chars()
            .take(300)
            .collect::<String>();
        let resource = node
            .attribute("src")
            .and_then(|src| resolve(path, src).ok())
            .and_then(|target| {
                let bytes = archive.bytes(&target, 1024 * 1024).ok()?;
                let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    "image/png"
                } else if bytes.starts_with(b"\xff\xd8\xff") {
                    "image/jpeg"
                } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                    "image/webp"
                } else {
                    return None;
                };
                let size = imagesize::blob_size(&bytes).ok()?;
                if size.width > 4096
                    || size.height > 4096
                    || size.width.saturating_mul(size.height) > 8_000_000
                {
                    return None;
                }
                let id = hash(&bytes);
                if resources.len() >= 500
                    || resources.values().map(Vec::len).sum::<usize>() + bytes.len()
                        > 32 * 1024 * 1024
                {
                    return None;
                }
                resources.entry(id.clone()).or_insert(bytes);
                mimes.insert(id.clone(), mime.into());
                Some(id)
            });
        if resource.is_none() {
            warnings.insert("部分图片格式、大小或地址不受支持，已显示替代文字".into());
        }
        blocks.push(Block {
            resource,
            ..block("image", alt)
        });
        return Ok(());
    }
    if node.is_text() {
        let value = node
            .text()
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        push_text(blocks, "paragraph", &value);
        return Ok(());
    }
    let paragraph = matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "pre" | "blockquote"
    );
    if paragraph && !node.descendants().any(|n| element(n, "img")) {
        let mut runs = vec![];
        for child in node
            .descendants()
            .filter(|n| n.is_text() || element(*n, "br"))
        {
            if child.ancestors().any(|n| {
                n.is_element()
                    && matches!(
                        n.tag_name().name(),
                        "script" | "style" | "iframe" | "object"
                    )
            }) {
                continue;
            }
            let value = if element(child, "br") {
                "\n"
            } else {
                child.text().unwrap_or("")
            };
            if value.is_empty() {
                continue;
            }
            runs.push(Run {
                text: value.to_owned(),
                strong: child
                    .ancestors()
                    .any(|n| n.is_element() && matches!(n.tag_name().name(), "b" | "strong")),
                emphasis: child
                    .ancestors()
                    .any(|n| n.is_element() && matches!(n.tag_name().name(), "i" | "em")),
                link: child
                    .ancestors()
                    .find(|n| element(*n, "a"))
                    .and_then(|n| n.attribute("href"))
                    .filter(|s| s.starts_with('#') && s.len() <= 256)
                    .map(str::to_owned),
            });
        }
        let value: String = runs.iter().map(|r| r.text.as_str()).collect();
        let kind = if name.starts_with('h') {
            "heading"
        } else if name == "pre" {
            "pre"
        } else {
            "paragraph"
        };
        if value.chars().count() > 1024 || runs.len() > 64 {
            push_text(blocks, kind, &value);
        } else if !value.trim().is_empty() {
            blocks.push(Block {
                runs,
                ..block(kind, value)
            });
        }
    } else {
        for child in node.children() {
            walk(
                child, path, archive, blocks, resources, mimes, warnings, cancel,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodings_require_explicit_choice_without_a_bom() {
        let bytes = encoding_rs::GB18030
            .encode("第一章 开始\n你好世界")
            .0
            .into_owned();
        assert_eq!(decode(&bytes, "auto").unwrap_err(), "encoding_required");
        let parsed = txt("书", bytes, "gb18030", &AtomicBool::new(false)).unwrap();
        assert_eq!(parsed.book.chapters[0].title, "第一章 开始");
        assert_eq!(parsed.chapters[0][0].text, "你好世界");
        assert_eq!(decode(&[255, 254, 65, 0], "auto").unwrap().0, "A");
        assert_eq!(decode(&[254, 255, 0, 65], "auto").unwrap().0, "A");
    }
    #[test]
    fn long_lines_are_bounded_and_positions_stable() {
        let data = format!("序章\n{}\n第一章 出发\n正文", "字".repeat(10000)).into_bytes();
        let a = txt("书", data.clone(), "auto", &AtomicBool::new(false)).unwrap();
        let b = txt("书", data, "utf-8", &AtomicBool::new(false)).unwrap();
        assert_eq!(a.book.id, b.book.id);
        assert_eq!(a.chapters[0].len(), 10);
        assert_eq!(a.book.chapters.len(), 2);
    }
    #[test]
    fn archive_paths_and_entities_are_rejected() {
        for href in [
            "../../outside",
            "https://example.com/a",
            "/a",
            "a\\b",
            "%2e%2e/%2e%2e/x",
        ] {
            assert!(resolve("OEBPS/a.xhtml", href).is_err());
        }
        assert_eq!(
            resolve("OEBPS/a.xhtml", "my%20book.xhtml").unwrap(),
            "OEBPS/my book.xhtml"
        );
        assert_eq!(
            resolve("OEBPS/text/a.xhtml", "../images/a.png").unwrap(),
            "OEBPS/images/a.png"
        );
        assert!(xml("<!DOCTYPE a [<!ENTITY e SYSTEM 'file:///x'>]><a>&e;</a>").is_err());
    }
}
