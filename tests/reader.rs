use reader_service::{Engine, parse};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!(
                "test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn import(&self, engine: &mut Engine, path: &str) -> Value {
        let job = engine
            .call(
                "library.import",
                json!({"path":self.0.join(path),"encoding":"auto"}),
            )
            .unwrap()["job"]
            .clone();
        for _ in 0..1000 {
            let status = engine.call("import.status", json!({"job":job})).unwrap();
            if status["status"] != "pending" {
                assert_eq!(status["status"], "complete", "{status}");
                return status["result"].clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("import timeout")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn epub(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn entries() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("mimetype",b"application/epub+zip".to_vec()),
        ("META-INF/container.xml",br#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#.to_vec()),
        ("OPS/book.opf",br#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Sample</dc:title><dc:creator>Author</dc:creator></metadata><manifest><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#.to_vec()),
        ("OPS/nav.xhtml",br#"<html><body><nav><ol><li><a href="a.xhtml">First</a><ol><li><a href="b.xhtml">Second</a></li></ol></li></ol></nav></body></html>"#.to_vec()),
        ("OPS/a.xhtml",br#"<!DOCTYPE html><html><body><h1>Chapter A</h1><p>Before <strong>bold</strong><em> emphasis</em><script>secret</script> after</p><img src="https://example.invalid/x.png" alt="offline image"/></body></html>"#.to_vec()),
        ("OPS/b.xhtml",br#"<html><body><p>Chapter B</p></body></html>"#.to_vec())
    ]
}
#[test]
fn epub_uses_spine_and_nested_navigation_without_active_content() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("book.epub"), epub(entries())).unwrap();
    let parsed = parse::parse(
        &fixture.0.join("book.epub"),
        "auto",
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(parsed.book.chapters[0].title, "First");
    assert_eq!(parsed.book.chapters[1].title, "Second");
    assert_eq!(parsed.book.author, "Author");
    let paragraph = &parsed.chapters[0][1];
    assert_eq!(paragraph.text, "Before bold emphasis after");
    assert!(paragraph.runs.iter().any(|r| r.strong));
    assert!(parsed.resources.is_empty());
    assert_eq!(parsed.book.warnings.len(), 1);
}
#[test]
fn dangerous_and_unsupported_epubs_are_explicit_failures() {
    let fixture = Fixture::new();
    let path = fixture.0.join("book.epub");
    let mut data = entries();
    data.push(("../outside", vec![1]));
    fs::write(&path, epub(data)).unwrap();
    assert!(parse::parse(&path, "auto", &AtomicBool::new(false)).is_err());
    let mut data = entries();
    data.push(("META-INF/encryption.xml", vec![1]));
    fs::write(&path, epub(data)).unwrap();
    assert_eq!(
        parse::parse(&path, "auto", &AtomicBool::new(false)).err(),
        Some("encrypted_epub")
    );
    let mut data = entries();
    data[2].1 = String::from_utf8(data[2].1.clone())
        .unwrap()
        .replace(
            "<metadata>",
            "<metadata><meta property=\"rendition:layout\">pre-paginated</meta>",
        )
        .into_bytes();
    fs::write(&path, epub(data)).unwrap();
    assert_eq!(
        parse::parse(&path, "auto", &AtomicBool::new(false)).err(),
        Some("fixed_layout_epub")
    );
    fs::write(&path, epub(vec![("oversize", vec![0; 33 * 1024 * 1024])])).unwrap();
    assert_eq!(
        parse::parse(&path, "auto", &AtomicBool::new(false)).err(),
        Some("archive_too_large")
    );
}
#[test]
fn import_progress_restart_duplicate_search_and_remove_preserve_original() {
    let fixture = Fixture::new();
    let text = format!(
        "第一章 起点\n{}\n第二章 归途\n回来",
        (0..220)
            .map(|i| format!("第{i}段 正文你好😀\n"))
            .collect::<String>()
    );
    fs::write(fixture.0.join("book.txt"), text).unwrap();
    let data = fixture.0.join("data");
    let mut engine = Engine::open(data.clone()).unwrap();
    let imported = fixture.import(&mut engine, "book.txt");
    let id = imported["book"]["id"].as_str().unwrap();
    let version = imported["book"]["content_version"].as_str().unwrap();
    assert_eq!(fixture.import(&mut engine, "book.txt")["duplicate"], true);
    let page = engine
        .call(
            "chapter.read",
            json!({"book_id":id,"chapter_id":"c0","start":96}),
        )
        .unwrap();
    assert_eq!(page["blocks"].as_array().unwrap().len(), 96);
    assert_eq!(page["total"], 220);
    let found = engine
        .call(
            "chapter.find",
            json!({"book_id":id,"chapter_id":"c0","query":"第150段"}),
        )
        .unwrap();
    assert_eq!(found["match"]["index"], 150);
    let position = json!({"book_id":id,"content_version":version,"chapter_id":"c0","block_id":"b150","offset":8});
    engine
        .call("progress.save", json!({"position":position,"revision":2}))
        .unwrap();
    assert_eq!(
        engine
            .call("progress.save", json!({"position":position,"revision":1}))
            .unwrap_err(),
        "stale_progress"
    );
    let mut invalid = position.clone();
    invalid["offset"] = json!(999);
    assert_eq!(
        engine
            .call("progress.save", json!({"position":invalid,"revision":3}))
            .unwrap_err(),
        "invalid_position"
    );
    engine
        .call("library.favorite", json!({"book_id":id,"in_shelf":false}))
        .unwrap();
    assert_eq!(engine.call("library.list", json!({})).unwrap()["total"], 0);
    let restored = fixture.import(&mut engine, "book.txt");
    assert_eq!(restored["restored"], true);
    assert_eq!(
        engine.call("book.open", json!({"book_id":id})).unwrap()["position"],
        position
    );
    drop(engine);
    fs::remove_file(fixture.0.join("book.txt")).unwrap();
    let mut engine = Engine::open(data.clone()).unwrap();
    let reopened = engine.call("book.open", json!({"book_id":id})).unwrap();
    assert_eq!(reopened["position"], position);
    fs::write(fixture.0.join("book.txt"), "user original").unwrap();
    engine
        .call("library.remove", json!({"book_id":id}))
        .unwrap();
    assert!(fixture.0.join("book.txt").is_file());
    assert!(!data.join("books").join(id).exists());
}
#[test]
fn failed_save_does_not_commit_memory_and_corrupt_index_is_preserved() {
    let fixture = Fixture::new();
    let data = fixture.0.join("data");
    let mut engine = Engine::open(data.clone()).unwrap();
    fs::create_dir(data.join("library.pending")).unwrap();
    assert_eq!(
        engine
            .call(
                "preferences.set",
                json!({"font_size":24,"line_height":2.1,"width":800})
            )
            .unwrap_err(),
        "save_failed"
    );
    assert_eq!(
        engine.call("preferences.get", json!({})).unwrap()["font_size"],
        20
    );
    fs::write(data.join("library.json"), b"broken").unwrap();
    drop(engine);
    assert!(Engine::open(data.clone()).is_err());
    assert_eq!(fs::read(data.join("library.json")).unwrap(), b"broken");
}
#[test]
fn cancellation_does_not_publish_a_book_or_read_arbitrary_identifiers() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("book.txt"), "正文".repeat(10000)).unwrap();
    let mut engine = Engine::open(fixture.0.join("data")).unwrap();
    let job = engine
        .call("library.import", json!({"path":fixture.0.join("book.txt")}))
        .unwrap()["job"]
        .clone();
    engine.call("lifecycle.suspend", json!({})).unwrap();
    for _ in 0..1000 {
        let status = engine.call("import.status", json!({"job":job})).unwrap();
        if status["status"] != "pending" {
            assert_eq!(status["status"], "cancelled");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(engine.call("library.list", json!({})).unwrap()["total"], 0);
    assert!(
        engine
            .call("book.open", json!({"book_id":"../../outside"}))
            .is_err()
    );
}
