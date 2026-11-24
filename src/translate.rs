use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::settings::Profile;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEADERS_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn http_client() -> Client {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(IDLE_TIMEOUT)
        .user_agent("MiniTranslation")
        .build()
        .expect("system TLS backend is available")
}

/// The language paired with Chinese: Chinese text is translated into it, anything
/// else into Chinese.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    English,
    Japanese,
    Korean,
    French,
    German,
    Spanish,
    Russian,
}

impl Language {
    pub const ALL: [Self; 7] = [
        Self::English,
        Self::Japanese,
        Self::Korean,
        Self::French,
        Self::German,
        Self::Spanish,
        Self::Russian,
    ];

    /// `(code, Chinese name, one-character abbreviation, BCP 47 tag)`
    fn info(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::English => ("en", "英语", "英", "en-US"),
            Self::Japanese => ("ja", "日语", "日", "ja-JP"),
            Self::Korean => ("ko", "韩语", "韩", "ko-KR"),
            Self::French => ("fr", "法语", "法", "fr-FR"),
            Self::German => ("de", "德语", "德", "de-DE"),
            Self::Spanish => ("es", "西班牙语", "西", "es-ES"),
            Self::Russian => ("ru", "俄语", "俄", "ru-RU"),
        }
    }

    pub fn name(self) -> &'static str {
        self.info().1
    }

    pub fn abbreviation(self) -> &'static str {
        self.info().2
    }

    pub fn tag(self) -> &'static str {
        self.info().3
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.info().0)
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Ok(Self::ALL
            .into_iter()
            .find(|l| l.info().0 == code)
            .unwrap_or_default())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    pub source_is_chinese: bool,
}

/// Streams a translation of `text` (already normalized) between Chinese and `language`.
/// `on_progress` receives the full visible text generated so far after every delta.
pub async fn translate(
    client: &Client,
    text: &str,
    language: Language,
    profile: &Profile,
    on_progress: &mut (impl FnMut(&str) + Send),
) -> Result<Translation> {
    let source_is_chinese = is_mainly_chinese(text);
    let body = request_body(&profile.model, &prompt(text, source_is_chinese, language));
    let request = client
        .post(endpoint(&profile.api_base_url))
        .bearer_auth(profile.api_key.trim())
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string());

    let mut response = tokio::time::timeout(HEADERS_TIMEOUT, request.send())
        .await
        .map_err(|_| anyhow!("接口响应超时。"))?
        .map_err(describe)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "接口返回 {}：{}",
            status.as_u16(),
            truncate(body.trim(), 300)
        );
    }

    let mut stream = ChatStream::default();
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        if stream.push(&chunk)? {
            on_progress(without_reasoning(&stream.content));
        }
        if stream.done {
            break;
        }
    }
    let content = stream.finish()?;
    let text = without_reasoning(&content).trim();
    if text.is_empty() {
        bail!("接口返回了空结果。");
    }
    Ok(Translation {
        text: text.to_owned(),
        source_is_chinese,
    })
}

fn describe(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("接口响应超时。")
    } else if error.is_connect() {
        let host = error
            .url()
            .and_then(|url| url.host_str())
            .unwrap_or_default();
        anyhow!("无法连接 {host}")
    } else {
        anyhow::Error::new(error)
    }
}

/// Chinese characters carry more meaning each, so they outvote Latin letters 3:1:
/// Chinese prose sprinkled with English terms is still Chinese.
/// Kana marks Japanese, whose kanji would otherwise count as Chinese.
pub fn is_mainly_chinese(text: &str) -> bool {
    if text.chars().any(|c| matches!(c, '\u{3040}'..='\u{30FF}')) {
        return false;
    }
    let (zh, en) = text.chars().fold((0usize, 0usize), |(zh, en), c| match c {
        '\u{4E00}'..='\u{9FFF}' => (zh + 1, en),
        c if c.is_ascii_alphabetic() => (zh, en + 1),
        _ => (zh, en),
    });
    zh > 0 && zh * 3 >= en
}

