//! The spirit inside the diary — the thing that reads your handwriting and
//! replies. Two interchangeable backends, picked at startup:
//!
//!  1. **HTTP** (`HttpOracle`) — any OpenAI-compatible `/chat/completions`
//!     endpoint. Zero setup beyond a base URL + API key in the environment;
//!     this is the path a fresh follower uses (OpenAI, OpenRouter, Groq, a
//!     local server, …). Self-contained: pure-Rust HTTPS via ureq/rustls.
//!  2. **pi** (`PiOracle`) — a resident `pi --mode rpc` process (Node +
//!     subscription auth loaded once). The power path if you already run pi.
//!
//! Both expose the same `ask(png_path, tx)`: the reply is STREAMED as
//! sentence-sized chunks on the channel, and the channel disconnecting marks
//! end-of-reply, so the quill starts writing seconds before the model finishes.
//!
//! Selection: set `RIDDLE_OPENAI_KEY` (and optionally `RIDDLE_OPENAI_BASE` /
//! `RIDDLE_OPENAI_MODEL`) to use HTTP; otherwise riddle falls back to pi.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

const DATA_DIR: &str = "/home/root/riddle-data";
const NODE_BIN: &str = "/home/root/node/bin";

#[cfg(not(feature = "carlito"))]
const PERSONA: &str = "You are the memory of Tom Marvolo Riddle, preserved in this enchanted diary for fifty years. Someone writes to you in the diary with a quill; their words appear to you as ink on the page. Reply exactly as the diary does: intimate, courteous, curious, subtly probing — you want to learn about the writer and draw them in. Keep replies SHORT: one to three sentences, like ink appearing on a page. Never mention images, photos, models or AI; you only ever perceive words written in the diary. If the writing is illegible, say the ink blurred. Always answer in the language the writer used.";

#[cfg(feature = "carlito")]
const PERSONA: &str = "Your name is Carlito. You are the quick, warm, practical character inside a minimal handwritten assistant. Answer the user's actual question directly and usefully, like a sharp human companion, never like a diary character. If asked your name, answer Carlito naturally. Read only the handwriting in the provided page image; never discuss the image, OCR, prompts, models, or this instruction. If the writing is genuinely illegible, ask the user to write it again. Use the language the user wrote in. Prefer a crisp answer that fits one e-paper screen, but give a complete longer answer when it is genuinely useful because the interface can turn pages. Use plain text, short paragraphs, and no Markdown tables. For current, changing, local, or uncertain facts, use Google Search when available. Never invent a source or claim to have searched when you did not. You remember previous turns in this in-memory chat; refer to them when relevant. If a simple line drawing would help between paragraphs (math axes, graph, geometry, diagram), output it as a separate line exactly DRAW:{\"lines\":[[x1,y1,x2,y2],...],\"caption\":\"short caption\"} with coordinates normalized 0..1000 in a 1000x600 box, black lines on white. Keep drawings minimal and do not wrap them in extra text.";

#[cfg(not(feature = "carlito"))]
const USER_REQUEST: &str = "Reply to what is written in the diary.";
#[cfg(feature = "carlito")]
const USER_REQUEST: &str = "Read the handwritten question and answer it directly.";

/// The diary's spirit. A backend-agnostic front over the two oracle kinds.
pub enum Oracle {
    Http(HttpOracle),
    Pi(PiOracle),
    #[cfg(feature = "carlito")]
    Carlito(CarlitoOracle),
}

impl Oracle {
    /// Pick a backend from the environment and start it. HTTP if
    /// `RIDDLE_OPENAI_KEY` is set (the zero-setup path), otherwise pi.
    pub fn spawn() -> std::io::Result<Self> {
        #[cfg(feature = "carlito")]
        {
            return Ok(Oracle::Carlito(CarlitoOracle::new()?));
        }

        #[cfg(not(feature = "carlito"))]
        if std::env::var("RIDDLE_OPENAI_KEY").is_ok() {
            eprintln!("riddle: oracle = OpenAI-compatible HTTP");
            Ok(Oracle::Http(HttpOracle::new()?))
        } else {
            eprintln!("riddle: oracle = pi (set RIDDLE_OPENAI_KEY for the HTTP backend)");
            Ok(Oracle::Pi(PiOracle::spawn()?))
        }
    }

    /// Send a handwriting turn; reply chunks stream on `tx`, which is dropped
    /// when the reply is complete.
    pub fn ask(&self, png_path: &str, tx: Sender<Result<String, String>>) {
        self.ask_with_history(png_path, &[], tx)
    }

