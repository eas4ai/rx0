//! Per-user preferences in `~/.rx0/settings.json`, never in a workspace.
//!
//! Ports `settings.go`: the typed settings bridge (`agent` ↔
//! `agent.harness`, `models` ↔ `agent.models`), the verbatim settings
//! schema the UI renders, merged/defaults/raw reads, and the preserving
//! writes. All file access serialises on one mutex.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub models: HashMap<String, String>,

    #[serde(
        rename = "editor.fontSize",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_font_size: Option<f64>,
    #[serde(
        rename = "editor.fontFamily",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_font_family: Option<String>,
    #[serde(
        rename = "editor.lineHeight",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_line_height: Option<f64>,
    #[serde(
        rename = "editor.tabSize",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_tab_size: Option<i64>,
    #[serde(
        rename = "editor.wordWrap",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_word_wrap: Option<String>,
    #[serde(
        rename = "editor.lineNumbers",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_line_numbers: Option<String>,
    #[serde(
        rename = "editor.renderWhitespace",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_render_whitespace: Option<String>,
    #[serde(
        rename = "editor.minimap.enabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_minimap_enabled: Option<bool>,
    #[serde(
        rename = "workbench.colorTheme",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workbench_color_theme: Option<String>,
    #[serde(
        rename = "diffEditor.renderSideBySide",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diff_side_by_side: Option<bool>,
    #[serde(
        rename = "markdown.preview.open",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub markdown_preview_open: Option<bool>,
    #[serde(
        rename = "telemetry.enabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub telemetry_enabled: Option<bool>,
}

/// Honour the XDG location when set, else `~/.rx0`. Ports Go
/// `settingsPath` (mirrors `stateFilePath` in update.go).
pub fn settings_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("rx0").join("settings.json"));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".rx0").join("settings.json"))
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaItem {
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub default: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
}

