use crate::FeedError;
use playbyte_types::System;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

const NES_DB_URL: &str = "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Nintendo%20Entertainment%20System.dat";
const SNES_DB_URL: &str = "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Super%20Nintendo%20Entertainment%20System.dat";
const GBC_DB_URL: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Game%20Boy%20Color.dat";
const GBA_DB_URL: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Game%20Boy%20Advance.dat";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomDatabase {
    sha1_to_title: HashMap<String, String>,
    titles: Vec<String>,
    #[serde(skip)]
    normalized_titles: Vec<String>,
    #[serde(skip)]
    normalized_base_titles: Vec<String>,
}

impl RomDatabase {
    pub fn load_or_fetch(system: System, cache_root: &Path) -> Result<Self, FeedError> {
        fs::create_dir_all(cache_root)?;
        let path = cache_root.join(format!("{}.json", system_id(system)));
        if path.exists() {
            let mut db = Self::load_from_path(&path)?;
            db.prepare();
            return Ok(db);
        }
        let dat_url = system_dat_url(system);
        let db = Self::download_and_parse(dat_url)?;
        db.save_to_path(&path)?;
        Ok(db)
    }

    pub fn title_for_sha1(&self, sha1: &str) -> Option<&str> {
        self.sha1_to_title.get(sha1).map(String::as_str)
    }

    pub fn titles(&self) -> &[String] {
        &self.titles
    }

    pub fn best_match(&self, candidate: &str) -> Option<String> {
        if self.titles.is_empty() {
            return None;
        }
        let normalized = normalize_title(candidate);
        if normalized.is_empty() {
            return None;
        }
        for (idx, title) in self.normalized_titles.iter().enumerate() {
            if title == &normalized {
                return Some(self.titles[idx].clone());
            }
        }

        let base = normalize_base_title(candidate);
        if base.is_empty() {
            return None;
        }
        let matches: Vec<usize> = self
            .normalized_base_titles
            .iter()
            .enumerate()
            .filter_map(|(idx, title)| (title == &base).then_some(idx))
            .collect();
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(self.titles[matches[0]].clone());
        }

        if normalized != base {
            if let Some(idx) = matches
                .iter()
                .find(|&&idx| self.normalized_titles[idx].contains(&normalized))
            {
                return Some(self.titles[*idx].clone());
            }
        }

        let preferred = ["(USA", "(World", "(Europe"];
        for pref in preferred {
            if let Some(idx) = matches.iter().find(|&&idx| self.titles[idx].contains(pref)) {
                return Some(self.titles[*idx].clone());
            }
        }

        Some(self.titles[matches[0]].clone())
    }

    fn load_from_path(path: &Path) -> Result<Self, FeedError> {
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    fn save_to_path(&self, path: &Path) -> Result<(), FeedError> {
        let serialized = serde_json::to_string_pretty(self)?;
        fs::write(path, serialized)?;
        Ok(())
    }

    fn download_and_parse(url: &str) -> Result<Self, FeedError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let response = client.get(url).send()?.error_for_status()?;
        let bytes = response.bytes()?;
        let reader = BufReader::new(bytes.as_ref());
        let mut db = parse_dat(reader)?;
        db.prepare();
        Ok(db)
    }

    fn prepare(&mut self) {
        if !self.normalized_titles.is_empty() {
            return;
        }
        self.normalized_titles = self.titles.iter().map(|title| normalize_title(title)).collect();
        self.normalized_base_titles = self
            .titles
            .iter()
            .map(|title| normalize_base_title(title))
            .collect();
    }
}

pub fn system_id(system: System) -> &'static str {
    match system {
        System::Nes => "nes",
        System::Snes => "snes",
        System::Gbc => "gbc",
        System::Gba => "gba",
    }
}

pub fn system_thumbnail_folder(system: System) -> &'static str {
    match system {
        System::Nes => "Nintendo - Nintendo Entertainment System",
        System::Snes => "Nintendo - Super Nintendo Entertainment System",
        System::Gbc => "Nintendo - Game Boy Color",
        System::Gba => "Nintendo - Game Boy Advance",
    }
}