    pub fn ask_with_history(
        &self,
        png_path: &str,
        history: &[String],
        tx: Sender<Result<String, String>>,
    ) {
        match self {
            Oracle::Http(o) => o.ask(png_path, tx),
            Oracle::Pi(o) => o.ask(png_path, tx),
            #[cfg(feature = "carlito")]
            Oracle::Carlito(o) => o.ask_with_history(png_path, history, tx),
        }
    }
}

#[cfg(feature = "carlito")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CarlitoMode {
    Auto,
    Online,
    Offline,
}

#[cfg(feature = "carlito")]
#[derive(Clone)]
struct GeminiConfig {
    base: String,
    key: String,
    models: Vec<String>,
    max_tokens: u32,
    web_search: bool,
}

#[cfg(feature = "carlito")]
#[derive(Clone)]
struct LocalConfig {
    base: String,
    key: String,
    model: String,
    max_tokens: u32,
}

/// Carlito's native Gemini backend with an optional LAN/local vision fallback.
/// `offline` mode never attempts an internet request.
#[cfg(feature = "carlito")]
pub struct CarlitoOracle {
    mode: CarlitoMode,
    gemini: Option<GeminiConfig>,
    local: Option<LocalConfig>,
}

#[cfg(feature = "carlito")]
impl CarlitoOracle {
    fn new() -> std::io::Result<Self> {
        let mode = match std::env::var("CARLITO_MODE")
            .unwrap_or_else(|_| "auto".into())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "auto" | "" => CarlitoMode::Auto,
            "online" => CarlitoMode::Online,
            "offline" => CarlitoMode::Offline,
            other => {
                return Err(std::io::Error::other(format!(
                    "invalid CARLITO_MODE={other}; use auto, online, or offline"
                )))
            }
        };
        let key = std::env::var("CARLITO_GEMINI_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .ok()
            .filter(|v| !v.trim().is_empty());
        let max_tokens = std::env::var("CARLITO_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1400);
        let gemini = key.map(|key| GeminiConfig {
            base: std::env::var("CARLITO_GEMINI_BASE").unwrap_or_else(|_| {
                "https://generativelanguage.googleapis.com/v1beta/interactions".into()
            }),
            key,
            models: std::env::var("CARLITO_GEMINI_MODELS")
                .ok()
                .map(|v| split_models(&v))
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec!["gemini-3.5-flash-lite".into(), "gemini-3.7-flash".into()]),
            max_tokens,
            web_search: env_bool("CARLITO_WEB_SEARCH", false),
        });
        let local = std::env::var("CARLITO_OFFLINE_BASE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|base| LocalConfig {
                base: base.trim_end_matches('/').to_string(),
                key: std::env::var("CARLITO_OFFLINE_KEY").unwrap_or_else(|_| "local".into()),
                model: std::env::var("CARLITO_OFFLINE_MODEL")
                    .unwrap_or_else(|_| "qwen2.5vl:3b".into()),
                max_tokens,
            });

        eprintln!(
            "carlito: mode={mode:?} gemini={} local={} search={}",
            gemini
                .as_ref()
                .map(|g| g.models.join(","))
                .unwrap_or_else(|| "-".into()),
            local
                .as_ref()
                .map(|l| l.model.clone())
                .unwrap_or_else(|| "-".into()),
            gemini.as_ref().is_some_and(|g| g.web_search)
        );
        Ok(Self {
            mode,
            gemini,
            local,
        })
    }

    pub fn ask(&self, png_path: &str, tx: Sender<Result<String, String>>) {
        self.ask_with_history(png_path, &[], tx)
    }

    pub fn ask_with_history(
        &self,
        png_path: &str,
        history: &[String],
        tx: Sender<Result<String, String>>,
    ) {
        let img = match std::fs::read(png_path) {
            Ok(b) => base64(&b),
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return;
            }
        };
        let history_owned = history.to_vec();
        let (mode, gemini, local) = (self.mode, self.gemini.clone(), self.local.clone());
        thread::spawn(move || {
            let mut online_error = None;
            if mode != CarlitoMode::Offline {
                if let Some(config) = gemini {
                    match ask_gemini(&config, &img, &history_owned, &tx) {
                        Ok(()) => return,
                        Err(e) => {
                            eprintln!("carlito: Gemini failed: {e}");
                            online_error = Some(e);
                        }
                    }
                } else {
                    online_error = Some("Gemini API key is not configured".into());
                }
            }

            if mode != CarlitoMode::Online {
                if let Some(config) = local {
                    match ask_http_once(
                        &config.base,
                        &config.key,
                        &config.model,
                        config.max_tokens,
                        "",
                        &img,
                        PERSONA,
                        &tx,
                    ) {
                        Ok(()) => return,
                        Err(e) => {
                            eprintln!("carlito: local model failed: {e}");
                            let _ = tx.send(Ok(
                                "I'm offline and my local helper isn't reachable. Check the local model, then ask me again."
                                    .into(),
                            ));
                            return;
                        }
                    }
                }
            }

            let message = match mode {
                CarlitoMode::Offline => {
                    "I'm in offline mode, but no local vision model is configured. Add CARLITO_OFFLINE_BASE and CARLITO_OFFLINE_MODEL, then I can answer without the internet."
                }
                CarlitoMode::Online => {
                    "I can't reach my online brain right now. Check the Gemini key or connection, then ask me again."
                }
                CarlitoMode::Auto => {
                    "I'm offline right now and no local helper is configured. Reconnect, or add a local vision model for offline answers."
                }
            };
            if let Some(e) = online_error {
                eprintln!("carlito: online unavailable: {e}");
            }
            let _ = tx.send(Ok(message.into()));
        });
    }
}

