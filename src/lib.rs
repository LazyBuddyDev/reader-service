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
            Ok(_) => read_json(&path, 8 * 1024 * 1024)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Library {
                schema: 1,
                ..Library::default()
            },
            Err(_) => return Err("read_failed"),
        };
        if library.schema != 1
            || !library.preferences.valid()
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
        let n = index(chapter, "c")?;
        book.chapters
            .get(n)
            .filter(|c| c.id == chapter)
            .ok_or("chapter_not_found")
    }
    fn page(&self, book: &Book, chapter: &str, start: usize) -> Result<Vec<Block>> {
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
    fn import(&mut self, parsed: parse::Parsed) -> Result<Value> {
        if let Some(existing) = self.library.entries.iter().find(|e| e.id == parsed.book.id) {
            return Ok(json!({"book":existing,"duplicate":true}));
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
            fs::write(stage.join("source.bin"), parsed.original).map_err(|_| "save_failed")?;
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
    pub fn call(&mut self, method: &str, args: Value) -> Result<Value> {
        if !args.is_object() {
            return Err("invalid_arguments");
        }
        match method {
            "online.search" => {
                if self.search.as_ref().is_some_and(|s| s.running()) {
                    return Err("search_busy");
                }
                let id = self.next_job + 1;
                let search = online::Search::start(id, &args)?;
                self.next_job = id;
                self.search = Some(search);
                Ok(json!({"job":id,"status":"pending"}))
            }
            "online.status" => {
                let search = self
                    .search
                    .as_mut()
                    .filter(|s| Some(s.id) == args["job"].as_u64())
                    .ok_or("job_not_found")?;
                Ok(search.status())
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
                json!({"service":"reader","api_version":1,"capabilities":["local_import","chapters","progress","chapter_search"]}),
            ),
            "library.list" => {
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let limit = args["limit"].as_u64().unwrap_or(100).min(100) as usize;
                Ok(
                    json!({"entries":self.library.entries.iter().skip(start).take(limit).collect::<Vec<_>>(),"total":self.library.entries.len(),"last_book":self.library.last_book}),
                )
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
                                Ok(json!({"status":"complete","result":self.import(parsed)?}))
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
                if next.last_book.as_deref() == Some(id) {
                    next.last_book = None;
                }
                self.commit(next)?;
                fs::remove_dir_all(directory).map_err(|_| "removal_cleanup_pending")?;
                Ok(json!({}))
            }
            "book.open" => {
                let id = string(&args, "book_id")?;
                let book = self.book(id)?;
                let entry = self.library.entries.iter().find(|e| e.id == id).unwrap();
                Ok(json!({"book":book,"position":entry.position,"revision":entry.revision}))
            }
            "chapter.read" => {
                let book = self.book(string(&args, "book_id")?)?;
                let chapter = string(&args, "chapter_id")?;
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let blocks = self.page(&book, chapter, start)?;
                let total = self.chapter_meta(&book, chapter)?.blocks;
                Ok(json!({"blocks":blocks,"start":start,"total":total}))
            }
            "chapter.find" => {
                let book = self.book(string(&args, "book_id")?)?;
                let chapter = string(&args, "chapter_id")?;
                let query = string(&args, "query")?;
                if query.chars().count() > 200 {
                    return Err("query_too_large");
                }
                let start = args["start"].as_u64().unwrap_or(0) as usize;
                let total = self.chapter_meta(&book, chapter)?.blocks;
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
