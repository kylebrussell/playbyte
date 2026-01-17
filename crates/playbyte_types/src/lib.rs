use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum System {
    Nes,
    Snes,
    Gbc,
    Gba,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByteMetadata {
    pub byte_id: String,
    pub system: System,
    pub core_id: String,
    pub core_semver: String,
    pub rom_sha1: String,
    pub region: Option<String>,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub author: String,
    pub created_at: String,
    pub thumbnail_path: String,
    pub state_path: String,
}
