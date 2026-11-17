mod body;
mod job;
pub mod model;
mod online;
pub mod parse;
use model::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub struct Engine {
    root: PathBuf,
    library: Library,
    job: Option<Job>,
    next_job: u64,
    search: Option<online::Search>,
    body: body::Body,
    body_job: Option<u64>,
}
struct Job {
    id: u64,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<parse::Parsed>>,
}
fn string<'a>(args: &'a Value, field: &str) -> Result<&'a str> {
    args[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 32768)
        .ok_or("invalid_arguments")
}
fn index(id: &str, prefix: &str) -> Result<usize> {
    id.strip_prefix(prefix)
        .and_then(|s| s.parse().ok())
        .ok_or("invalid_position")
}
fn book_id(id: &str) -> bool {
    id.len() == 29 && id.starts_with("book-") && id[5..].bytes().all(|b| b.is_ascii_hexdigit())
}
impl Engine {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("books")).map_err(|_| "data_directory_failed")?;
        let path = root.join("library.json");
        let library: Library = match fs::metadata(&path) {
            Ok(_) => {
                let value: Value = read_json(&path, 8 * 1024 * 1024)?;
                let missing_history = value.get("history").is_none();
                let mut library: Library =
                    serde_json::from_value(value).map_err(|_| "invalid_saved_data")?;
                if missing_history {
                    let mut entries: Vec<_> = library
                        .entries
                        .iter()
                        .filter(|e| e.position.is_some())
                        .collect();
                    entries.sort_by_key(|e| std::cmp::Reverse(e.updated_ms));
                    library.history = entries
                        .into_iter()
                        .take(library.preferences.history_limit)
                        .map(|e| Visit {
                            book_id: e.id.clone(),
                            visited_ms: e.updated_ms,
                        })
                        .collect();
                }
                library
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Library {
                schema: 1,
                ..Library::default()
            },
            Err(_) => return Err("read_failed"),
        };
        if library.schema != 1
            || !library.preferences.valid()
            || library.history.len() > library.preferences.history_limit
            || library
                .history
                .iter()
                .any(|v| !library.entries.iter().any(|e| e.id == v.book_id))
            || library.entries.len() > 5000
            || library.entries.iter().any(|e| !book_id(&e.id))
        {
            return Err("invalid_saved_data");
        }
        Ok(Self {
            root,
            library,
            job: None,
            next_job: 0,
            search: None,
            body: Default::default(),
            body_job: None,
        })
    }
    fn commit(&mut self, next: Library) -> Result<()> {
        if serde_json::to_vec(&next).map_err(|_| "save_failed")?.len() > 8 * 1024 * 1024 {
            return Err("library_full");
        }
        atomic_json(&self.root.join("library.json"), &next)?;
        self.library = next;
        Ok(())
    }
    fn directory(&self, id: &str) -> Result<PathBuf> {
        if !book_id(id) || !self.library.entries.iter().any(|b| b.id == id) {
            return Err("book_not_found");
        }
        Ok(self.root.join("books").join(id))
    }
    fn book(&self, id: &str) -> Result<Book> {
        read_json(&self.directory(id)?.join("book.json"), 4 * 1024 * 1024)
    }
    fn chapter_meta<'a>(&self, book: &'a Book, chapter: &str) -> Result<&'a Chapter> {
        if book.source.kind == "fanqie" {
            return book
                .chapters
                .iter()
                .find(|c| c.id == chapter)
                .ok_or("chapter_not_found");
        }
        let n = index(chapter, "c")?;
        book.chapters
            .get(n)
            .filter(|c| c.id == chapter)
            .ok_or("chapter_not_found")
    }
    fn page(&self, book: &Book, chapter: &str, start: usize) -> Result<Vec<Block>> {
        if book.source.kind == "fanqie" {
            let body = self.body.chapter(&book.id, chapter)?;
            if start >= body.blocks.len() || start % PAGE_BLOCKS != 0 {
                return Err("invalid_position");
            }
            return Ok(body
                .blocks
                .iter()
                .skip(start)
                .take(PAGE_BLOCKS)
                .cloned()
                .collect());
        }
        let meta = self.chapter_meta(book, chapter)?;
        if start >= meta.blocks || start % PAGE_BLOCKS != 0 {
            return Err("invalid_position");
        }
        let n = index(chapter, "c")?;
        read_json(
            &self
                .directory(&book.id)?
                .join(format!("c{n}/p{}.json", start / PAGE_BLOCKS)),
            4 * 1024 * 1024,
        )
    }
    fn favorite(&mut self, id: &str, in_shelf: bool) -> Result<Value> {
        let mut next = self.library.clone();
        let entry = next
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or("book_not_found")?;
        if entry.in_shelf == in_shelf {
            return Ok(json!({"book":entry}));
        }
        entry.in_shelf = in_shelf;
        let entry = entry.clone();
        self.commit(next)?;
        Ok(json!({"book":entry}))
    }
    fn import(&mut self, parsed: parse::Parsed, in_shelf: bool) -> Result<Value> {
        if let Some(existing) = self.library.entries.iter().find(|e| e.id == parsed.book.id) {
            let restored = in_shelf && !existing.in_shelf;
            let entry = if restored {
                self.favorite(&parsed.book.id, true)?["book"].clone()
            } else {
                json!(existing)
            };
            return Ok(json!({"book":entry,"duplicate":true,"restored":restored}));
        }
        if self.library.entries.len() >= 5000 {
            return Err("library_full");
        }
        let book = &parsed.book;
        if serde_json::to_vec(book).map_err(|_| "save_failed")?.len() > 4 * 1024 * 1024 {
            return Err("metadata_too_large");
        }
        let directory = self.root.join("books").join(&book.id);
        let stage = self.root.join("books").join(format!(".import-{}", book.id));
        // Only this service's deterministic staging directory is reclaimed.
        if stage.exists() {
            fs::remove_dir_all(&stage).map_err(|_| "cleanup_failed")?;
        }
        fs::create_dir(&stage).map_err(|_| "save_failed")?;
        let result = (|| {
            atomic_json(&stage.join("book.json"), book)?;
            if book.source.kind == "local" {
                fs::write(stage.join("source.bin"), parsed.original).map_err(|_| "save_failed")?;
            }
            for (i, blocks) in parsed.chapters.iter().enumerate() {
                let chapter = stage.join(format!("c{i}"));
                fs::create_dir(&chapter).map_err(|_| "save_failed")?;
                for (page, blocks) in blocks.chunks(PAGE_BLOCKS).enumerate() {
                    atomic_json(&chapter.join(format!("p{page}.json")), &blocks)?;
                }
            }
            fs::create_dir(stage.join("resources")).map_err(|_| "save_failed")?;
            for (id, bytes) in parsed.resources {
                fs::write(stage.join("resources").join(id), bytes).map_err(|_| "save_failed")?;
            }
            // An orphan from an interrupted index commit has no visible owner.
            if directory.exists() {
                fs::remove_dir_all(&directory).map_err(|_| "cleanup_failed")?;
            }
            fs::rename(&stage, &directory).map_err(|_| "save_failed")?;
            let entry = Entry {
                id: book.id.clone(),
                title: book.title.clone(),
                author: book.author.clone(),
                source: book.source.clone(),
                content_version: book.content_version.clone(),
                chapters: book.chapters.len(),
                position: None,
                revision: 0,
                updated_ms: now(),
                in_shelf,
            };
            let mut next = self.library.clone();
            next.entries.push(entry.clone());
            self.commit(next)?;
            Ok(json!({"book":entry,"duplicate":false,"warnings":book.warnings}))
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
            if !self.library.entries.iter().any(|e| e.id == book.id) {
                let _ = fs::remove_dir_all(directory);
            }
        }
        result
    }
    fn add_online(&mut self, detail: Value, in_shelf: bool) -> Result<Value> {
        // This snapshot comes only from the completed, validated source worker.
        let source_id = detail["id"].as_str().ok_or("invalid_source_response")?;
        let id = format!(
            "book-{}",
            &hash(format!("fanqie:{source_id}").as_bytes())[..24]
        );
        let chapters = detail["chapters"]
            .as_array()
            .ok_or("invalid_source_response")?
            .iter()
            .map(|c| {
                Ok(Chapter {
                    id: c["id"]
                        .as_str()
                        .ok_or("invalid_source_response")?
                        .to_owned(),
                    title: c["title"]
                        .as_str()
                        .ok_or("invalid_source_response")?
                        .to_owned(),
                    blocks: 0,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let book = Book {
            id,
            title: detail["title"]
                .as_str()
                .ok_or("invalid_source_response")?
                .to_owned(),
            author: detail["author"].as_str().unwrap_or("").to_owned(),
            description: detail["description"].as_str().unwrap_or("").to_owned(),
            source: Source {
                kind: "fanqie".into(),
                id: source_id.into(),
                format: "online".into(),
                encoding: String::new(),
            },
            content_version: format!(
                "catalog-{}",
                hash(&serde_json::to_vec(&chapters).map_err(|_| "save_failed")?)
            ),
            chapters,
            resources: Default::default(),
            warnings: vec![],
        };
        self.import(
            parse::Parsed {
                book,
                chapters: vec![],
                resources: Default::default(),
                original: vec![],
            },
            in_shelf,
        )
    }
    pub fn call(&mut self, method: &str, args: Value) -> Result<Value> {
        if !args.is_object() {
            return Err("invalid_arguments");
        }
        match method {
            "online.chapter" => {
                if self.search.as_ref().is_some_and(|s| s.running()) {
                    return Err("search_busy");
                }
                let book = self.book(string(&args, "book_id")?)?;
                if book.source.kind != "fanqie" {
                    return Err("unsupported_source");
                }
                let chapter = string(&args, "chapter_id")?;
                self.chapter_meta(&book, chapter)?;
                if let Ok(body) = self.body.chapter(&book.id, chapter) {
                    return Ok(
                        json!({"status":"complete","result":{"book_id":book.id,"chapter_id":chapter,"chapter_version":body.chapter_version,"total":body.blocks.len()}}),
                    );
                }
                self.next_job += 1;
                self.search = Some(self.body.start(
                    self.next_job,
                    book.id,
                    book.source.id,
                    chapter.to_owned(),
                ));
                self.body_job = Some(self.next_job);
                Ok(json!({"job":self.next_job,"status":"pending"}))
            }
            "library.favorite" => {
                let id = string(&args, "book_id")?;
                let in_shelf = args["in_shelf"].as_bool().ok_or("invalid_arguments")?;
                self.favorite(id, in_shelf)
            }
            "library.add" | "online.open" => {
                let in_shelf = method == "library.add";
                if args["source"] != "fanqie" {
                    return Err("unsupported_source");
                }
                let source_id = string(&args, "book_id")?;
                if let Some(existing) = self
                    .library
                    .entries
                    .iter()
                    .find(|e| e.source.kind == "fanqie" && e.source.id == source_id)
                {
                    if in_shelf && !existing.in_shelf {
                        let id = existing.id.clone();
                        return Ok(
                            json!({"book":self.favorite(&id,true)?["book"],"duplicate":true,"restored":true}),
                        );
                    }
                    return Ok(json!({"book":existing,"duplicate":true}));
                }
                let detail = self
                    .search
                    .as_ref()
                    .ok_or("source_detail_required")?
                    .detail_result(source_id)?;
                self.add_online(detail, in_shelf)
            }
            "online.search" | "online.detail" => {
                if self.search.as_ref().is_some_and(|s| s.running()) {
                    return Err("search_busy");
                }
                let id = self.next_job + 1;
                let search = if method == "online.detail" {
                    online::Search::detail(id, &args)?
                } else {
                    online::Search::start(id, &args)?
                };
                self.next_job = id;
                self.search = Some(search);
                self.body_job = None;
                Ok(json!({"job":id,"status":"pending"}))
            }
            "online.status" => {
                let search = self
                    .search
                    .as_mut()
                    .filter(|s| Some(s.id) == args["job"].as_u64())
                    .ok_or("job_not_found")?;
                let mut status = search.status();
                if self.body_job == Some(search.id) && status["status"] == "complete" {
                    let result = status["result"].take();
                    self.directory(
                        result["book_id"]
                            .as_str()
                            .ok_or("invalid_source_response")?,
                    )?;
                    let metadata = self.body.remember(result)?;
                    self.search
                        .as_mut()
                        .unwrap()
                        .replace_result(metadata.clone());
                    status["result"] = metadata;
                    self.body_job = None;
                }
                Ok(status)
            }
            "online.cancel" => {
                let search = self
                    .search
                    .as_ref()
                    .filter(|s| Some(s.id) == args["job"].as_u64())
                    .ok_or("job_not_found")?;
                search.cancel();
                Ok(json!({}))
            }
            "health" => Ok(
                json!({"service":"reader","api_version":1,"capabilities":["local_import","chapters","progress","chapter_search","online_search","online_details","online_shelf","online_body","shelf_membership","browsing_history"]}),
            ),
            "library.list" => {
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let limit = args["limit"].as_u64().unwrap_or(100).min(100) as usize;
                Ok(
                    json!({"entries":self.library.entries.iter().filter(|e|e.in_shelf).skip(start).take(limit).collect::<Vec<_>>(),"total":self.library.entries.iter().filter(|e|e.in_shelf).count(),"last_book":self.library.last_book}),
                )
            }
            "history.list" => {
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let limit = args["limit"].as_u64().unwrap_or(100).min(100) as usize;
                let entries: Vec<_> = self
                    .library
                    .history
                    .iter()
                    .skip(start)
                    .take(limit)
                    .map(|v| {
                        let mut entry = json!(
                            self.library
                                .entries
                                .iter()
                                .find(|e| e.id == v.book_id)
                                .unwrap()
                        );
                        entry["visited_ms"] = json!(v.visited_ms);
                        entry
                    })
                    .collect();
                Ok(json!({"entries":entries,"total":self.library.history.len()}))
            }
            "library.import" => {
                if let Some(job) = &self.job {
                    if job.cancel.load(Ordering::Acquire) {
                        match job.receiver.try_recv() {
                            Ok(_) | Err(mpsc::TryRecvError::Disconnected) => self.job = None,
                            Err(_) => return Err("import_busy"),
                        }
                    } else {
                        return Err("import_busy");
                    }
                }
                let path = PathBuf::from(string(&args, "path")?);
                if !path.is_absolute() {
                    return Err("invalid_import_path");
                }
                let encoding = args["encoding"].as_str().unwrap_or("auto").to_owned();
                self.next_job += 1;
                let cancel = Arc::new(AtomicBool::new(false));
                let worker = cancel.clone();
                let (tx, receiver) = mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let result = parse::parse(&path, &encoding, &worker).and_then(|p| {
                        if worker.load(Ordering::Acquire) {
                            Err("cancelled")
                        } else {
                            Ok(p)
                        }
                    });
                    let _ = tx.send(result);
                });
                self.job = Some(Job {
                    id: self.next_job,
                    cancel,
                    receiver,
                });
                Ok(json!({"job":self.next_job,"status":"pending"}))
            }
            "import.status" => {
                let job = self
                    .job
                    .as_ref()
                    .filter(|j| Some(j.id) == args["job"].as_u64())
                    .ok_or("job_not_found")?;
                match job.receiver.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => Ok(json!({"status":"pending"})),
                    result => {
                        let cancelled = self.job.take().unwrap().cancel.load(Ordering::Acquire);
                        if cancelled {
                            return Ok(json!({"status":"cancelled"}));
                        }
                        match result {
                            Ok(Ok(parsed)) => {
                                Ok(json!({"status":"complete","result":self.import(parsed,true)?}))
                            }
                            Ok(Err(code)) => Ok(json!({"status":"failed","error":code})),
                            Err(_) => Err("import_worker_failed"),
                        }
                    }
                }
            }
            "import.cancel" | "lifecycle.suspend" => {
                if let Some(job) = &self.job {
                    job.cancel.store(true, Ordering::Release);
                }
                if method == "lifecycle.suspend" {
                    if let Some(search) = &self.search {
                        search.cancel();
                    }
                }
                Ok(json!({}))
            }
            "library.remove" => {
                let id = string(&args, "book_id")?;
                let directory = self.directory(id)?;
                let mut next = self.library.clone();
                next.entries.retain(|e| e.id != id);
                next.history.retain(|v| v.book_id != id);
                if next.last_book.as_deref() == Some(id) {
                    next.last_book = None;
                }
                self.commit(next)?;
                self.body.cache.retain(|c| c.book_id != id);
                fs::remove_dir_all(directory).map_err(|_| "removal_cleanup_pending")?;
                Ok(json!({}))
            }
            "book.open" => {
                let id = string(&args, "book_id")?;
                let book = self.book(id)?;
                let mut next = self.library.clone();
                next.history.retain(|v| v.book_id != id);
                next.history.insert(
                    0,
                    Visit {
                        book_id: id.to_owned(),
                        visited_ms: now(),
                    },
                );
                next.history.truncate(next.preferences.history_limit);
                self.commit(next)?;
                let entry = self.library.entries.iter().find(|e| e.id == id).unwrap();
                Ok(
                    json!({"book":book,"position":entry.position,"revision":entry.revision,"in_shelf":entry.in_shelf}),
                )
            }
            "chapter.read" => {
                let book = self.book(string(&args, "book_id")?)?;
                let chapter = string(&args, "chapter_id")?;
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let blocks = self.page(&book, chapter, start)?;
                let total = if book.source.kind == "fanqie" {
                    self.body.chapter(&book.id, chapter)?.blocks.len()
                } else {
                    self.chapter_meta(&book, chapter)?.blocks
                };
                let version = if book.source.kind == "fanqie" {
                    Some(
                        self.body
                            .chapter(&book.id, chapter)?
                            .chapter_version
                            .as_str(),
                    )
                } else {
                    None
                };
                Ok(json!({"blocks":blocks,"start":start,"total":total,"chapter_version":version}))
            }
            "chapter.find" => {
                let book = self.book(string(&args, "book_id")?)?;
                let chapter = string(&args, "chapter_id")?;
                let query = string(&args, "query")?;
                if query.chars().count() > 200 {
                    return Err("query_too_large");
                }
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let total = if book.source.kind == "fanqie" {
                    self.body.chapter(&book.id, chapter)?.blocks.len()
                } else {
                    self.chapter_meta(&book, chapter)?.blocks
                };
                for page_start in (start / PAGE_BLOCKS * PAGE_BLOCKS..total).step_by(PAGE_BLOCKS) {
                    let blocks = self.page(&book, chapter, page_start)?;
                    for (i, block) in blocks
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| page_start + i >= start)
                    {
                        if let Some(offset) = block.text.find(query) {
                            return Ok(
                                json!({"match":{"index":page_start+i,"block_id":block.id,"offset":block.text[..offset].chars().count()}}),
                            );
                        }
                    }
                }
                Ok(json!({"match":null}))
            }
            "resource.read" => {
                use base64::Engine;
                let book = self.book(string(&args, "book_id")?)?;
                let id = string(&args, "resource_id")?;
                if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalid_resource");
                }
                let mime = book.resources.get(id).ok_or("resource_not_found")?;
                let bytes = fs::read(self.directory(&book.id)?.join("resources").join(id))
                    .map_err(|_| "resource_not_found")?;
                if bytes.len() > 1024 * 1024 {
                    return Err("resource_too_large");
                }
                Ok(
                    json!({"url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes))}),
                )
            }
            "progress.save" => {
                let position: Position = serde_json::from_value(args["position"].clone())
                    .map_err(|_| "invalid_position")?;
                let revision = args["revision"].as_u64().ok_or("invalid_revision")?;
                let book = self.book(&position.book_id)?;
                if position.content_version != book.content_version {
                    return Err("content_version_changed");
                }
                if book.source.kind == "fanqie"
                    && position.chapter_version.as_deref()
                        != Some(
                            self.body
                                .chapter(&book.id, &position.chapter_id)?
                                .chapter_version
                                .as_str(),
                        )
                {
                    return Err("content_version_changed");
                }
                let block_index = index(&position.block_id, "b")?;
                let blocks = self.page(
                    &book,
                    &position.chapter_id,
                    block_index / PAGE_BLOCKS * PAGE_BLOCKS,
                )?;
                let block = blocks
                    .get(block_index % PAGE_BLOCKS)
                    .filter(|b| b.id == position.block_id)
                    .ok_or("invalid_position")?;
                if position.offset > block.text.chars().count() {
                    return Err("invalid_position");
                }
                let mut next = self.library.clone();
                let entry = next
                    .entries
                    .iter_mut()
                    .find(|e| e.id == position.book_id)
                    .unwrap();
                if revision <= entry.revision {
                    return Err("stale_progress");
                }
                entry.revision = revision;
                entry.position = Some(position.clone());
                entry.updated_ms = now();
                next.last_book = Some(position.book_id);
                self.commit(next)?;
                Ok(json!({"saved":true,"revision":revision}))
            }
            "preferences.get" => Ok(json!(self.library.preferences)),
            "preferences.set" => {
                let prefs: Preferences =
                    serde_json::from_value(args).map_err(|_| "invalid_preferences")?;
                if !prefs.valid() {
                    return Err("invalid_preferences");
                }
                let mut next = self.library.clone();
                next.history.truncate(prefs.history_limit);
                next.preferences = prefs;
                self.commit(next)?;
                Ok(json!(self.library.preferences))
            }
            _ => Err("unknown_method"),
        }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Release);
        }
    }
}

pub fn handle(engine: &mut Engine, request: Value) -> Value {
    let id = request["id"].clone();
    let result = if request["version"] != 1 || id.as_str().is_none_or(|s| s.len() > 128) {
        Err("invalid_request")
    } else {
        engine.call(
            request["method"].as_str().unwrap_or(""),
            request["args"].clone(),
        )
    };
    match result {
        Ok(data) => json!({"version":1,"id":id,"ok":true,"data":data}),
        Err(code) => json!({"version":1,"id":id,"ok":false,"error":{"code":code}}),
    }
}
