//! Explicit local acceptance mode; normal windows have no test reporting behavior.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::Manager;
#[derive(Clone)]
pub struct Smoke {
    path: PathBuf,
    nonce: String,
    done: Arc<AtomicBool>,
    setup: bool,
}
impl Smoke {
    pub fn from_args() -> Option<Self> {
        let mut args = std::env::args_os().skip(1);
        let mode = args.next()?;
        let mode = mode.to_str()?;
        if !["--smoke-test", "--setup-smoke-test"].contains(&mode) {
            return None;
        }
        Some(Self {
            path: PathBuf::from(args.next()?),
            nonce: agentflow_core::api::auth::token_hex(16),
            done: Arc::new(AtomicBool::new(false)),
            setup: mode == "--setup-smoke-test",
        })
    }
    pub fn script(&self) -> String {
        SCRIPT.replace("TEST_NONCE", &self.nonce)
    }
    /// Report the setup URL, then submit the setup form from inside the window once the harness
    /// writes its values (after checking the server's rejections), so the request carries the
    /// headers WebView2 really sends.
    pub fn setup_ready(&self, app: &tauri::AppHandle, url: &str) -> std::io::Result<()> {
        if !self.setup {
            return Ok(());
        }
        std::fs::write(self.path.with_extension("setup.json"), serde_json::json!({"url": url}).to_string())?;
        let form = self.path.with_extension("form.json");
        let app = app.clone();
        std::thread::spawn(move || {
            for _ in 0..600 {
                // A partly written file fails to parse and is read again.
                let fields = std::fs::read(&form).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
                if let Some(serde_json::Value::Object(fields)) = fields {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.eval(SUBMIT.replace("FIELDS", &serde_json::Value::Object(fields).to_string()));
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        Ok(())
    }
    pub fn navigation(&self, app: &tauri::AppHandle, url: &tauri::Url) -> bool {
        if !url.query().is_some_and(|q| q.starts_with("desktopSmoke=")) {
            return true;
        }
        let Some((_, body)) = url.query_pairs().next() else {
            return false;
        };
        let Ok(report) = serde_json::from_str::<serde_json::Value>(&body) else {
            return false;
        };
        if report["nonce"] != self.nonce || self.done.swap(true, Ordering::SeqCst) {
            return false;
        }
        let saved = std::fs::write(&self.path, serde_json::to_vec_pretty(&report).unwrap()).is_ok();
        app.exit(if saved && report["ok"] == true { 0 } else { 1 });
        false
    }
}
const SUBMIT: &str = r#"
(() => {
    const fields = FIELDS;
    const submit = (attempt) => {
        const form = document.querySelector('form[action="/setup"] input[name="mode"][value="' + fields.mode + '"]')?.form;
        if (!form) { if (attempt < 100) setTimeout(() => submit(attempt + 1), 100); return; }
        for (const [name, value] of Object.entries(fields)) {
            const input = form.elements[name];
            if (input.type === 'checkbox') input.checked = value === 'yes'; else input.value = value;
        }
        form.requestSubmit();
    };
    submit(0);
})();
"#;
const SCRIPT: &str = r#"
(() => {
    if (window.__desktopSmoke) return;
    window.__desktopSmoke = true;
    const report = (value) => { location.href = location.origin + '/?desktopSmoke=' + encodeURIComponent(JSON.stringify({nonce:'TEST_NONCE', ...value})); };
    const delay = (ms) => new Promise((resolve) => setTimeout(resolve,ms));
    (async () => {
        try {
            for (let i = 0; i < 100; i++) {
                if (document.querySelector('form[action="/setup"]')) return;
                if (document.querySelector('#app')) break;
                await delay(50);
            }
            let tasks;
            for (let i=0;i<200;i++) {
                const response = await fetch('/api/tasks');
                if (response.ok && !location.hash.includes('pair=') && document.querySelector('#app')?.textContent.includes('AgentFlow') && document.querySelector('.dot.on')) { tasks = await response.json(); break; }
                await delay(100);
            }
            if (!Array.isArray(tasks)) throw new Error('Vue pairing or task page failed');
            const config = await fetch('/api/config');
            if (!config.ok) throw new Error('Paired config request failed');
            const terminal = (await config.json()).system?.terminal?.backend;
            const reload = await fetch('/api/config/reload',{method:'POST'});
            if (!reload.ok) throw new Error('Desktop config reload failed');
            await new Promise((resolve,reject) => {
                const socket = new WebSocket('ws://' + location.host + '/api/ws');
                const timer = setTimeout(() => { socket.close(); reject(new Error('WebSocket timeout')); },5000);
                socket.onopen = () => { clearTimeout(timer);socket.close();resolve(); };
                socket.onerror = () => { clearTimeout(timer);reject(new Error('WebSocket failed')); };
            });
            report({ok:true,vue:true,paired:true,fragmentCleared:true,tasks:tasks.length,websocket:true,config:true,reload:true,terminal,port:location.port});
        } catch (error) { report({ok:false,error:String(error)}); }
    })();
})();
"#;
