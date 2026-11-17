use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub type Result<T> = std::result::Result<T, &'static str>;
pub const MAX_FILE: u64 = 32 * 1024 * 1024;
pub const MAX_EXPANDED: u64 = 128 * 1024 * 1024;
pub const PAGE_BLOCKS: usize = 96;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub kind: String,
    pub id: String,
    pub format: String,
    pub encoding: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub book_id: String,
    pub content_version: String,
    pub chapter_id: String,
    pub block_id: String,
    pub offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter_version: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub source: Source,
    pub content_version: String,
    pub chapters: Vec<Chapter>,
    pub resources: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub title: String,
    pub blocks: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strong: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub emphasis: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub runs: Vec<Run>,
    pub resource: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub author: String,
    pub source: Source,
    pub content_version: String,
    pub chapters: usize,
    pub position: Option<Position>,
    pub revision: u64,
    pub updated_ms: u64,
    #[serde(default = "default_shelf")]
    pub in_shelf: bool,
}
fn default_shelf() -> bool {
    true
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PageMode {
    #[default]
    Horizontal,
    Vertical,
    Scroll,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub font_size: u32,
    pub line_height: f32,
    pub width: u32,
    #[serde(default)]
    pub page_mode: PageMode,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            font_size: 20,
            line_height: 1.9,
            width: 680,
            page_mode: PageMode::default(),
            history_limit: default_history_limit(),
        }
    }
}
impl Preferences {
    pub fn valid(&self) -> bool {
        (14..=36).contains(&self.font_size)
            && (1.3..=2.5).contains(&self.line_height)
            && (320..=1000).contains(&self.width)
            && (1..=500).contains(&self.history_limit)
    }
}
fn default_history_limit() -> usize {
    100
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Visit {
    pub book_id: String,
    pub visited_ms: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Library {
    pub schema: u32,
    pub entries: Vec<Entry>,
    pub preferences: Preferences,
    pub last_book: Option<String>,
    #[serde(default)]
    pub history: Vec<Visit>,
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path, max: u64) -> Result<T> {
    if fs::metadata(path).map_err(|_| "read_failed")?.len() > max {
        return Err("data_too_large");
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "read_failed")?)
        .map_err(|_| "invalid_saved_data")
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension("pending");
    let mut file = fs::File::create(&temp).map_err(|_| "save_failed")?;
    serde_json::to_writer(&mut file, value).map_err(|_| "save_failed")?;
    file.flush()
        .and_then(|_| file.sync_all())
        .map_err(|_| "save_failed")?;
    drop(file);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                0x1 | 0x8,
            )
        } == 0
        {
            return Err("save_failed");
        }
    }
    #[cfg(not(windows))]
    fs::rename(&temp, path).map_err(|_| "save_failed")?;
    Ok(())
}

#[cfg(test)]
mod preference_tests {
    use super::*;
    #[test]
    fn page_mode_defaults_and_validates_the_protocol_boundary() {
        let old: Preferences =
            serde_json::from_str(r#"{"font_size":20,"line_height":1.9,"width":680}"#).unwrap();
        assert_eq!(old.page_mode, PageMode::Horizontal);
        for mode in ["horizontal", "vertical", "scroll"] {
            let value =
                serde_json::json!({"font_size":20,"line_height":1.9,"width":680,"page_mode":mode});
            let preferences: Preferences = serde_json::from_value(value.clone()).unwrap();
            assert!(preferences.valid());
            assert_eq!(
                serde_json::to_value(preferences).unwrap()["page_mode"],
                value["page_mode"]
            );
        }
        assert!(serde_json::from_value::<Preferences>(serde_json::json!({"font_size":20,"line_height":1.9,"width":680,"page_mode":"unknown"})).is_err());
    }
}