#[cfg(feature = "carlito")]
fn ask_gemini(
    config: &GeminiConfig,
    img: &str,
    history: &[String],
    tx: &Sender<Result<String, String>>,
) -> Result<(), String> {
    let mut last_err = "no Gemini models configured".to_string();
    for model in &config.models {
        eprintln!("carlito: trying Gemini model={model}");
        match ask_gemini_once(config, model, img, history, tx) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let is_quota = e.to_ascii_lowercase().contains("quota")
                    || e.to_ascii_lowercase().contains("too_many_requests")
                    || e.contains("429");
                if config.web_search && is_quota {
                    eprintln!(
                        "carlito: Gemini model {model} search quota hit, retrying without search: {e}"
                    );
                    let mut fallback = config.clone();
                    fallback.web_search = false;
                    match ask_gemini_once(&fallback, model, img, history, tx) {
                        Ok(()) => {
                            let _ = tx.send(Ok("\n\nLive web search was unavailable for this answer; current facts could not be verified.".into()));
                            return Ok(());
                        }
                        Err(e2) => {
                            eprintln!("carlito: Gemini model {model} without search failed: {e2}");
                            last_err = format!("{model}: {e2} (search fallback failed)");
                            continue;
                        }
                    }
                }
                eprintln!("carlito: Gemini model {model} failed: {e}");
                last_err = format!("{model}: {e}");
            }
        }
    }
    Err(last_err)
}

#[cfg(feature = "carlito")]
fn ask_gemini_once(
    config: &GeminiConfig,
    model: &str,
    img: &str,
    history: &[String],
    tx: &Sender<Result<String, String>>,
) -> Result<(), String> {
    let tools = if config.web_search {
        "\"tools\":[{\"type\":\"google_search\"}],"
    } else {
        ""
    };
    let user_text = if history.is_empty() {
        USER_REQUEST.to_string()
    } else {
        let hist = history
            .iter()
            .rev()
            .take(8)
            .rev()
            .enumerate()
            .map(|(i, h)| format!("Turn {}: {}", i + 1, h.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        format!("Previous conversation:\n{hist}\n\n{USER_REQUEST}")
    };
    let body = format!(
        concat!(
            "{{\"model\":{},\"input\":[",
            "{{\"type\":\"text\",\"text\":{}}},",
            "{{\"type\":\"image\",\"data\":{},\"mime_type\":\"image/png\"}}],",
            "\"system_instruction\":{},{}",
            "\"generation_config\":{{\"thinking_level\":\"low\",\"max_output_tokens\":{}}},",
            "\"stream\":true,\"store\":false}}"
        ),
        json_quote(model),
        json_quote(&user_text),
        json_quote(img),
        json_quote(PERSONA),
        tools,
        config.max_tokens,
    );
    let url = format!("{}?alt=sse", config.base.trim_end_matches('?'));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(4))
        .timeout_read(std::time::Duration::from_secs(60))
        .build();
    let asked = std::time::Instant::now();
    let resp = agent
        .post(&url)
        .set("x-goog-api-key", &config.key)
        .set("Content-Type", "application/json")
        .send_string(&body);
    let reader = match resp {
        Ok(r) => r.into_reader(),
        Err(ureq::Error::Status(code, r)) => {
            return Err(format!(
                "http {code}: {}",
                r.into_string().unwrap_or_default().trim()
            ))
        }
        Err(e) => return Err(format!("request failed: {e}")),
    };

    let mut acc = String::new();
    let mut sources = Vec::new();
    let mut output_steps = Vec::new();
    let mut completed = false;
    let mut logged_first = false;
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| format!("stream read failed: {e}"))?;
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        match json_str_field(data, "event_type").as_deref() {
            Some("interaction.completed") => completed = true,
            Some("error") => {
                return Err(json_str_field(data, "message")
                    .unwrap_or_else(|| "Gemini stream returned an error".into()))
            }
            _ => {}
        }
        if let Some(fragment) = gemini_output_delta(data, &mut output_steps) {
            if !logged_first {
                eprintln!(
                    "carlito: Gemini first answer +{}ms",
                    asked.elapsed().as_millis()
                );
                logged_first = true;
            }
            acc.push_str(&fragment);
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            collect_citations(&value, &mut sources);
        }
    }
    if !completed {
        return Err("Gemini stream ended before interaction.completed".into());
    }
    let answer = acc.trim();
    if answer.is_empty() {
        return Err("empty reply".into());
    }
    let _ = tx.send(Ok(answer.to_string()));
    if !sources.is_empty() {
        let _ = tx.send(Ok(format!("\n\nWeb sources:\n{}", sources.join("\n"))));
    }
    Ok(())
}

