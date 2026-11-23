//! Login ID in a small config file; password in the macOS Keychain.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SERVICE: &str = "Docket";
/// Keychain service and settings folder used before the tool was named Docket.
const OLD_SERVICE: &str = "PaperPlaza Assistant";

#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    /// Last site used (host name, see papercept::SITES).
    #[serde(default)]
    pub site: Option<String>,
    /// Login on css.paperplaza.net (kept from before other sites were supported).
    pub login_id: Option<String>,
    /// Login per site: each site has its own accounts.
    #[serde(default)]
    pub logins: std::collections::BTreeMap<String, String>,
    /// Papers shown collapsed, per "site journal".
    #[serde(default)]
    pub folded: std::collections::BTreeMap<String, Vec<String>>,
    pub journal: Option<String>,
    pub role_label: Option<String>,
    /// Viewer mode was ticked at the last login.
    #[serde(default)]
    pub viewer: bool,
    /// Window frames (x, y, width, height in points) per kind of window.
    #[serde(default)]
    pub windows: std::collections::BTreeMap<String, [f64; 4]>,
    /// When the app has a Dock icon: "always", "never", or "" while one of its
    /// windows is open.
    #[serde(default)]
    pub dock: String,
}

impl Config {
    pub fn login_for(&self, site: &str) -> Option<String> {
        self.logins.get(site).cloned().or_else(|| if site == crate::papercept::SITES[0].0 { self.login_id.clone() } else { None })
    }

    pub fn set_login(&mut self, site: &str, id: &str) {
        self.site = Some(site.to_string());
        self.logins.insert(site.to_string(), id.to_string());
        if site == crate::papercept::SITES[0].0 {
            self.login_id = Some(id.to_string());
        }
    }
}

/// Keychain account name of a login on a site.
pub fn account(site: &str, login_id: &str) -> String {
    if site == crate::papercept::SITES[0].0 { login_id.to_string() } else { format!("{login_id} @ {site}") }
}

/// Debug builds can use another folder with DOCKET_CONFIG_DIR. Tests never
/// use the real one.
pub fn config_dir() -> PathBuf {
    if cfg!(debug_assertions) || cfg!(test) {
        if let Some(d) = std::env::var_os("DOCKET_CONFIG_DIR") {
            return PathBuf::from(d);
        }
    }
    if cfg!(test) {
        return std::env::temp_dir().join(format!("docket-test-{}", std::process::id()));
    }
    dir_named("Docket", "docket")
}