pub fn system_dat_url(system: System) -> &'static str {
    match system {
        System::Nes => NES_DB_URL,
        System::Snes => SNES_DB_URL,
        System::Gbc => GBC_DB_URL,
        System::Gba => GBA_DB_URL,
    }
}

pub fn sanitize_thumbnail_title(title: &str) -> String {
    let mut output = String::with_capacity(title.len());
    for ch in title.chars() {
        let needs_replace = matches!(ch, '&' | '*' | '/' | ':' | '`' | '<' | '>' | '?' | '\\' | '|');
        if needs_replace {
            output.push('_');
        } else {
            output.push(ch);
        }
    }
    output
}

pub fn build_thumbnail_url(system: System, title: &str) -> String {
    let folder = system_thumbnail_folder(system);
    let sanitized = sanitize_thumbnail_title(title);
    let filename = format!("{sanitized}.png");
    let folder_segment = percent_encode_path_segment(folder);
    let file_segment = percent_encode_path_segment(&filename);
    format!(
        "https://thumbnails.libretro.com/{folder_segment}/Named_Boxarts/{file_segment}"
    )
}

pub fn cover_path(cache_root: &Path, system: System, title: &str) -> PathBuf {
    let sanitized = sanitize_thumbnail_title(title);
    cache_root.join(system_id(system)).join(format!("{sanitized}.png"))
}

fn parse_dat<R: BufRead>(reader: R) -> Result<RomDatabase, FeedError> {
    let mut sha1_to_title = HashMap::new();
    let mut titles = Vec::new();
    let mut title_set = HashSet::new();
    let mut current_title: Option<String> = None;
    let mut in_game = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.starts_with("game (") {
            in_game = true;
            current_title = None;
            continue;
        }
        if in_game && trimmed == ")" {
            in_game = false;
            current_title = None;
            continue;
        }
        if !in_game {
            continue;
        }

        if trimmed.starts_with("name ") {
            if let Some(name) = extract_quoted_value(trimmed) {
                current_title = Some(name);
            }
            continue;
        }
        if trimmed.starts_with("description ") && current_title.is_none() {
            if let Some(description) = extract_quoted_value(trimmed) {
                current_title = Some(description);
            }
            continue;
        }

        if trimmed.contains("rom (") {
            let Some(sha1) = extract_sha1(trimmed) else {
                continue;
            };
            let Some(title) = current_title.clone() else {
                continue;
            };
            sha1_to_title.insert(sha1, title.clone());
            if title_set.insert(title.clone()) {
                titles.push(title);
            }
        }
    }

    Ok(RomDatabase {
        sha1_to_title,
        titles,
        normalized_titles: Vec::new(),
        normalized_base_titles: Vec::new(),
    })
}

fn extract_quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_sha1(line: &str) -> Option<String> {
    let idx = line.find("sha1")?;
    let rest = &line[idx + 4..];
    let token = rest.split_whitespace().next()?;
    let cleaned: String = token.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    if cleaned.len() == 40 {
        Some(cleaned.to_ascii_lowercase())
    } else {
        None
    }
}

fn normalize_title(title: &str) -> String {
    let mut output = String::with_capacity(title.len());
    let mut last_space = false;
    for ch in title.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            ' '
        };
        if normalized == ' ' {
            if !last_space {
                output.push(' ');
                last_space = true;
            }
        } else {
            output.push(normalized);
            last_space = false;
        }
    }
    output.trim().to_string()
}

fn normalize_base_title(title: &str) -> String {
    normalize_title(&strip_bracketed_segments(title))
}

fn strip_bracketed_segments(title: &str) -> String {
    let mut output = String::with_capacity(title.len());
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;
    for ch in title.chars() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            _ => {
                if paren_depth == 0 && bracket_depth == 0 {
                    output.push(ch);
                }
            }
        }
    }
    output
}

fn percent_encode_path_segment(segment: &str) -> String {
    let mut output = String::with_capacity(segment.len());
    for b in segment.as_bytes() {
        let ch = *b as char;
        let is_unreserved = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~');
        if is_unreserved {
            output.push(ch);
        } else {
            output.push('%');
            output.push_str(&format!("{:02X}", *b));
        }
    }
    output
}