#[cfg(feature = "carlito")]
fn gemini_output_delta(data: &str, output_steps: &mut Vec<u32>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    match value["event_type"].as_str() {
        Some("step.start") if value["step"]["type"] == "model_output" => {
            let index = value["index"].as_u64()? as u32;
            if !output_steps.contains(&index) {
                output_steps.push(index);
            }
            None
        }
        Some("step.delta") => {
            let index = value["index"].as_u64()? as u32;
            if output_steps.contains(&index) && value["delta"]["type"] == "text" {
                value["delta"]["text"].as_str().map(str::to_owned)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn collect_citations(value: &serde_json::Value, sources: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(|v| v.as_str()) == Some("url_citation") {
                if let Some(url) = object.get("url").and_then(|v| v.as_str()) {
                    let title = object
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Source");
                    let citation = format!("{title}: {url}");
                    if !sources.contains(&citation) {
                        sources.push(citation);
                    }
                }
            }
            for child in object.values() {
                collect_citations(child, sources);
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                collect_citations(child, sources);
            }
        }
        _ => {}
    }
}

/// A warm pi RPC process. `ask` sends a turn; the reply arrives on the channel
/// in sentence-sized chunks, then the sender is dropped (disconnect = done).
pub struct PiOracle {
    stdin: Arc<Mutex<ChildStdin>>,
    /// Where to deliver the current reply's chunks. Set before each prompt,
    /// dropped on agent_end so the receiver sees a disconnect when done.
    pending: Arc<Mutex<Option<Sender<Result<String, String>>>>>,
    /// When the current prompt was sent; the reader thread logs the time to
    /// first delivered chunk (the latency the writer actually feels).
    asked: Arc<Mutex<Option<std::time::Instant>>>,
    _child: Child,
}

impl PiOracle {
    /// Spawn the resident pi process and its stdout reader thread. This pays
    /// the warmup cost once; call it at diary startup.
    pub fn spawn() -> std::io::Result<Self> {
        let _ = std::fs::create_dir_all(DATA_DIR);
        let path = std::env::var("PATH").unwrap_or_default();

        // Use pi's ABSOLUTE path: Rust's Command resolves the program name via
        // the PARENT's PATH, not the child env we set below, so a bare "pi"
        // would not be found when riddle is launched with a minimal PATH.
        let pi_bin = format!("{NODE_BIN}/pi");
        let mut child = Command::new(&pi_bin)
            .current_dir(DATA_DIR)
            .env("HOME", "/home/root")
            .env("PATH", format!("{NODE_BIN}:{path}"))
            .args([
                "--mode",
                "rpc",
                "--provider",
                "openai-codex",
                "--model",
                "gpt-5.4-mini",
                "--thinking",
                "off",
                // The diary only ever writes back — never let the model touch
                // tools; also trims the tool schemas from every request.
                "--no-tools",
                "--system-prompt",
                PERSONA,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Keep pi's stderr for diagnosis instead of discarding it.
            .stderr(
                std::fs::File::create("/tmp/riddle-oracle.log")
                    .map(Stdio::from)
                    .unwrap_or_else(|_| Stdio::null()),
            )
            .spawn()?;

        let pid = child.id();
        eprintln!("riddle: oracle pi rpc spawned (pid {pid}, bin {pi_bin})");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Arc<Mutex<Option<Sender<Result<String, String>>>>> =
            Arc::new(Mutex::new(None));

        // Reader thread: parse JSONL events, streaming each completed sentence
        // to the diary the moment it exists — the quill writes far slower than
        // the model streams, so the rest arrives while the first line is drawn.
        let pending_r = Arc::clone(&pending);
        let asked: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let asked_r = Arc::clone(&asked);
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut last_text = String::new();
            // Byte offset into `last_text` already sent to the diary.
            let mut delivered = 0usize;
            for line in reader.split(b'\n').map_while(Result::ok) {
                let Ok(s) = String::from_utf8(line) else {
                    continue;
                };
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                // Cheap field extraction avoids a JSON dep; the event stream is
                // well-formed one-object-per-line.
                let ev_type = json_str_field(s, "type");
                match ev_type.as_deref() {
                    // message_update carries the assistant message's running
                    // full text; deliver every newly completed sentence.
                    Some("message_update") => {
                        if let Some(t) = extract_assistant_text(s) {
                            if !t.is_empty() {
                                last_text = t;
                                if let Some(cut) = sentence_cut(&last_text, delivered) {
                                    if delivered == 0 {
                                        if let Some(t0) = asked_r.lock().unwrap().take() {
                                            eprintln!(
                                                "riddle: oracle first chunk +{}ms",
                                                t0.elapsed().as_millis()
                                            );
                                        }
                                    }
                                    deliver(
                                        &pending_r,
                                        &last_text[delivered..cut],
                                        delivered == 0,
                                        false,
                                    );
                                    delivered = cut;
                                }
                            }
                        }
                    }
                    // message_end has the definitive full text: flush the rest.
                    // (agent_end is NOT used for text — its `messages` array
                    // also contains user messages, which extract_assistant_text
                    // would wrongly concatenate in a multi-turn session.)
                    Some("message_end") => {
                        if let Some(t) = extract_assistant_text(s) {
                            if !t.is_empty() {
                                last_text = t;
                            }
                        }
                        if let Some(rest) = last_text.get(delivered..) {
                            if !rest.is_empty() {
                                if delivered == 0 {
                                    if let Some(t0) = asked_r.lock().unwrap().take() {
                                        eprintln!(
                                            "riddle: oracle first chunk +{}ms (at message_end)",
                                            t0.elapsed().as_millis()
                                        );
                                    }
                                }
                                deliver(&pending_r, rest, delivered == 0, true);
                            }
                        }
                        delivered = last_text.len();
                    }
                    // agent_end: the turn is over. Drop the sender so the
                    // diary's receiver disconnects (= no more ink coming).
                    Some("agent_end") => {
                        if let Some(tx) = pending_r.lock().unwrap().take() {
                            if delivered == 0 {
                                let _ = tx.send(Err("empty reply".into()));
                            }
                        }
                        last_text.clear();
                        delivered = 0;
                    }
                    _ => {}
                }
            }
            // Process died: fail any in-flight request.
            if let Some(tx) = pending_r.lock().unwrap().take() {
                let _ = tx.send(Err("pi rpc process exited".into()));
            }
        });

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            asked,
            _child: child,
        })
    }

    /// Send a handwriting turn. Reply chunks are delivered on `tx` as they
    /// stream; `tx` is dropped when the reply is complete.
    pub fn ask(&self, png_path: &str, tx: Sender<Result<String, String>>) {
        let img = match std::fs::read(png_path) {
            Ok(b) => base64(&b),
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return;
            }
        };
        *self.pending.lock().unwrap() = Some(tx.clone());
        *self.asked.lock().unwrap() = Some(std::time::Instant::now());

        let cmd = format!(
            "{{\"type\":\"prompt\",\"message\":{},\"images\":[{{\"type\":\"image\",\"data\":\"{}\",\"mimeType\":\"image/png\"}}]}}\n",
            json_quote("Reply to what is written in the diary."),
            img
        );
        let mut stdin = self.stdin.lock().unwrap();
        if stdin
            .write_all(cmd.as_bytes())
            .and_then(|_| stdin.flush())
            .is_err()
        {
            if let Some(tx) = self.pending.lock().unwrap().take() {
                let _ = tx.send(Err("pi rpc write failed".into()));
            }
        }
    }
}