/// Downloaded files (manuscripts, review attachments); emptied at each start.
pub fn files_dir() -> PathBuf {
    if cfg!(test) || std::env::var_os("DOCKET_CONFIG_DIR").is_some() {
        return config_dir().join("files");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") { home.join("Library/Caches/Docket/files") } else { home.join(".cache/docket/files") }
}

fn dir_named(mac: &str, other: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") { home.join("Library/Application Support").join(mac) } else { home.join(".config").join(other) }
}

/// Move the settings and schedules saved under the old name, once.
pub fn migrate_old_settings() {
    let (old, new) = (dir_named(OLD_SERVICE, "paperplaza-assistant"), config_dir());
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

pub fn load_config() -> Config {
    std::fs::read(config_dir().join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_config(c: &Config) -> Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(config_dir().join("config.json"), serde_json::to_vec_pretty(c)?)?;
    Ok(())
}

/// The files of the settings folder that move to another computer: logins,
/// PINs and choices, the message templates and signature, the schedules.
/// Passwords stay in the Keychain.
const SETTINGS_FILES: [&str; 3] = ["config.json", "templates.json", "schedules.json"];
const SETTINGS_FORMAT: &str = "Docket settings";

/// All settings in one JSON document.
pub fn export_settings() -> Result<serde_json::Value> {
    let mut files = serde_json::Map::new();
    for name in SETTINGS_FILES {
        if let Ok(b) = std::fs::read(config_dir().join(name)) {
            files.insert(name.into(), serde_json::from_slice(&b).with_context(|| format!("{name} is not valid JSON"))?);
        }
    }
    Ok(serde_json::json!({"format": SETTINGS_FORMAT, "version": 1, "exported": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(), "files": files}))
}

/// Replace the settings with an exported document. The current files are
/// first copied to a "backup …" folder in the settings folder, which is returned.
pub fn import_settings(doc: &serde_json::Value) -> Result<PathBuf> {
    if doc.get("format").and_then(|f| f.as_str()) != Some(SETTINGS_FORMAT) {
        anyhow::bail!("this file is not a Docket settings export");
    }
    let files = doc.get("files").and_then(|f| f.as_object()).context("the export has no settings in it")?;
    if let Some(other) = files.keys().find(|k| !SETTINGS_FILES.contains(&k.as_str())) {
        anyhow::bail!("unexpected file '{other}' in the export");
    }
    if files.get("config.json").is_some_and(|c| serde_json::from_value::<Config>(c.clone()).is_err()) {
        anyhow::bail!("the settings in this export cannot be read");
    }
    let dir = config_dir();
    let backup = dir.join(format!("backup {}", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
    std::fs::create_dir_all(&backup)?;
    for name in SETTINGS_FILES {
        let path = dir.join(name);
        if path.exists() {
            std::fs::copy(&path, backup.join(name))?;
        }
    }
    for (name, value) in files {
        std::fs::write(dir.join(name), serde_json::to_vec_pretty(value)?)?;
    }
    Ok(backup)
}

/// The saved password; one saved under the old name is moved to the new one.
#[cfg(target_os = "macos")]
pub fn get_password(login_id: &str) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};
    if let Some(p) = get_generic_password(SERVICE, login_id).ok().and_then(|b| String::from_utf8(b).ok()) {
        return Some(p);
    }
    let p = get_generic_password(OLD_SERVICE, login_id).ok().and_then(|b| String::from_utf8(b).ok())?;
    if set_generic_password(SERVICE, login_id, p.as_bytes()).is_ok() {
        let _ = delete_generic_password(OLD_SERVICE, login_id);
    }
    Some(p)
}

#[cfg(target_os = "macos")]
pub fn set_password(login_id: &str, password: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    security_framework::passwords::set_generic_password(SERVICE, login_id, password.as_bytes())?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn delete_password(login_id: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let old = security_framework::passwords::delete_generic_password(OLD_SERVICE, login_id);
    let new = security_framework::passwords::delete_generic_password(SERVICE, login_id);
    if new.is_err() {
        old?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn get_password(_login_id: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn set_password(_login_id: &str, _password: &str) -> Result<()> {
    anyhow::bail!("saving the password is only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn delete_password(_login_id: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_move_to_another_computer() {
        let dir = std::env::temp_dir().join(format!("docket-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("DOCKET_CONFIG_DIR", &dir) };
        let cfg = Config { journal: Some("tac".into()), logins: [("css.paperplaza.net".to_string(), "12345".to_string())].into(), ..Default::default() };
        save_config(&cfg).unwrap();
        std::fs::write(dir.join("templates.json"), r#"{"signature":"C. B.","list":[]}"#).unwrap();
        let doc = export_settings().unwrap();
        assert_eq!(doc["files"].as_object().unwrap().keys().collect::<Vec<_>>(), ["config.json", "templates.json"]);
        assert!(!doc.to_string().to_lowercase().contains("password"));

        // The other computer has its own settings: they go to a backup folder.
        save_config(&Config { journal: Some("auto".into()), ..Default::default() }).unwrap();
        std::fs::remove_file(dir.join("templates.json")).unwrap();
        let backup = import_settings(&doc).unwrap();
        assert_eq!(load_config().journal.as_deref(), Some("tac"));
        assert_eq!(load_config().login_for("css.paperplaza.net").as_deref(), Some("12345"));
        assert!(std::fs::read_to_string(dir.join("templates.json")).unwrap().contains("C. B."));
        assert!(std::fs::read_to_string(backup.join("config.json")).unwrap().contains("auto"));

        assert!(import_settings(&serde_json::json!({"journal": "tac"})).is_err(), "not an export");
        let mut bad = doc.clone();
        bad["files"]["../evil.json"] = serde_json::json!({});
        assert!(import_settings(&bad).is_err(), "only the settings files");
        let _ = std::fs::remove_dir_all(dir);
    }
}
