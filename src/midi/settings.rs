//! Music settings (`<data_dir>/midi-settings.json`, replacing miditui's own
//! config location) and SoundFont auto-detection.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const SETTINGS_FILE: &str = "midi-settings.json";

/// The SoundFont that is preferred when a folder holds several.
const PREFERRED_SOUNDFONT: &str = "TimGM6mb.sf2";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// The SoundFont the user last chose.
    #[serde(default)]
    pub soundfont: Option<PathBuf>,
}

impl Settings {
    pub fn load(data_dir: &Path) -> Self {
        fs::read_to_string(data_dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let _ = fs::create_dir_all(data_dir);
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(data_dir.join(SETTINGS_FILE), json)
    }
}

/// Folders searched for a SoundFont, in order.
pub fn soundfont_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        data_dir.join("soundfonts"),
        PathBuf::from("/usr/share/sounds/sf2"),
        PathBuf::from("/usr/share/soundfonts"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/soundfonts"));
    }
    dirs
}

pub fn is_soundfont(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("sf2"))
}

/// The SoundFont to use in `dir`: `TimGM6mb.sf2` if present, else the first
/// `.sf2` by name.
fn pick_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_soundfont(p) && p.is_file())
        .collect();
    found.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    found
        .iter()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(PREFERRED_SOUNDFONT)))
        .or_else(|| found.first())
        .cloned()
}

/// A previously saved choice (if it still exists), else the first folder in
/// `dirs` that holds a SoundFont.
pub fn detect_soundfont_in(saved: Option<&Path>, dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(saved) = saved.filter(|p| p.is_file()) {
        return Some(saved.to_path_buf());
    }
    dirs.iter().find_map(|d| pick_in(d))
}

/// SoundFont auto-detection for `data_dir`.
pub fn detect_soundfont(data_dir: &Path) -> Option<PathBuf> {
    let settings = Settings::load(data_dir);
    detect_soundfont_in(settings.soundfont.as_deref(), &soundfont_dirs(data_dir))
}

/// Where the SoundFont browser starts.
pub fn soundfont_browse_dir(data_dir: &Path, current: Option<&Path>) -> PathBuf {
    if let Some(dir) = current.and_then(|p| p.parent()).filter(|d| d.is_dir()) {
        return dir.to_path_buf();
    }
    soundfont_dirs(data_dir)
        .into_iter()
        .find(|d| d.is_dir())
        .unwrap_or_else(|| data_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mm-midi-sf-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prefers_saved_then_timgm_then_first() {
        let root = temp("detect");
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("zzz.sf2"), "x").unwrap();
        fs::write(b.join("TimGM6mb.sf2"), "x").unwrap();
        fs::write(b.join("aaa.SF2"), "x").unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(detect_soundfont_in(None, &dirs), Some(b.join("TimGM6mb.sf2")));
        fs::remove_file(b.join("TimGM6mb.sf2")).unwrap();
        assert_eq!(detect_soundfont_in(None, &dirs), Some(b.join("aaa.SF2")));
        fs::write(a.join("mine.sf2"), "x").unwrap();
        assert_eq!(detect_soundfont_in(None, &dirs), Some(a.join("mine.sf2")));
        let saved = b.join("zzz.sf2");
        assert_eq!(detect_soundfont_in(Some(&saved), &dirs), Some(saved.clone()));
        assert_eq!(detect_soundfont_in(Some(&root.join("gone.sf2")), &dirs), Some(a.join("mine.sf2")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_round_trip() {
        let dir = temp("settings");
        assert!(Settings::load(&dir).soundfont.is_none());
        Settings { soundfont: Some(PathBuf::from("/x/y.sf2")) }.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir).soundfont, Some(PathBuf::from("/x/y.sf2")));
        let _ = fs::remove_dir_all(&dir);
    }
}