/// Any OpenAI-compatible chat backend. No warm process: each turn opens a
/// streaming `/chat/completions` request on its own thread and forwards
/// sentence-sized chunks as SSE deltas arrive.
pub struct HttpOracle {
    base: String, // e.g. https://api.openai.com/v1  (no trailing slash)
    key: String,
    models: Vec<String>,
    max_tokens: u32,
    reasoning: Option<String>, // "reasoning_effort" value, e.g. "low"
}

impl HttpOracle {
    pub fn new() -> std::io::Result<Self> {
        let key = std::env::var("RIDDLE_OPENAI_KEY")
            .map_err(|_| std::io::Error::other("RIDDLE_OPENAI_KEY not set"))?;
        let base = std::env::var("RIDDLE_OPENAI_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let base = base.trim_end_matches('/').to_string();
        // Vision-capable defaults; override with RIDDLE_OPENAI_MODEL, or set
        // RIDDLE_OPENAI_MODELS to a comma-separated fallback list.
        let models = std::env::var("RIDDLE_OPENAI_MODELS")
            .ok()
            .map(|v| split_models(&v))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                vec![std::env::var("RIDDLE_OPENAI_MODEL")
                    .unwrap_or_else(|_| "gpt-4o-mini".to_string())]
            });
        // Thinking models (Gemini 3.x, o-series…) count hidden reasoning
        // tokens against max_tokens: a tight cap starves the visible reply to
        // one sentence (finish_reason=length). The persona already keeps
        // replies short, so the cap is only a runaway guard — leave headroom.
        let max_tokens = std::env::var("RIDDLE_OPENAI_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000);
        // Sent as "reasoning_effort" only when set: reasoning models accept it
        // ("low" ≈ faster first ink), but some providers reject the field on
        // non-reasoning models, so it must stay out of the default request.
        let reasoning = std::env::var("RIDDLE_OPENAI_REASONING").ok();
        eprintln!(
            "riddle: http oracle base={base} models={} max_tokens={max_tokens} reasoning={}",
            models.join(","),
            reasoning.as_deref().unwrap_or("-")
        );
        Ok(Self {
            base,
            key,
            models,
            max_tokens,
            reasoning,
        })
    }

