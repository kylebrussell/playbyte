mod romdb;

use playbyte_types::{ByteMetadata, System};
use romdb::{build_thumbnail_url, cover_path, RomDatabase};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;
use walkdir::WalkDir;

use std::io::{BufRead, BufReader};

#[derive(Error, Debug)]
pub enum FeedError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("missing metadata for byte {0}")]
    MissingMetadata(String),
}

/// Write `data` to `path` atomically: stage the bytes in a uniquely named temp
/// file in the same directory, then persist it over the destination. The
/// replace is atomic on POSIX (rename) and Windows (MoveFileEx with
/// REPLACE_EXISTING via tempfile's `persist`), so readers never observe a
/// partially written file. Staged files are removed if any step fails.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());

    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .rand_bytes(8)
        .tempfile_in(parent)?;
    use std::io::Write as _;
    temp.as_file_mut().write_all(data)?; // NamedTempFile deletes on drop on error paths

    // `persist` atomically replaces the destination, including on Windows
    // (MoveFileEx with REPLACE_EXISTING). On persist failure the staged file is
    // dropped along with the PersistError, cleaning itself up.
    temp.persist(path).map(|_| ()).map_err(|err| err.error)
}

#[derive(Clone)]
pub struct LocalByteStore {
    root: PathBuf,
    index: Arc<Mutex<Vec<ByteMetadata>>>,
    state_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    thumbnail_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    romdb_cache: Arc<Mutex<HashMap<System, RomDatabase>>>,
}

