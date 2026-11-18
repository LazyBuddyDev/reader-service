use crate::{model::*, online::Search};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Serialize, Deserialize)]
pub struct ChapterBody {
    pub book_id: String,
    pub chapter_id: String,
    pub chapter_version: String,
    pub blocks: Vec<Block>,
}
#[derive(Default)]
pub struct Body {
    pub cache: VecDeque<ChapterBody>,
    helper: Arc<Mutex<Option<Helper>>>,
}
impl Body {
    pub fn chapter(&self, book: &str, chapter: &str) -> Result<&ChapterBody> {
        self.cache
            .iter()
            .find(|c| c.book_id == book && c.chapter_id == chapter)
            .ok_or("chapter_not_loaded")
    }
    pub fn remember(&mut self, value: Value) -> Result<Value> {
        let chapter: ChapterBody =
            serde_json::from_value(value).map_err(|_| "invalid_source_response")?;
        let meta = json!({"book_id":chapter.book_id,"chapter_id":chapter.chapter_id,"chapter_version":chapter.chapter_version,"total":chapter.blocks.len()});
        self.cache
            .retain(|c| c.book_id != chapter.book_id || c.chapter_id != chapter.chapter_id);
        self.cache.push_back(chapter);
        while self.cache.len() > 3 {
            self.cache.pop_front();
        }
        Ok(meta)
    }
    pub fn source(&self, id: u64, method: &str, args: &Value) -> Result<Search> {
        let prefix = match args["source"].as_str() {
            Some("qimao") => "qimao",
            Some("shuqi_app") => "shuqi",
            _ => return Err("unsupported_source"),
        };
        let request = if method == "online.search" {
            let (query, cursor) = crate::online::parameters(args)?;
            json!({"method":format!("{prefix}.search"),"query":query,"cursor":cursor})
        } else {
            let book = args["book_id"]
                .as_str()
                .filter(|s| crate::online::valid_id(s))
                .ok_or("invalid_source_book")?;
            json!({"method":format!("{prefix}.detail"),"book_id":book})
        };
        let slot = self.helper.clone();
        Ok(Search::worker(id, move |cancel| {
            helper_request(&slot, &request, cancel)
        }))
    }
    pub fn start(
        &self,
        id: u64,
        book: String,
        kind: String,
        source: String,
        chapter: String,
    ) -> Search {
        let slot = self.helper.clone();
        Search::worker(id, move |cancel| {
            let reply = if kind == "midu" {
                serde_json::json!({"content":crate::midu::chapter(&source,&chapter,cancel)?})
            } else {
                helper_request(
                    &slot,
                    &json!({"method":match kind.as_str(){"qimao"=>"qimao.chapter","shuqi_app"=>"shuqi.chapter",_=>"chapter"},"book_id":source,"chapter_id":chapter}),
                    cancel,
                )?
            };
            if cancel.load(Ordering::Acquire) {
                return Err("cancelled");
            }
            let content = reply["content"].as_str().ok_or("invalid_source_response")?;
            let blocks = crate::parse::online_html(content)?;
            let version = hash(&serde_json::to_vec(&blocks).map_err(|_| "source_decode_failed")?);
            Ok(
                json!({"book_id":book,"chapter_id":chapter,"chapter_version":version,"blocks":blocks}),
            )
        })
    }
}
fn helper_request(
    slot: &Mutex<Option<Helper>>,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut helper = slot.lock().map_err(|_| "source_runtime_failed")?;
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    if helper.is_none() {
        *helper = Some(Helper::start()?);
    }
    let reply = helper.as_mut().unwrap().request(args, cancel);
    if reply.is_err() {
        *helper = None;
    }
    let mut reply = reply?;
    if reply["ok"] != true {
        return Err(helper_error(reply["error"].as_str().unwrap_or("")));
    }
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled");
    }
    Ok(reply["result"].take())
}