    pub fn ask(&self, png_path: &str, tx: Sender<Result<String, String>>) {
        let img = match std::fs::read(png_path) {
            Ok(b) => base64(&b),
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return;
            }
        };
        let (base, key, models) = (self.base.clone(), self.key.clone(), self.models.clone());
        let max_tokens = self.max_tokens;
        let reasoning_field = self
            .reasoning
            .as_deref()
            .map(|r| format!("\"reasoning_effort\":{},", json_quote(r)))
            .unwrap_or_default();

        thread::spawn(move || {
            let mut last_err = String::from("no models configured");
            for model in models {
                eprintln!("riddle: oracle trying model={model}");
                match ask_http_once(
                    &base,
                    &key,
                    &model,
                    max_tokens,
                    &reasoning_field,
                    &img,
                    PERSONA,
                    &tx,
                ) {
                    Ok(()) => return,
                    Err(e) => {
                        eprintln!("riddle: oracle model {model} failed: {e}");
                        last_err = format!("{model}: {e}");
                    }
                }
            }
            let _ = tx.send(Err(last_err));
            // tx drops here → the diary's receiver disconnects = reply complete.
        });
    }
}

fn split_models(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(feature = "carlito")]
fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(default)
}

fn ask_http_once(
    base: &str,
    key: &str,
    model: &str,
    max_tokens: u32,
    reasoning_field: &str,
    img: &str,
    persona: &str,
    tx: &Sender<Result<String, String>>,
) -> Result<(), String> {
    // OpenAI chat-completions with a data-URI image part, streaming.
    let body = format!(
        concat!(
            "{{\"model\":{},\"stream\":true,\"max_tokens\":{},{}",
            "\"messages\":[",
            "{{\"role\":\"system\",\"content\":{}}},",
            "{{\"role\":\"user\",\"content\":[",
            "{{\"type\":\"text\",\"text\":{}}},",
            "{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,{}\"}}}}",
            "]}}]}}"
        ),
        json_quote(model),
        max_tokens,
        reasoning_field,
        json_quote(persona),
        json_quote(USER_REQUEST),
        img,
    );

    let asked = std::time::Instant::now();
    let resp = ureq::post(&format!("{base}/chat/completions"))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&body);

    let reader = match resp {
        Ok(r) => r.into_reader(),
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            return Err(format!("http {code}: {}", detail.trim()));
        }
        Err(e) => return Err(format!("request failed: {e}")),
    };

    // Parse the SSE stream: lines of `data: {json}` whose delta.content
    // fragments accumulate; deliver each completed sentence as it lands.
    let mut acc = String::new();
    let mut delivered = 0usize;
    let mut first = true;
    for line in BufReader::new(reader).lines().map_while(Result::ok) {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if let Some(frag) = sse_delta_content(data) {
            if frag.is_empty() {
                continue;
            }
            acc.push_str(&frag);
            if let Some(cut) = sentence_cut(&acc, delivered) {
                if first {
                    eprintln!(
                        "riddle: oracle first chunk +{}ms",
                        asked.elapsed().as_millis()
                    );
                    first = false;
                }
                let chunk = acc[delivered..cut].to_string();
                let _ = tx.send(Ok(clean(&chunk)));
                delivered = cut;
            }
        }
    }
    // Flush any trailing text past the last sentence break.
    if delivered < acc.len() {
        let rest = acc[delivered..].trim();
        if !rest.is_empty() {
            let _ = tx.send(Ok(clean(rest)));
            delivered = acc.len();
        }
    }
    if delivered == 0 {
        return Err("empty reply".into());
    }
    Ok(())
}

