//! OpenRouter client. Bare REST, no SDK — it is one POST.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

const URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct Llm {
    http: reqwest::blocking::Client,
    key: String,
    pub model: String,
}

#[derive(Debug, Default)]
pub struct Reply {
    pub content: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Cost of the call in dollars, as OpenRouter counted it.
    pub cost: f64,
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    cost: f64,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
    #[serde(default)]
    code: Option<Value>,
}

impl Llm {
    pub fn new(model: &str) -> Result<Self> {
        let key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?;
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()?,
            key,
            model: model.to_string(),
        })
    }

    /// One call with strict structured output. Retries only what is worth
    /// retrying: throttling and 5xx.
    pub fn complete(&self, system: &str, user: &str, schema: &Value) -> Result<Reply> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user",   "content": user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "verdict", "strict": true, "schema": schema}
            },
            "temperature": 0,
            "usage": {"include": true},
        });

        let mut delay = Duration::from_secs(2);
        let mut last = String::new();

        for attempt in 1..=5 {
            let resp = self
                .http
                .post(URL)
                .bearer_auth(&self.key)
                .json(&body)
                .send();

            match resp {
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().unwrap_or_default();

                    if status.is_success() {
                        // Providers regularly answer 200 with an empty or
                        // broken body — network hiccup, not a refusal.
                        match parse(&text) {
                            Ok(reply) => return Ok(reply),
                            Err(e) => last = format!("{e:#}"),
                        }
                    } else {
                        last = format!("HTTP {status}: {}", clip(&text));
                        // 4xx other than 429 will not change on a retry.
                        if status.as_u16() != 429 && status.is_client_error() {
                            bail!("{last}");
                        }
                    }
                }
                Err(e) => last = e.to_string(),
            }

            if attempt < 5 {
                tracing::warn!("attempt {attempt} failed ({last}), waiting {:?}", delay);
                std::thread::sleep(delay);
                delay *= 2;
            }
        }

        bail!("the model stayed unreachable after 5 attempts: {last}")
    }
}

/// Parse a successful answer. Anything that fails here is retried.
fn parse(text: &str) -> Result<Reply> {
    let parsed: ApiResponse = serde_json::from_str(text)
        .with_context(|| format!("unrecognized answer: {}", clip(text)))?;
    if let Some(e) = parsed.error {
        bail!("OpenRouter: {} {:?}", e.message, e.code);
    }
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .filter(|c| !c.trim().is_empty())
        .context("empty answer from the model")?;
    let u = parsed.usage.unwrap_or(Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        cost: 0.0,
    });
    Ok(Reply {
        content,
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        cost: u.cost,
    })
}

fn clip(s: &str) -> String {
    s.chars().take(400).collect()
}