/// The schema the settings UI renders. Verbatim port of Go
/// `settingsSchema`, including whole-number defaults as integers (Go
/// marshals `float64(21)` as `21`). A function rather than a `static`
/// because `serde_json::json!` is not `const`.
pub fn settings_schema() -> Vec<SchemaItem> {
    vec![
    SchemaItem { key: "editor.fontSize", title: "Font Size", description: "Controls the font size in pixels for the code viewer.", category: "Text Editor", kind: "number", default: serde_json::json!(13.5), options: None, min: Some(9.0), max: Some(32.0), step: Some(0.5) },
    SchemaItem { key: "editor.fontFamily", title: "Font Family", description: "Controls the font family used in the code viewer.", category: "Text Editor", kind: "string", default: serde_json::json!("\"JetBrains Mono\", \"Fira Code\", \"Cascadia Code\", \"SF Mono\", Menlo, Consolas, ui-monospace, monospace"), options: None, min: None, max: None, step: None },
    SchemaItem { key: "editor.lineHeight", title: "Line Height", description: "Controls the line height in pixels for the code viewer.", category: "Text Editor", kind: "number", default: serde_json::json!(21), options: None, min: Some(14.0), max: Some(48.0), step: Some(1.0) },
    SchemaItem { key: "editor.tabSize", title: "Tab Size", description: "The number of spaces a tab is equal to.", category: "Text Editor", kind: "select", default: serde_json::json!(4), options: Some(&["2", "4", "8"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.wordWrap", title: "Word Wrap", description: "Controls whether lines should wrap around or scroll horizontally.", category: "Text Editor", kind: "select", default: serde_json::json!("on"), options: Some(&["on", "off"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.lineNumbers", title: "Line Numbers", description: "Controls the display of line numbers in the gutter.", category: "Text Editor", kind: "select", default: serde_json::json!("on"), options: Some(&["on", "off"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.renderWhitespace", title: "Render Whitespace", description: "Controls how whitespace characters are rendered in the viewer.", category: "Text Editor", kind: "select", default: serde_json::json!("selection"), options: Some(&["none", "boundary", "selection", "all"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.minimap.enabled", title: "Minimap Hits", description: "Controls whether search hit indicators are shown in the scroll minimap gutter.", category: "Text Editor", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "workbench.colorTheme", title: "Color Theme", description: "Specifies the color theme used in the workbench.", category: "Workbench", kind: "select", default: serde_json::json!("github-dark"), options: Some(&["github-dark", "dark", "light", "catppuccin-mocha", "catppuccin-latte", "dracula", "gruvbox-dark", "gruvbox-light", "monokai", "nord", "one-dark", "rose-pine", "solarized-dark", "solarized-light"]), min: None, max: None, step: None },
    SchemaItem { key: "diffEditor.renderSideBySide", title: "Diff Side By Side", description: "Controls whether the diff editor shows changes in split (side-by-side) or unified mode.", category: "Workbench", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "markdown.preview.open", title: "Markdown Preview", description: "Controls whether Markdown files open in rendered preview by default.", category: "Workbench", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "editor.cursorStyle", title: "Cursor Style", description: "Controls the cursor style in the code viewer.", category: "Text Editor", kind: "select", default: serde_json::json!("line"), options: Some(&["line", "block", "underline"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.cursorBlinking", title: "Cursor Blinking", description: "Controls the cursor animation style.", category: "Text Editor", kind: "select", default: serde_json::json!("smooth"), options: Some(&["blink", "smooth", "solid"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.renderLineHighlight", title: "Render Line Highlight", description: "Controls how the editor should render the current line highlight.", category: "Text Editor", kind: "select", default: serde_json::json!("line"), options: Some(&["line", "none"]), min: None, max: None, step: None },
    SchemaItem { key: "editor.occurrencesHighlight", title: "Occurrences Highlight", description: "Controls whether the editor should highlight occurrences of the selected word.", category: "Text Editor", kind: "select", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "editor.scrollBeyondLastLine", title: "Scroll Beyond Last Line", description: "Controls whether the editor will scroll beyond the last line of the file.", category: "Text Editor", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "editor.bracketPairColorization", title: "Bracket Pair Colorization", description: "Controls whether bracket pair colorization and matching is enabled.", category: "Text Editor", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "explorer.compactFolders", title: "Compact Folders", description: "Controls whether the file tree renders single-child directory chains compactly.", category: "Files & Explorer", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "explorer.autoReveal", title: "Auto Reveal Active File", description: "Controls whether the file explorer automatically scrolls to and reveals active tabs.", category: "Files & Explorer", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "files.exclude", title: "Files Exclude Patterns", description: "Configure glob patterns for excluding files and folders from search and trees.", category: "Files & Explorer", kind: "string", default: serde_json::json!("**/.git, **/node_modules, **/target, **/.DS_Store"), options: None, min: None, max: None, step: None },
    SchemaItem { key: "search.smartCase", title: "Smart Case Search", description: "Searches case-insensitively when query is lowercase, and case-sensitively when uppercase characters exist.", category: "Search", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "search.maxResults", title: "Max Search Results", description: "Controls the maximum number of results returned in workspace-wide searches.", category: "Search", kind: "number", default: serde_json::json!(1000), options: None, min: Some(50.0), max: Some(10000.0), step: Some(50.0) },
    SchemaItem { key: "diffEditor.ignoreTrimWhitespace", title: "Diff: Ignore Trim Whitespace", description: "Controls whether the diff viewer ignores changes in leading or trailing whitespace.", category: "Git & Diff", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "git.gutterIndicators", title: "Git Gutter Indicators", description: "Controls whether changed line indicators are shown in the editor gutter.", category: "Git & Diff", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "lsp.enabled", title: "Language Server Protocol (LSP)", description: "Master switch for language server integrations (definitions, references, diagnostics).", category: "LSP & Intelligence", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "lsp.hover.enabled", title: "Hover Documentation", description: "Controls whether hovercards with documentation and type signatures appear on hover.", category: "LSP & Intelligence", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    SchemaItem { key: "agent.harness", title: "Coding Harness", description: "Coding agent harness invoked for code edits (e.g. claude, gemini, cursor-agent, agy, opencode, codex, aider, goose).", category: "Agent / AI", kind: "string", default: serde_json::json!(""), options: None, min: None, max: None, step: None },
    SchemaItem { key: "agent.timeoutSeconds", title: "Agent Timeout (Seconds)", description: "Controls the maximum execution time in seconds for agent edits before canceling.", category: "Agent / AI", kind: "number", default: serde_json::json!(120), options: None, min: Some(10.0), max: Some(600.0), step: Some(10.0) },
    SchemaItem { key: "agent.autoAcceptEdits", title: "Auto Accept Agent Edits", description: "Controls whether agent-generated code diffs are accepted without manual confirmation.", category: "Agent / AI", kind: "boolean", default: serde_json::json!(false), options: None, min: None, max: None, step: None },
    SchemaItem { key: "telemetry.enabled", title: "Telemetry", description: "Enable anonymous usage metrics to help improve rx0.", category: "Security & Privacy", kind: "boolean", default: serde_json::json!(true), options: None, min: None, max: None, step: None },
    ]
}

/// Defaults plus the agent/models aliases. Ports Go `defaultSettingsMap`.
pub fn default_settings_map() -> Map<String, Value> {
    let schema = settings_schema();
    let mut res = Map::with_capacity(schema.len() + 2);
    for item in &schema {
        res.insert(item.key.to_string(), item.default.clone());
    }
    res.insert("agent".to_string(), Value::String(String::new()));
    res.insert("models".to_string(), Value::Object(Map::new()));
    res
}

/// Raw file contents; never fails. Ports Go `readSettingsRawMap`.
fn read_raw_map_locked() -> Map<String, Value> {
    let Some(path) = settings_path() else {
        return Map::new();
    };
    let Ok(data) = std::fs::read(&path) else {
        return Map::new();
    };
    serde_json::from_slice::<Value>(&data)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Typed read with the agent/models bridges. Ports Go `readSettings`.
pub fn read_settings() -> Settings {
    let _guard = LOCK.lock().unwrap();
    let raw = read_raw_map_locked();
    if raw.is_empty() {
        return Settings::default();
    }
    let mut s: Settings = serde_json::from_value(Value::Object(raw.clone())).unwrap_or_default();
    if s.agent.is_empty() {
        if let Some(Value::String(h)) = raw.get("agent.harness") {
            if !h.is_empty() {
                s.agent = h.clone();
            }
        }
    }
    if s.models.is_empty() {
        if let Some(Value::Object(am)) = raw.get("agent.models") {
            for (k, v) in am {
                if let Value::String(vs) = v {
                    s.models.insert(k.clone(), vs.clone());
                }
            }
        }
    }
    s
}

/// Defaults overlaid with stored choices, bridges synchronised. Ports Go
/// `readMergedSettingsMap`.
pub fn read_merged_map() -> Map<String, Value> {
    let _guard = LOCK.lock().unwrap();
    let mut res = default_settings_map();
    let raw = read_raw_map_locked();
    for (k, v) in &raw {
        res.insert(k.clone(), v.clone());
    }
    let agent = raw.get("agent").and_then(|v| v.as_str()).unwrap_or("");
    let harness = raw
        .get("agent.harness")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !agent.is_empty() {
        res.insert(
            "agent.harness".to_string(),
            Value::String(agent.to_string()),
        );
    } else if !harness.is_empty() {
        res.insert("agent".to_string(), Value::String(harness.to_string()));
    }
    let models = raw.get("models");
    let agent_models = raw.get("agent.models");
    let non_empty =
        |v: Option<&Value>| v.and_then(|v| v.as_object()).is_some_and(|m| !m.is_empty());
    if non_empty(models) {
        res.insert("agent.models".to_string(), models.unwrap().clone());
    } else if non_empty(agent_models) {
        res.insert("models".to_string(), agent_models.unwrap().clone());
    }
    res
}

/// Pretty file content for the settings editor. Ports Go
/// `readRawSettingsJSON`.
pub fn read_raw_json() -> String {
    let _guard = LOCK.lock().unwrap();
    let Some(path) = settings_path() else {
        return "{}\n".to_string();
    };
    let Ok(data) = std::fs::read(&path) else {
        return "{\n}\n".to_string();
    };
    if data.is_empty() {
        return "{\n}\n".to_string();
    }
    if let Ok(raw) = serde_json::from_slice::<Value>(&data) {
        if let Ok(pretty) = serde_json::to_string_pretty(&raw) {
            return pretty + "\n";
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn write_raw_map_locked(path: &std::path::Path, raw: &Map<String, Value>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut data = serde_json::to_string_pretty(raw).map_err(|e| e.to_string())?;
    data.push('\n');
    std::fs::write(path, data).map_err(|e| e.to_string())
}

/// Merge pairs in, deleting nulls, keeping the bridges in sync. Ports Go
/// `updateSettingsMap`.
pub fn update_settings_map(updates: &Map<String, Value>) -> Result<(), String> {
    let Some(path) = settings_path() else {
        return Err("no home directory to save settings in".to_string());
    };
    let _guard = LOCK.lock().unwrap();
    let mut raw = read_raw_map_locked();
    for (k, v) in updates {
        if v.is_null() {
            raw.remove(k);
        } else {
            raw.insert(k.clone(), v.clone());
        }
        if k == "agent" {
            if v.is_null() || v == "" {
                raw.remove("agent.harness");
            } else {
                raw.insert("agent.harness".to_string(), v.clone());
            }
        } else if k == "agent.harness" {
            if v.is_null() || v == "" {
                raw.remove("agent");
            } else {
                raw.insert("agent".to_string(), v.clone());
            }
        }
        if k == "models" {
            if v.is_null() {
                raw.remove("agent.models");
            } else {
                raw.insert("agent.models".to_string(), v.clone());
            }
        } else if k == "agent.models" {
            if v.is_null() {
                raw.remove("models");
            } else {
                raw.insert("models".to_string(), v.clone());
            }
        }
    }
    write_raw_map_locked(&path, &raw)
}

/// Persist the agent harness choice without disturbing anything else.
/// Ports Go `writeSettings`.
pub fn write_settings(s: &Settings) -> Result<(), String> {
    let Some(path) = settings_path() else {
        return Err("no home directory to save settings in".to_string());
    };
    let _guard = LOCK.lock().unwrap();
    let mut raw = read_raw_map_locked();
    if !s.agent.is_empty() {
        raw.insert("agent".to_string(), Value::String(s.agent.clone()));
        raw.insert("agent.harness".to_string(), Value::String(s.agent.clone()));
    } else {
        raw.remove("agent");
        raw.remove("agent.harness");
    }
    if !s.models.is_empty() {
        let models: Map<String, Value> = s
            .models
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        raw.insert("models".to_string(), Value::Object(models.clone()));
        raw.insert("agent.models".to_string(), Value::Object(models));
    } else if s.agent.is_empty() {
        raw.remove("models");
        raw.remove("agent.models");
    }
    write_raw_map_locked(&path, &raw)
}

/// Validate and save raw editor text. Ports Go `saveRawSettingsJSON`.
pub fn save_raw_json(raw_text: &str) -> Result<(), String> {
    let mut m: Map<String, Value> = serde_json::from_str::<Value>(raw_text)
        .map_err(|e| e.to_string())
        .and_then(|v| {
            v.as_object()
                .cloned()
                .ok_or_else(|| "settings must be a JSON object".to_string())
        })?;
    let agent = m.get("agent").and_then(|v| v.as_str()).unwrap_or("");
    let harness = m
        .get("agent.harness")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !agent.is_empty() {
        m.insert(
            "agent.harness".to_string(),
            Value::String(agent.to_string()),
        );
    } else if !harness.is_empty() {
        m.insert("agent".to_string(), Value::String(harness.to_string()));
    }
    let Some(path) = settings_path() else {
        return Err("no home directory to save settings in".to_string());
    };
    let _guard = LOCK.lock().unwrap();
    write_raw_map_locked(&path, &m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Isolated `~/.rx0`: point both lookup roots at a temp dir while
    /// holding the env lock. Returns the dir and guards (drop order:
    /// env first, then dir removal — declare dir first).
    fn isolate() -> (
        crate::testutil::TempDir,
        std::sync::MutexGuard<'static, ()>,
        crate::testutil::EnvGuard,
    ) {
        let dir = crate::testutil::tempdir("settings");
        let lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let guard = crate::testutil::set_env(&[("XDG_CONFIG_HOME", &path), ("HOME", &path)]);
        (dir, lock, guard)
    }

    /// Ports Go `TestSettingsDefaults`.
    #[test]
    fn merged_defaults_match_go() {
        let (_dir, _lock, _env) = isolate();
        let m = read_merged_map();
        assert_eq!(m["editor.fontSize"], json!(13.5));
        assert_eq!(m["workbench.colorTheme"], json!("github-dark"));
        assert_eq!(m["editor.wordWrap"], json!("on"));
        assert_eq!(m["editor.cursorStyle"], json!("line"));
        assert_eq!(m["explorer.compactFolders"], json!(true));
        assert_eq!(m["search.smartCase"], json!(true));
        assert_eq!(m["lsp.enabled"], json!(true));
        assert_eq!(m["agent.timeoutSeconds"], json!(120));
    }

    /// Ports Go `TestSettingsPreserveNonAgentValues`.
    #[test]
    fn agent_write_preserves_editor_values() {
        let (_dir, _lock, _env) = isolate();
        let mut updates = Map::new();
        updates.insert("editor.fontSize".to_string(), json!(16.0));
        updates.insert("workbench.colorTheme".to_string(), json!("gruvbox-dark"));
        updates.insert("custom.property".to_string(), json!("hello"));
        update_settings_map(&updates).unwrap();

        let mut models = HashMap::new();
        models.insert("claude".to_string(), "sonnet".to_string());
        write_settings(&Settings {
            agent: "claude".to_string(),
            models,
            ..Default::default()
        })
        .unwrap();

        let m = read_merged_map();
        assert_eq!(m["editor.fontSize"], json!(16.0));
        assert_eq!(m["workbench.colorTheme"], json!("gruvbox-dark"));
        assert_eq!(m["custom.property"], json!("hello"));
        assert_eq!(m["agent"], json!("claude"));
        assert_eq!(m["agent.harness"], json!("claude"));
    }

    #[test]
    fn bridges_sync_both_directions() {
        let (_dir, _lock, _env) = isolate();
        let mut updates = Map::new();
        updates.insert("agent.harness".to_string(), json!("gemini"));
        update_settings_map(&updates).unwrap();
        assert_eq!(read_merged_map()["agent"], json!("gemini"));

        // Nulls delete.
        let mut del = Map::new();
        del.insert("agent.harness".to_string(), Value::Null);
        update_settings_map(&del).unwrap();
        let m = read_merged_map();
        assert_eq!(m["agent"], json!(""));
    }

    #[test]
    fn raw_save_replaces_and_validates() {
        let (_dir, _lock, _env) = isolate();
        assert!(save_raw_json("{not json").is_err());
        assert!(save_raw_json("[1,2]").is_err());
        save_raw_json("{\n  \"editor.fontSize\": 15,\n  \"workbench.colorTheme\": \"nord\"\n}\n")
            .unwrap();
        let m = read_merged_map();
        assert_eq!(m["editor.fontSize"], json!(15));
        assert_eq!(m["workbench.colorTheme"], json!("nord"));
    }
}