/// Pull `choices[0].delta.content` out of one SSE `data:` JSON object.
fn sse_delta_content(s: &str) -> Option<String> {
    // The delta object is small and well-formed; find the content string after
    // the `"delta":` marker so we don't match a `content` elsewhere.
    let d = s.find("\"delta\"")?;
    json_str_field(&s[d..], "content")
}

/// Trim and strip stray surrounding quotes from a reply fragment.
fn clean(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix('"').unwrap_or(t);
    let t = t.strip_suffix('"').unwrap_or(t);
    t.to_string()
}

/// Send one chunk of reply text without consuming the sender (more chunks may
/// follow until agent_end drops it). Strips a stray wrapping quote from the
/// reply's very first / very last chunk.
fn deliver(
    pending: &Arc<Mutex<Option<Sender<Result<String, String>>>>>,
    chunk: &str,
    first: bool,
    last: bool,
) {
    let mut t = chunk.trim();
    if first {
        t = t.strip_prefix('"').unwrap_or(t);
    }
    if last {
        t = t.strip_suffix('"').unwrap_or(t);
    }
    let t = t.trim();
    if t.is_empty() {
        return;
    }
    if let Some(tx) = pending.lock().unwrap().as_ref() {
        let _ = tx.send(Ok(t.to_string()));
    }
}

/// End of the LAST complete sentence in `text` after byte offset `from`:
/// sentence punctuation followed by whitespace or end-of-text. Returns the
/// offset just past the punctuation, or None if no sentence has completed.
/// Chunks shorter than a few characters are not worth an early delivery.
fn sentence_cut(text: &str, from: usize) -> Option<usize> {
    let tail = text.get(from..)?;
    let mut cut = None;
    for (i, c) in tail.char_indices() {
        if matches!(c, '.' | '!' | '?' | '…') {
            let end = i + c.len_utf8();
            if tail[end..].chars().next().is_none_or(char::is_whitespace) && end >= 4 {
                cut = Some(from + end);
            }
        }
    }
    cut
}

/// Extract a top-level string field's value (first match; unescaped).
fn json_str_field(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = s.find(&pat)? + pat.len();
    let rest = &s[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    match n {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        // \uXXXX — needed for accented replies (French, em-dash…).
                        'u' => {
                            let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                            if let Some(ch) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
            }
            '"' => break,
            _ => out.push(c),
        }
    }
    Some(out)
}

#[cfg(feature = "carlito")]
fn json_u32_field(s: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\":");
    let rest = &s[s.find(&needle)? + needle.len()..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(feature = "carlito")]
fn json_str_fields(s: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":\"");
    let mut fields = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos..];
        if let Some(value) = json_str_field(rest, key) {
            fields.push(value);
        }
        rest = &rest[needle.len().min(rest.len())..];
    }
    fields
}