impl LocalByteStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            index: Arc::new(Mutex::new(Vec::new())),
            state_cache: Arc::new(Mutex::new(HashMap::new())),
            thumbnail_cache: Arc::new(Mutex::new(HashMap::new())),
            romdb_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn bytes_root(&self) -> PathBuf {
        self.root.join("bytes")
    }

    fn rom_titles_path(&self) -> PathBuf {
        self.root.join("rom_titles.json")
    }

    fn rom_official_overrides_path(&self) -> PathBuf {
        self.root.join("rom_official_overrides.json")
    }

    fn romdb_root(&self) -> PathBuf {
        self.root.join("romdb")
    }

    fn covers_root(&self) -> PathBuf {
        self.root.join("covers")
    }

    pub fn load_index(&self) -> Result<Vec<ByteMetadata>, FeedError> {
        let bytes_root = self.bytes_root();
        if !bytes_root.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(bytes_root)? {
            // A single unreadable directory entry should not fail the whole feed.
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let byte_json = entry.path().join("byte.json");
            if !byte_json.exists() {
                continue;
            }
            let data = match fs::read_to_string(&byte_json) {
                Ok(data) => data,
                Err(err) => {
                    eprintln!(
                        "playbyte_feed: skipping unreadable {}: {err}",
                        byte_json.display()
                    );
                    continue;
                }
            };
            let metadata: ByteMetadata = match serde_json::from_str(&data) {
                Ok(metadata) => metadata,
                Err(err) => {
                    eprintln!(
                        "playbyte_feed: skipping malformed {}: {err}",
                        byte_json.display()
                    );
                    continue;
                }
            };
            entries.push(metadata);
        }

        if let Ok(mut guard) = self.index.lock() {
            *guard = entries.clone();
        }

        Ok(entries)
    }

    pub fn list(&self) -> Vec<ByteMetadata> {
        self.index
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn get(&self, byte_id: &str) -> Result<ByteMetadata, FeedError> {
        if let Ok(guard) = self.index.lock() {
            if let Some(found) = guard.iter().find(|entry| entry.byte_id == byte_id) {
                return Ok(found.clone());
            }
        }

        let metadata = Self::load_metadata(&self.bytes_root(), byte_id)?;
        if let Ok(mut guard) = self.index.lock() {
            guard.push(metadata.clone());
        }
        Ok(metadata)
    }

    pub fn load_state(&self, byte_id: &str) -> Result<Vec<u8>, FeedError> {
        if let Ok(guard) = self.state_cache.lock() {
            if let Some(cached) = guard.get(byte_id) {
                return Ok((**cached).clone());
            }
        }

        let metadata = self.get(byte_id)?;
        let path = self
            .bytes_root()
            .join(&metadata.byte_id)
            .join(&metadata.state_path);
        let compressed = fs::read(path)?;
        let state = zstd::stream::decode_all(&compressed[..])?;
        let state_arc = Arc::new(state.clone());
        if let Ok(mut guard) = self.state_cache.lock() {
            guard.insert(byte_id.to_string(), state_arc);
        }
        Ok(state)
    }

    pub fn load_thumbnail(&self, byte_id: &str) -> Result<Vec<u8>, FeedError> {
        if let Ok(guard) = self.thumbnail_cache.lock() {
            if let Some(cached) = guard.get(byte_id) {
                return Ok((**cached).clone());
            }
        }

        let metadata = self.get(byte_id)?;
        let path = self
            .bytes_root()
            .join(&metadata.byte_id)
            .join(&metadata.thumbnail_path);
        let data = fs::read(path)?;
        let data_arc = Arc::new(data.clone());
        if let Ok(mut guard) = self.thumbnail_cache.lock() {
            guard.insert(byte_id.to_string(), data_arc);
        }
        Ok(data)
    }

    pub fn save_byte(
        &self,
        metadata: &ByteMetadata,
        state: &[u8],
        thumbnail: &[u8],
    ) -> Result<(), FeedError> {
        let byte_dir = self.bytes_root().join(&metadata.byte_id);
        fs::create_dir_all(&byte_dir)?;
        let metadata_path = byte_dir.join("byte.json");
        let state_path = byte_dir.join(&metadata.state_path);
        let thumbnail_path = byte_dir.join(&metadata.thumbnail_path);

        // Payloads first, metadata LAST: a crash mid-save can only leave files
        // without a valid-looking byte.json pointing at them, and load_index
        // skips directories whose byte.json is absent.
        let compressed = zstd::stream::encode_all(state, 3)?;
        write_atomic(&state_path, &compressed)?;
        write_atomic(&thumbnail_path, thumbnail)?;

        let serialized = serde_json::to_string_pretty(metadata)?;
        write_atomic(&metadata_path, serialized.as_bytes())?;

        if let Ok(mut guard) = self.index.lock() {
            guard.push(metadata.clone());
        }

        Ok(())
    }

    pub fn update_metadata(&self, metadata: &ByteMetadata) -> Result<(), FeedError> {
        let byte_dir = self.bytes_root().join(&metadata.byte_id);
        fs::create_dir_all(&byte_dir)?;
        let metadata_path = byte_dir.join("byte.json");
        let serialized = serde_json::to_string_pretty(metadata)?;
        write_atomic(&metadata_path, serialized.as_bytes())?;

        if let Ok(mut guard) = self.index.lock() {
            if let Some(entry) = guard
                .iter_mut()
                .find(|entry| entry.byte_id == metadata.byte_id)
            {
                *entry = metadata.clone();
            } else {
                guard.push(metadata.clone());
            }
        }

        Ok(())
    }

    pub fn load_rom_titles(&self) -> Result<HashMap<String, String>, FeedError> {
        let path = self.rom_titles_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn set_rom_title(&self, sha1: &str, title: &str) -> Result<(), FeedError> {
        let mut titles = self.load_rom_titles()?;
        let trimmed = title.trim();
        if trimmed.is_empty() {
            titles.remove(sha1);
        } else {
            titles.insert(sha1.to_string(), trimmed.to_string());
        }
        fs::create_dir_all(&self.root)?;
        let serialized = serde_json::to_string_pretty(&titles)?;
        write_atomic(&self.rom_titles_path(), serialized.as_bytes())?;
        Ok(())
    }

    pub fn load_rom_official_overrides(&self) -> Result<HashMap<String, String>, FeedError> {
        let path = self.rom_official_overrides_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn set_rom_official_override(
        &self,
        sha1: &str,
        title: Option<&str>,
    ) -> Result<(), FeedError> {
        let mut overrides = self.load_rom_official_overrides()?;
        let trimmed = title.unwrap_or_default().trim();
        if trimmed.is_empty() {
            overrides.remove(sha1);
        } else {
            overrides.insert(sha1.to_string(), trimmed.to_string());
        }
        fs::create_dir_all(&self.root)?;
        let serialized = serde_json::to_string_pretty(&overrides)?;
        write_atomic(&self.rom_official_overrides_path(), serialized.as_bytes())?;
        Ok(())
    }

    pub fn load_romdb(&self, system: System) -> Result<RomDatabase, FeedError> {
        if let Ok(guard) = self.romdb_cache.lock() {
            if let Some(db) = guard.get(&system) {
                return Ok(db.clone());
            }
        }
        let db = RomDatabase::load_or_fetch(system, &self.romdb_root())?;
        if let Ok(mut guard) = self.romdb_cache.lock() {
            guard.insert(system, db.clone());
        }
        Ok(db)
    }

    pub fn list_romdb_titles(&self, system: System) -> Result<Vec<String>, FeedError> {
        let db = self.load_romdb(system)?;
        Ok(db.titles().to_vec())
    }

    pub fn cover_art_path(&self, system: System, title: &str) -> PathBuf {
        cover_path(&self.covers_root(), system, title)
    }

    pub fn load_cover_art(&self, system: System, title: &str) -> Result<Vec<u8>, FeedError> {
        let path = self.cover_art_path(system, title);
        Ok(fs::read(path)?)
    }

    pub fn ensure_cover_art(&self, system: System, title: &str) -> Result<PathBuf, FeedError> {
        let path = self.cover_art_path(system, title);
        if path.exists() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let url = build_thumbnail_url(system, title);
        let response = reqwest::blocking::get(url)?.error_for_status()?;
        let bytes = response.bytes()?;
        fs::write(&path, bytes)?;
        Ok(path)
    }

    pub fn prefetch(&self, byte_ids: &[String]) {
        let store = self.clone();
        let ids = byte_ids.to_vec();
        std::thread::spawn(move || {
            for id in ids {
                let _ = store.load_state(&id);
                let _ = store.load_thumbnail(&id);
            }
        });
    }

    fn load_metadata(root: &Path, byte_id: &str) -> Result<ByteMetadata, FeedError> {
        let path = root.join(byte_id).join("byte.json");
        if !path.exists() {
            return Err(FeedError::MissingMetadata(byte_id.to_string()));
        }
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }
}

#[derive(Debug, Default)]
pub struct RomLibrary {
    roots: Vec<PathBuf>,
    index: HashMap<String, PathBuf>,
}

impl RomLibrary {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn add_root(&mut self, path: impl AsRef<Path>) {
        self.roots.push(path.as_ref().to_path_buf());
    }

    pub fn scan(&mut self) -> Result<usize, FeedError> {
        let mut count = 0;
        for root in &self.roots {
            for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if !is_rom_file(path) {
                    continue;
                }
                let hash = hash_file(path)?;
                self.index.insert(hash, path.to_path_buf());
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn find_by_hash(&self, sha1: &str) -> Option<PathBuf> {
        self.index.get(sha1).cloned()
    }

    pub fn entries(&self) -> Vec<(String, PathBuf)> {
        self.index
            .iter()
            .map(|(sha1, path)| (sha1.clone(), path.clone()))
            .collect()
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

#[derive(Debug, Deserialize)]
struct FeedResponse {
    items: Vec<ByteMetadata>,
}

pub struct RemoteByteStore {
    base_url: String,
    client: reqwest::Client,
}

impl RemoteByteStore {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn fetch_feed(&self) -> Result<Vec<ByteMetadata>, FeedError> {
        let url = format!("{}/feed", self.base_url.trim_end_matches('/'));
        let response = self.client.get(url).send().await?;
        let payload: FeedResponse = response.json().await?;
        Ok(payload.items)
    }

    pub async fn fetch_metadata(&self, byte_id: &str) -> Result<ByteMetadata, FeedError> {
        let url = format!("{}/bytes/{}", self.base_url.trim_end_matches('/'), byte_id);
        let response = self.client.get(url).send().await?;
        Ok(response.json().await?)
    }

    pub async fn fetch_state(&self, byte_id: &str) -> Result<Vec<u8>, FeedError> {
        let url = format!(
            "{}/bytes/{}/state",
            self.base_url.trim_end_matches('/'),
            byte_id
        );
        let response = self.client.get(url).send().await?;
        Ok(response.bytes().await?.to_vec())
    }

    pub async fn fetch_thumbnail(&self, byte_id: &str) -> Result<Vec<u8>, FeedError> {
        let url = format!(
            "{}/bytes/{}/thumbnail",
            self.base_url.trim_end_matches('/'),
            byte_id
        );
        let response = self.client.get(url).send().await?;
        Ok(response.bytes().await?.to_vec())
    }
}

fn is_rom_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_lowercase()),
        Some(ext)
            if ext == "nes"
                || ext == "sfc"
                || ext == "smc"
                || ext == "gb"
                || ext == "gbc"
                || ext == "gba"
    )
}

/// Stream `path` through SHA-1 without ever holding the whole file in memory.
/// Returns the digest as lowercase hex.
pub fn sha1_hex_of_file(path: &Path) -> Result<String, FeedError> {
    let mut hasher = Sha1::new();
    let mut reader = BufReader::with_capacity(64 * 1024, fs::File::open(path)?);
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        let len = chunk.len();
        reader.consume(len);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_file(path: &Path) -> Result<String, FeedError> {
    sha1_hex_of_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_byte(root: &Path, byte_id: &str, contents: &str) {
        let dir = root.join("bytes").join(byte_id);
        fs::create_dir_all(&dir).expect("create byte dir");
        fs::write(dir.join("byte.json"), contents).expect("write byte.json");
    }

    #[test]
    fn load_index_skips_corrupt_entries() {
        let root = std::env::temp_dir().join(format!("playbyte_feed_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = LocalByteStore::new(&root);

        write_byte(
            &root,
            "good-byte",
            r#"{
                "byte_id": "good-byte",
                "system": "nes",
                "core_id": "mesen",
                "core_semver": "1.0.0",
                "rom_sha1": "abc123",
                "region": null,
                "title": "Good Byte",
                "description": "",
                "tags": [],
                "author": "local",
                "created_at": "2026-01-17T00:00:00Z",
                "thumbnail_path": "thumbnail.png",
                "state_path": "state.zst"
            }"#,
        );
        write_byte(&root, "bad-json", "{ not valid json");
        // Missing required fields.
        write_byte(&root, "missing-fields", r#"{"byte_id": "missing-fields"}"#);

        let entries = store.load_index().expect("load_index should succeed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].byte_id, "good-byte");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sha1_hex_of_file_matches_known_vectors() {
        let dir = std::env::temp_dir().join(format!("playbyte_sha1_stream_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");

        // Empty file: well-known SHA-1 vector.
        let empty = dir.join("empty.bin");
        fs::write(&empty, b"").expect("create empty file");
        assert_eq!(
            sha1_hex_of_file(&empty).expect("hash empty file"),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );

        // "abc": standard NIST SHA-1 test vector.
        let abc = dir.join("abc.bin");
        fs::write(&abc, b"abc").expect("write abc");
        assert_eq!(
            sha1_hex_of_file(&abc).expect("hash abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );

        // Multi-chunk input (larger than the 64 KiB reader capacity) must hash
        // identically to a whole-file read would.
        let big = dir.join("big.bin");
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&big, &payload).expect("write payload");
        let mut expected = Sha1::new();
        expected.update(&payload);
        let expected_hex = format!("{:x}", expected.finalize());
        assert_eq!(sha1_hex_of_file(&big).expect("hash payload"), expected_hex);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_atomic_creates_overwrites_and_leaves_no_temp_files() {
        let dir =
            std::env::temp_dir().join(format!("playbyte_write_atomic_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let target = dir.join("out.json");

        write_atomic(&target, b"first").expect("initial write");
        assert_eq!(fs::read(&target).unwrap(), b"first");

        // Overwriting an existing destination must also be atomic-safe.
        write_atomic(&target, b"second-payload").expect("overwrite");
        assert_eq!(fs::read(&target).unwrap(), b"second-payload");

        // No staging files left behind in the directory.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers.len(), 1, "only the destination should remain");
        assert_eq!(leftovers[0], "out.json");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_byte_round_trips_through_loaders() {
        let root =
            std::env::temp_dir().join(format!("playbyte_save_roundtrip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = LocalByteStore::new(&root);

        let metadata = ByteMetadata {
            byte_id: "round-trip".to_string(),
            system: System::Nes,
            core_id: "mesen".to_string(),
            core_semver: "1.0.0".to_string(),
            rom_sha1: "abc123".to_string(),
            region: None,
            title: "Round Trip".to_string(),
            description: String::new(),
            tags: Vec::new(),
            author: "local".to_string(),
            created_at: "2026-01-17T00:00:00Z".to_string(),
            thumbnail_path: "thumbnail.png".to_string(),
            state_path: "state.zst".to_string(),
        };

        store
            .save_byte(&metadata, b"emulator-state-bytes", b"png-thumbnail")
            .expect("save_byte should succeed");

        // The freshly saved Byte is immediately loadable through every reader.
        assert_eq!(store.get("round-trip").unwrap().title, "Round Trip");
        assert_eq!(
            store.load_state("round-trip").unwrap(),
            b"emulator-state-bytes"
        );
        assert_eq!(
            store.load_thumbnail("round-trip").unwrap(),
            b"png-thumbnail"
        );

        // Metadata is parseable by load_index (happy path of the crash-safety
        // contract: payloads + valid metadata all present).
        let indexed = store.load_index().unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0].byte_id, "round-trip");

        fs::remove_dir_all(&root).ok();
    }
}