fn helper_error(code: &str) -> &'static str {
    match code {
        "source_body_unavailable" => "source_body_unavailable",
        "source_request_rejected" => "source_request_rejected",
        "source_empty_chapter" => "source_empty_chapter",
        "source_chapter_locked" => "source_chapter_locked",
        "unsupported_source_book" => "unsupported_source_book",
        "source_request_too_large" => "invalid_search_query",
        "incomplete_source_directory" => "incomplete_source_directory",
        "invalid_search_cursor" => "invalid_search_cursor",
        "invalid_search_query" => "invalid_search_query",
        "invalid_source_book" => "invalid_source_book",
        "invalid_source_response" => "invalid_source_response",
        "source_access_denied" => "source_access_denied",
        "source_verification_required" => "source_verification_required",
        "source_empty_response" => "source_empty_response",
        "source_rate_limited" => "source_rate_limited",
        "source_timeout" => "source_timeout",
        "source_network_failed" => "source_network_failed",
        "source_response_too_large" => "source_response_too_large",
        "credential_save_failed" => "credential_save_failed",
        "credential_read_failed" => "credential_read_failed",
        "invalid_saved_identity" => "invalid_saved_identity",
        "invalid_credential_scope" => "invalid_credential_scope",
        _ => "source_decode_failed",
    }
}
struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    replies: mpsc::Receiver<Result<Value>>,
    job: crate::job::Job,
}
impl Helper {
    fn start() -> Result<Self> {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe()
            .map_err(|_| "source_helper_missing")?
            .with_file_name("fanqie-helper.exe");
        if !exe.is_file() {
            return Err("source_helper_missing");
        }
        let job = crate::job::Job::new().map_err(|_| "source_runtime_failed")?;
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "source_runtime_failed")?;
        if job.attach(&child).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("source_runtime_failed");
        }
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut bytes = vec![];
                let size = (&mut reader)
                    .take(4 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes);
                let result = match size {
                    Ok(0) => return,
                    Ok(n) if n <= 4 * 1024 * 1024 => {
                        serde_json::from_slice(&bytes).map_err(|_| "invalid_source_response")
                    }
                    _ => Err("invalid_source_response"),
                };
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            input,
            replies,
            job,
        })
    }
    fn request(&mut self, args: &Value, cancel: &AtomicBool) -> Result<Value> {
        let input = self.input.as_mut().ok_or("source_runtime_failed")?;
        serde_json::to_writer(&mut *input, args).map_err(|_| "source_runtime_failed")?;
        input
            .write_all(b"\n")
            .and_then(|_| input.flush())
            .map_err(|_| "source_runtime_failed")?;
        let deadline = Instant::now() + Duration::from_secs(65);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("cancelled");
            }
            if Instant::now() > deadline {
                return Err("source_timeout");
            }
            match self.replies.recv_timeout(Duration::from_millis(25)) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("source_runtime_failed"),
            }
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.input.take();
        self.job.stop();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_is_bounded_and_replaces_a_revised_chapter() {
        let mut body = Body::default();
        for i in 0..4 {
            body.remember(json!({"book_id":"b","chapter_id":i.to_string(),"chapter_version":"v1","blocks":[]})).unwrap();
        }
        assert_eq!(body.cache.len(), 3);
        assert!(body.chapter("b", "0").is_err());
        body.remember(json!({"book_id":"b","chapter_id":"2","chapter_version":"v2","blocks":[]}))
            .unwrap();
        assert_eq!(body.cache.len(), 3);
        assert_eq!(body.chapter("b", "2").unwrap().chapter_version, "v2");
    }
    #[test]
    fn native_permission_failures_are_not_reported_as_decode_errors() {
        assert_eq!(
            helper_error("source_chapter_locked"),
            "source_chapter_locked"
        );
        assert_eq!(
            helper_error("unsupported_source_book"),
            "unsupported_source_book"
        );
        assert_eq!(
            helper_error("upstream private details"),
            "source_decode_failed"
        );
    }
    #[test]
    fn cancellation_terminates_the_owned_helper_job() {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::*;
        let job = crate::job::Job::new().unwrap();
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::ReadLine() | Out-Null; Start-Sleep -Seconds 60",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        job.attach(&child).unwrap();
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child.id()) };
        assert!(!handle.is_null());
        let (_tx, rx) = mpsc::channel();
        let input = child.stdin.take();
        let mut helper = Helper {
            child,
            input,
            replies: rx,
            job,
        };
        assert_eq!(
            helper.request(&json!({}), &AtomicBool::new(true)),
            Err("cancelled")
        );
        drop(helper);
        assert_eq!(unsafe { WaitForSingleObject(handle, 3000) }, 0);
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
    }
}