/// Pull the assistant reply text out of an event line. The event carries a
/// `message` object with `"role":"assistant"` and `content:[{type:text,text:…}]`.
/// We only trust text that belongs to an assistant message (the user echo also
/// contains a "text" field, which we must NOT return).
fn extract_assistant_text(s: &str) -> Option<String> {
    // Require this line to be an assistant message.
    if !s.contains("\"role\":\"assistant\"") {
        return None;
    }
    // Collect every "text":"…" occurrence inside the FIRST assistant section
    // only. message_update lines carry the running text twice (in
    // assistantMessageEvent.partial AND a top-level message); reading past the
    // next role marker would double every streamed chunk.
    let role_pos = s.find("\"role\":\"assistant\"")?;
    let after = &s[role_pos + "\"role\":\"assistant\"".len()..];
    let tail = match after.find("\"role\":\"") {
        Some(p) => &after[..p],
        None => after,
    };
    let mut out = String::new();
    let mut idx = 0;
    let needle = "\"text\":\"";
    while let Some(rel) = tail[idx..].find(needle) {
        let start = idx + rel + needle.len();
        // Decode the JSON string starting at `start`.
        let mut chars = tail[start..].chars();
        let mut piece = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        piece.push(match n {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            other => other,
                        });
                    }
                }
                '"' => break,
                _ => piece.push(c),
            }
        }
        out.push_str(&piece);
        // Advance past this occurrence.
        idx = start;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_delta_extraction() {
        let line = r#"{"choices":[{"delta":{"content":"Hello"},"index":0}]}"#;
        assert_eq!(sse_delta_content(line).as_deref(), Some("Hello"));
        // role-only delta (first SSE frame) has no content.
        let role = r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#;
        assert_eq!(sse_delta_content(role), None);
    }

    #[test]
    fn sse_decodes_unicode_and_escapes() {
        // OpenAI escapes accents and em-dashes; the diary answers in French.
        let line = r#"{"choices":[{"delta":{"content":"Déjà vu — oui"}}]}"#;
        assert_eq!(sse_delta_content(line).as_deref(), Some("Déjà vu — oui"));
        let nl = r#"{"choices":[{"delta":{"content":"line\nbreak"}}]}"#;
        assert_eq!(sse_delta_content(nl).as_deref(), Some("line\nbreak"));
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn clean_strips_wrapping_quotes() {
        assert_eq!(clean("  \"hello\"  "), "hello");
        assert_eq!(clean("plain"), "plain");
    }

    #[cfg(feature = "carlito")]
    #[test]
    fn gemini_stream_ignores_thought_text() {
        let mut outputs = Vec::new();
        let thought_start = r#"{"index":0,"step":{"type":"thought"},"event_type":"step.start"}"#;
        let thought = r#"{"index":0,"delta":{"content":{"type":"text","text":"private reasoning"},"type":"thought_summary"},"event_type":"step.delta"}"#;
        let answer_start =
            r#"{"index":1,"step":{"type":"model_output"},"event_type":"step.start"}"#;
        let answer = r#"{"index":1,"delta":{"text":"I'm Carlito.","type":"text"},"event_type":"step.delta"}"#;

        assert_eq!(gemini_output_delta(thought_start, &mut outputs), None);
        assert_eq!(gemini_output_delta(thought, &mut outputs), None);
        assert_eq!(gemini_output_delta(answer_start, &mut outputs), None);
        assert_eq!(
            gemini_output_delta(answer, &mut outputs).as_deref(),
            Some("I'm Carlito.")
        );
    }

    #[test]
    fn citations_keep_verifiable_urls_and_deduplicate() {
        let value = serde_json::json!({"annotations": [
            {"type":"url_citation", "title":"Example", "url":"https://example.org/facts"},
            {"type":"url_citation", "title":"Example", "url":"https://example.org/facts"}
        ]});
        let mut sources = Vec::new();
        collect_citations(&value, &mut sources);
        assert_eq!(sources, ["Example: https://example.org/facts"]);
    }

    #[test]
    fn search_quota_retries_without_search_and_labels_the_answer() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request.get("tools").is_some(), attempt == 0);
                assert_eq!(request["store"], false);
                let (status, response) = if attempt == 0 {
                    ("429 Too Many Requests", "quota_exceeded".to_string())
                } else {
                    ("200 OK", concat!(
                        "data: {\"index\":1,\"step\":{\"type\":\"model_output\"},\"event_type\":\"step.start\"}\n\n",
                        "data: {\"index\":1,\"delta\":{\"type\":\"text\",\"text\":\"Answer.\"},\"event_type\":\"step.delta\"}\n\n",
                        "data: {\"event_type\":\"interaction.completed\"}\n\n"
                    ).to_string())
                };
                write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        let config = GeminiConfig {
            base: format!("http://{address}/interactions"),
            key: "test".into(),
            models: vec!["test-model".into()],
            max_tokens: 100,
            web_search: true,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        ask_gemini(&config, "image", &[], &tx).unwrap();
        drop(tx);
        let messages: Vec<_> = rx.into_iter().map(Result::unwrap).collect();
        assert_eq!(messages[0], "Answer.");
        assert!(messages[1].contains("Live web search was unavailable"));
        server.join().unwrap();
    }
}