/// Collapses runs of spaces/tabs and blank lines while keeping paragraph breaks.
pub fn normalize_input(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_blank = false;
    for line in raw.split('\n') {
        let words: Vec<&str> = line
            .split([' ', '\t', '\r'])
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            pending_blank = true;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if pending_blank { "\n\n" } else { "\n" });
        }
        pending_blank = false;
        out.push_str(&words.join(" "));
    }
    out.trim().to_owned()
}

/// Prompt template of Tencent Hunyuan-MT, which general chat models follow just as well.
/// Reasoning models served through some OpenAI-compatible gateways put their
/// thinking inline as a leading `<think>…</think>` block.
fn without_reasoning(content: &str) -> &str {
    const OPEN: &str = "<think>";
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix(OPEN) {
        return rest
            .split_once("</think>")
            .map_or("", |(_, answer)| answer.trim_start());
    }
    if !trimmed.is_empty() && OPEN.starts_with(trimmed) {
        return "";
    }
    content
}

fn prompt(text: &str, source_is_chinese: bool, language: Language) -> String {
    let text = text.trim();
    if source_is_chinese {
        let target = language.name();
        format!("将以下文本翻译为 {target}，注意只需要输出翻译后的结果，不要额外解释：\n\n{text}")
    } else {
        format!(
            "Translate the following text into Simplified Chinese. Note that you should only \
             output the translated result without any additional explanation:\n\n{text}"
        )
    }
}

fn request_body(model: &str, prompt: &str) -> Value {
    let mut body = json!({
        "model": model.trim(),
        "messages": [{ "role": "user", "content": prompt }],
        "stream": true,
    });
    let lower = model.to_ascii_lowercase();
    if lower.contains("hy-mt") || lower.contains("hunyuan-mt") {
        // Recommended sampling for Hunyuan-MT; top_k and repetition_penalty are
        // vLLM extensions that OpenAI itself rejects, hence model-gated.
        let extra =
            json!({ "temperature": 0.7, "top_p": 0.6, "top_k": 20, "repetition_penalty": 1.05 });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
    }
    body
}

