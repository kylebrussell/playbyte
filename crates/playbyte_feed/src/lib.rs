use playbyte_types::ByteMetadata;
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

#[derive(Clone)]
pub struct LocalByteStore {
    root: PathBuf,
    index: Arc<Mutex<Vec<ByteMetadata>>>,
    state_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    thumbnail_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
}

impl LocalByteStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            index: Arc::new(Mutex::new(Vec::new())),
            state_cache: Arc::new(Mutex::new(HashMap::new())),
            thumbnail_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn bytes_root(&self) -> PathBuf {
        self.root.join("bytes")
    }

    pub fn load_index(&self) -> Result<Vec<ByteMetadata>, FeedError> {
        let bytes_root = self.bytes_root();
        if !bytes_root.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(bytes_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let byte_dir = entry.path();
            let byte_json = byte_dir.join("byte.json");
            if !byte_json.exists() {
                continue;
            }
            let data = fs::read_to_string(&byte_json)?;
            let metadata: ByteMetadata = serde_json::from_str(&data)?;
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

        let serialized = serde_json::to_string_pretty(metadata)?;
        fs::write(metadata_path, serialized)?;

        let compressed = zstd::stream::encode_all(state, 3)?;
        fs::write(state_path, compressed)?;
        fs::write(thumbnail_path, thumbnail)?;

        if let Ok(mut guard) = self.index.lock() {
            guard.push(metadata.clone());
        }

        Ok(())
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
        Some(ext) if ext == "nes" || ext == "sfc" || ext == "smc"
    )
}

fn hash_file(path: &Path) -> Result<String, FeedError> {
    let data = fs::read(path)?;
    let mut hasher = Sha1::new();
    hasher.update(data);
    Ok(format!("{:x}", hasher.finalize()))
}
