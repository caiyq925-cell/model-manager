#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::Manager;

// ---------- 数据模型 ----------

#[derive(Serialize, Deserialize, Clone)]
struct Provider {
    id: u64,
    name: String,
    base_url: String,
    api_key: String,
    /// 保存(勾选)的模型
    models: Vec<String>,
    enabled: bool,
}

// ---------- 持久化 ----------

fn providers_file(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("providers.json")
}

#[tauri::command]
fn load_providers(app: tauri::AppHandle) -> Vec<Provider> {
    std::fs::read(providers_file(&app))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn save_providers(app: tauri::AppHandle, providers: Vec<Provider>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&providers).map_err(|e| e.to_string())?;
    std::fs::write(providers_file(&app), json).map_err(|e| e.to_string())
}

// ---------- API 工具 ----------

/// 拼接 API 地址：末尾带 /v1 则直接用，否则自动补 /v1；以 # 结尾表示按输入原样使用。
fn build_url(base: &str, path: &str) -> String {
    let mut b = base.trim().trim_end_matches('/').to_string();
    if b.ends_with('#') {
        b.pop();
    } else if !b.ends_with("/v1") {
        b.push_str("/v1");
    }
    format!("{b}{path}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn error_message_from_body(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(msg) = v["error"]["message"].as_str() {
            return truncate(msg, 300);
        }
        if let Some(msg) = v["error"].as_str() {
            return truncate(msg, 300);
        }
        if let Some(msg) = v["message"].as_str() {
            return truncate(msg, 300);
        }
    }
    if text.trim().is_empty() {
        "(服务器返回空内容)".to_string()
    } else {
        truncate(text.trim(), 300)
    }
}

fn make_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

// ---------- 命令 ----------

#[tauri::command]
async fn fetch_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let url = build_url(&base_url, "/models");
    let client = make_client()?;
    let resp = client
        .get(&url)
        .bearer_auth(&api_key)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", error_message_from_body(&text)));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| format!("响应不是合法 JSON: {}", truncate(&text, 200)))?;

    let mut ids: Vec<String> = Vec::new();
    if let Some(arr) = v["data"].as_array() {
        for m in arr {
            if let Some(id) = m["id"].as_str() {
                ids.push(id.to_string());
            }
        }
    }
    // 兼容 {"models": [...]} 形式
    if ids.is_empty() {
        if let Some(arr) = v["models"].as_array() {
            for m in arr {
                let id = m
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| m["id"].as_str().map(|s| s.to_string()))
                    .or_else(|| m["name"].as_str().map(|s| s.to_string()));
                if let Some(id) = id {
                    ids.push(id);
                }
            }
        }
    }
    ids.sort();
    if ids.is_empty() {
        return Err("模型列表为空".to_string());
    }
    Ok(ids)
}

#[tauri::command]
async fn test_model(base_url: String, api_key: String, model: String) -> Result<u64, String> {
    let url = build_url(&base_url, "/chat/completions");
    let client = make_client()?;
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "你好，请只回复：ok"}]
    });
    let start = Instant::now();
    let resp = client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", error_message_from_body(&text)));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| format!("响应不是合法 JSON: {}", truncate(&text, 200)))?;
    // 兼容标准响应和部分服务把内容包在 data 字段里的写法
    let choices = v["choices"]
        .as_array()
        .or_else(|| v["data"]["choices"].as_array());
    if choices.map_or(true, |c| c.is_empty()) {
        return Err(format!("响应中没有 choices: {}", truncate(&text, 200)));
    }
    Ok(start.elapsed().as_millis() as u64)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            load_providers,
            save_providers,
            fetch_models,
            test_model
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