/// Accepts a bare host, a versioned base (`…/v1`) or the full completions URL.
fn endpoint(base_url: &str) -> String {
    let url = base_url.trim().trim_end_matches('/');
    if url.to_ascii_lowercase().ends_with("/chat/completions") {
        return url.to_owned();
    }
    let versioned = url.rsplit_once('/').is_some_and(|(_, last)| {
        last.strip_prefix(['v', 'V'])
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    if versioned {
        format!("{url}/chat/completions")
    } else {
        format!("{url}/v1/chat/completions")
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Option<Message>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

/// Incremental reader for an SSE completion stream that also accepts a plain JSON
/// body from servers ignoring `stream: true`.
#[derive(Default)]
struct ChatStream {
    pending: Vec<u8>,
    raw: Vec<u8>,
    content: String,
    sse: bool,
    done: bool,
}

impl ChatStream {
    /// Returns whether the content grew.
    fn push(&mut self, bytes: &[u8]) -> Result<bool> {
        let before = self.content.len();
        self.pending.extend_from_slice(bytes);
        let buffer = std::mem::take(&mut self.pending);
        let mut rest = buffer.as_slice();
        while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
            self.line(&rest[..newline])?;
            rest = &rest[newline + 1..];
            if self.done {
                rest = &[];
            }
        }
        self.pending = rest.to_vec();
        Ok(self.content.len() != before)
    }

    fn line(&mut self, line: &[u8]) -> Result<()> {
        let Some(data) = line.strip_prefix(b"data:") else {
            if !self.sse {
                self.raw.extend_from_slice(line);
                self.raw.push(b'\n');
            }
            return Ok(());
        };
        self.sse = true;
        let data = data.trim_ascii();
        match data {
            b"" => {}
            b"[DONE]" => self.done = true,
            _ => self.apply(serde_json::from_slice(data).context("无法解析接口返回的数据")?)?,
        }
        Ok(())
    }

    fn apply(&mut self, chunk: Chunk) -> Result<()> {
        if let Some(error) = chunk.error {
            let message = error["message"]
                .as_str()
                .map_or_else(|| error.to_string(), str::to_owned);
            bail!("接口返回错误：{message}");
        }
        let piece = chunk
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.delta.or(choice.message))
            .and_then(|message| message.content);
        if let Some(piece) = piece {
            self.content.push_str(&piece);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<String> {
        if !self.done && !self.pending.is_empty() {
            let tail = std::mem::take(&mut self.pending);
            self.line(&tail)?;
        }
        if !self.sse && !self.raw.trim_ascii().is_empty() {
            let raw = std::mem::take(&mut self.raw);
            self.apply(serde_json::from_slice(&raw).context("无法解析接口返回的数据")?)?;
        }
        Ok(self.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_direction() {
        assert!(is_mainly_chinese("你好，世界"));
        assert!(is_mainly_chinese("我们用 Rust 重写"));
        assert!(!is_mainly_chinese("Hello world"));
        assert!(!is_mainly_chinese("A long English sentence with 一 word"));
        assert!(!is_mainly_chinese("12345"));
        assert!(!is_mainly_chinese("日本語を勉強します"));
    }

    #[test]
    fn strips_inline_reasoning() {
        assert_eq!(without_reasoning("<think>hmm</think>\n\nHello"), "Hello");
        assert_eq!(without_reasoning("  <think>still thinking"), "");
        assert_eq!(without_reasoning("<thi"), "");
        assert_eq!(without_reasoning("Hello <think>"), "Hello <think>");
    }

    #[test]
    fn prompt_names_the_target_language() {
        assert!(prompt("你好", true, Language::Japanese).contains("翻译为 日语"));
        assert!(prompt("hola", false, Language::Spanish).contains("Simplified Chinese"));
    }

    #[test]
    fn language_codes_round_trip() {
        for language in Language::ALL {
            let json = serde_json::to_string(&language).unwrap();
            assert_eq!(serde_json::from_str::<Language>(&json).unwrap(), language);
        }
        assert_eq!(
            serde_json::from_str::<Language>("\"xx\"").unwrap(),
            Language::English
        );
    }

    #[test]
    fn normalizes_whitespace_but_keeps_paragraphs() {
        assert_eq!(
            normalize_input("  a \t b  \r\n c\n\n\n\n d \n"),
            "a b\nc\n\nd"
        );
        assert_eq!(normalize_input("one\n  \ntwo"), "one\n\ntwo");
        assert_eq!(normalize_input(" \n \t"), "");
    }

    #[test]
    fn builds_endpoints() {
        assert_eq!(
            endpoint("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://x.ai/api/V3"),
            "https://x.ai/api/V3/chat/completions"
        );
        assert_eq!(
            endpoint("http://h/v1/chat/completions"),
            "http://h/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://h/vision"),
            "http://h/vision/v1/chat/completions"
        );
    }

    #[test]
    fn hunyuan_gets_sampling_parameters() {
        assert_eq!(request_body("HY-MT2-7B", "p")["top_k"], 20);
        assert!(request_body("gpt-4o-mini", "p").get("top_k").is_none());
    }

    #[test]
    fn parses_sse_split_across_chunks() {
        let mut stream = ChatStream::default();
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n\
                    data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\r\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\
                    data: [DONE]\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"ignored\"}}]}\n";
        let mut grew = 0;
        for chunk in body.as_bytes().chunks(7) {
            grew += usize::from(stream.push(chunk).unwrap());
        }
        assert_eq!(grew, 2);
        assert!(stream.done);
        assert_eq!(stream.finish().unwrap(), "你好");
    }

    #[test]
    fn accepts_non_streaming_json() {
        let mut stream = ChatStream::default();
        stream
            .push(b"{\"choices\":[{\"message\":\n{\"content\":\"hello\"}}]}")
            .unwrap();
        assert_eq!(stream.finish().unwrap(), "hello");
    }

    #[test]
    fn surfaces_stream_errors() {
        let mut stream = ChatStream::default();
        let error = stream
            .push(b"data: {\"error\":{\"message\":\"quota\"}}\n")
            .unwrap_err();
        assert!(error.to_string().contains("quota"));
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("中文测试", 2), "中文…");
        assert_eq!(truncate("ok", 5), "ok");
    }
}
