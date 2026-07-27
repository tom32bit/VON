//! Asking a provider what models your key can actually reach.
//!
//! The curated catalog in `catalog.rs` exists to give a sensible default with
//! real prices and a real RAM figure next to it. It is not meant to be the
//! ceiling: providers ship models faster than an app store review cycle, your
//! account may have access to something the catalog has never heard of. So the
//! picker asks.
//!
//! Every provider here has a list endpoint. The shapes differ — Anthropic's is
//! close to OpenAI's, Gemini's is its own thing — so each gets a small reader
//! and they all produce the same rows.
//!
//! What comes back is merged with the catalog rather than replacing it: a model
//! we know about keeps its blurb and prices, and everything else arrives with
//! its id and whatever the provider volunteered.

use serde_json::Value;

use crate::error::{Result, VonError};
use crate::llm::{catalog, Provider};
use crate::state;

/// One model a provider says is available.
#[derive(Debug, Clone)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    /// 0 when the provider does not say.
    pub context_tokens: i64,
    pub blurb: String,
    /// Negative when unknown — the UI hides the cost hint rather than claiming
    /// a model is free.
    pub input_price: f32,
    pub output_price: f32,
    /// Whether VON ships a description and prices for this one.
    pub curated: bool,
}

/// Model ids that are not chat models.
///
/// OpenAI's list endpoint returns embeddings, speech, images and moderation
/// alongside the chat models, and offering those in a model picker is offering
/// a guaranteed failure. Matched as substrings because the families are named
/// consistently even as versions change.
const NOT_CHAT: &[&str] = &[
    "embed",
    "whisper",
    "tts",
    "dall-e",
    "moderation",
    "rerank",
    "guard",
    "davinci",
    "babbage",
    "audio",
    "transcribe",
    "image",
    "realtime",
    "search-",
    "codex-mini",
];

fn is_chat_model(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    !NOT_CHAT.iter().any(|bad| lower.contains(bad))
}

/// Checks a key by listing models with it.
///
/// Cheaper and far faster than the old approach of sending a real chat
/// completion: no tokens are spent, no model has to be picked, and a bad key
/// comes back as a 401 in one round trip instead of a streaming request that
/// sits open. NVIDIA NIM in particular left the button saying "Testing
/// connection" for as long as the connection took to fail.
pub fn test_key(provider: Provider, key: &str) -> Result<()> {
    let body = fetch(provider, key)?;
    let count = read_for(provider, &body).len();
    if count == 0 {
        return Err(VonError::Other(format!(
            "{} accepted the key but offered no chat models on it",
            provider.display_name()
        )));
    }
    Ok(())
}

/// Lists the models this provider's stored key can reach.
///
/// Falls back to the curated entries when there is no key, so the picker is
/// never empty and the user can still see what VON would suggest.
pub fn list_models(provider: Provider) -> Result<Vec<ProviderModel>> {
    if provider == Provider::Local {
        return Err(VonError::InvalidArgument(
            "on-device models are listed in the Model Hub, not fetched from a provider".into(),
        ));
    }

    let key = crate::db::get_api_key(provider.as_str())?
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| VonError::MissingApiKey(provider.as_str().to_string()))?;

    let body = fetch(provider, &key)?;
    let mut models = read_for(provider, &body);

    if models.is_empty() {
        return Err(VonError::Other(format!(
            "{} returned no usable chat models for this key",
            provider.display_name()
        )));
    }

    enrich(provider, &mut models);
    // Curated first — they are the ones with prices and a description — then
    // everything else alphabetically, which is the only ordering that stays
    // stable as providers add models.
    models.sort_by(|a, b| b.curated.cmp(&a.curated).then_with(|| a.id.cmp(&b.id)));
    Ok(models)
}

/// The curated entries for a provider, used when there is no key to ask with.
pub fn curated(provider: Provider) -> Vec<ProviderModel> {
    catalog::cloud_catalog()
        .into_iter()
        .filter(|m| m.provider == provider.as_str())
        .map(|m| ProviderModel {
            id: m.id,
            name: m.name,
            provider: m.provider,
            context_tokens: m.context_tokens,
            blurb: m.blurb,
            input_price: m.input_price,
            output_price: m.output_price,
            curated: true,
        })
        .collect()
}

/// Copies the catalog's description and prices onto anything we recognise.
fn enrich(provider: Provider, models: &mut [ProviderModel]) {
    let known = catalog::cloud_catalog();
    for model in models.iter_mut() {
        let Some(entry) = known
            .iter()
            .find(|k| k.provider == provider.as_str() && k.id == model.id)
        else {
            continue;
        };
        model.name = entry.name.clone();
        model.blurb = entry.blurb.clone();
        model.input_price = entry.input_price;
        model.output_price = entry.output_price;
        model.curated = true;
        if model.context_tokens == 0 {
            model.context_tokens = entry.context_tokens;
        }
    }
}

fn fetch(provider: Provider, key: &str) -> Result<Value> {
    let (url, headers): (&str, Vec<(&str, String)>) = match provider {
        Provider::Anthropic => (
            "https://api.anthropic.com/v1/models?limit=1000",
            vec![
                ("x-api-key", key.to_string()),
                ("anthropic-version", "2023-06-01".to_string()),
            ],
        ),
        Provider::Gemini => (
            "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000",
            vec![("x-goog-api-key", key.to_string())],
        ),
        Provider::OpenRouter => (
            "https://openrouter.ai/api/v1/models",
            vec![("authorization", format!("Bearer {key}"))],
        ),
        _ => (
            "https://api.openai.com/v1/models",
            vec![("authorization", format!("Bearer {key}"))],
        ),
    };

    let label = provider.display_name();
    state::block_on(async move {
        // Unlike a completion, a model listing is a small GET with a bounded
        // answer, so it gets a bounded wait. The shared client has no global
        // timeout because streaming completions legitimately stay open for
        // minutes — which is why this one has to be set here.
        let mut request = state::http()
            .get(url)
            .timeout(std::time::Duration::from_secs(20));
        for (name, value) in headers {
            request = request.header(name, value);
        }
        // Retried once, because this is a GET with no side effects and the
        // failure it guards against is a pooled connection the server closed
        // while we were not looking. Tightening the pool timeout makes that
        // rarer; it does not make it impossible, and a key that reports itself
        // broken because of one dead socket is a bad way to find out.
        let mut attempt = request
            .try_clone()
            .ok_or_else(|| VonError::Other("could not prepare the request".into()))?
            .send()
            .await;
        if attempt.is_err() {
            attempt = request.send().await;
        }
        let response = attempt
            .map_err(|e| VonError::Network(format!("could not reach {label}: {e}")))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .or_else(|| v.pointer("/message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());

            return Err(match status.as_u16() {
                401 | 403 => VonError::MissingApiKey(format!(
                    "{label}: the key was rejected ({detail})"
                )),
                status => VonError::Provider {
                    provider: label.to_string(),
                    status,
                    body: detail,
                },
            });
        }

        serde_json::from_str(&text)
            .map_err(|e| VonError::Other(format!("{label} sent a model list we cannot read: {e}")))
    })?
}

fn read_for(provider: Provider, body: &Value) -> Vec<ProviderModel> {
    match provider {
        Provider::Gemini => read_gemini(body),
        Provider::Anthropic => read_anthropic(body),
        Provider::OpenRouter => read_openrouter(body),
        _ => read_openai(body),
    }
}

/// OpenRouter's listing, which is the richest of the four.
///
/// It carries a display name, a context window and real per-token prices, so
/// unlike the others nothing has to be guessed or left blank. It also declares
/// `supported_parameters`, which is the useful part: an agent is worthless
/// without tool calling, and OpenRouter lists hundreds of models that cannot do
/// it. Offering those would be offering a guaranteed failure, so they are
/// filtered out on the provider's own say-so rather than on a guess about the
/// name.
fn read_openrouter(body: &Value) -> Vec<ProviderModel> {
    let Some(entries) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;

            let calls_tools = entry
                .get("supported_parameters")
                .and_then(Value::as_array)
                .map(|params| params.iter().any(|p| p.as_str() == Some("tools")))
                .unwrap_or(false);
            if !calls_tools {
                return None;
            }

            Some(ProviderModel {
                name: entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                id: id.to_string(),
                provider: String::new(),
                context_tokens: entry
                    .get("context_length")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                blurb: String::new(),
                // Quoted strings in dollars per token; the UI works in dollars
                // per million, which is the unit every provider prices in.
                input_price: per_million(entry.pointer("/pricing/prompt")),
                output_price: per_million(entry.pointer("/pricing/completion")),
                curated: false,
            })
        })
        .collect()
}

/// Converts OpenRouter's per-token price string to dollars per million.
///
/// Negative when absent or unparseable, which is how the UI is told to say
/// nothing rather than to claim a model is free.
fn per_million(value: Option<&Value>) -> f32 {
    value
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .map(|per_token| (per_token * 1_000_000.0) as f32)
        .unwrap_or(-1.0)
}

/// OpenAI's `{ data: [{ id, ... }] }`.
fn read_openai(body: &Value) -> Vec<ProviderModel> {
    let Some(entries) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            if !is_chat_model(id) {
                return None;
            }
            // Some listings mark retired models inactive rather than removing
            // them; an inactive model is not selectable.
            if entry.get("active").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            Some(ProviderModel {
                id: id.to_string(),
                name: id.to_string(),
                provider: String::new(),
                // Volunteered by some listings, absent from OpenAI's.
                context_tokens: entry
                    .get("context_window")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                blurb: String::new(),
                input_price: -1.0,
                output_price: -1.0,
                curated: false,
            })
        })
        .collect()
}

/// Anthropic's `{ data: [{ id, display_name }] }` — all chat models.
fn read_anthropic(body: &Value) -> Vec<ProviderModel> {
    let Some(entries) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            Some(ProviderModel {
                id: id.to_string(),
                name: entry
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                provider: String::new(),
                context_tokens: 0,
                blurb: String::new(),
                input_price: -1.0,
                output_price: -1.0,
                curated: false,
            })
        })
        .collect()
}

/// Gemini's `{ models: [{ name: "models/x", supportedGenerationMethods }] }`.
///
/// The list includes embedding and image models, but unlike OpenAI it says so
/// properly — so this filters on the declared capability rather than guessing
/// from the name.
fn read_gemini(body: &Value) -> Vec<ProviderModel> {
    let Some(entries) = body.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let supports_chat = entry
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .map(|methods| {
                    methods
                        .iter()
                        .any(|m| m.as_str() == Some("generateContent"))
                })
                .unwrap_or(false);
            if !supports_chat {
                return None;
            }

            // Names arrive fully qualified; the API takes the bare id.
            let id = entry
                .get("name")
                .and_then(Value::as_str)?
                .trim_start_matches("models/")
                .to_string();
            if id.is_empty() {
                return None;
            }

            Some(ProviderModel {
                name: entry
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                context_tokens: entry
                    .get("inputTokenLimit")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                id,
                provider: String::new(),
                blurb: String::new(),
                input_price: -1.0,
                output_price: -1.0,
                curated: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_listings_drop_everything_that_is_not_a_chat_model() {
        // The real endpoint returns all of these together, and offering an
        // embedding model in a chat picker is offering a guaranteed failure.
        let body = json!({"data": [
            {"id": "gpt-4o"},
            {"id": "text-embedding-3-small"},
            {"id": "whisper-1"},
            {"id": "dall-e-3"},
            {"id": "tts-1-hd"},
            {"id": "omni-moderation-latest"},
            {"id": "gpt-4o-mini"},
        ]});
        let ids: Vec<String> = read_openai(&body).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["gpt-4o", "gpt-4o-mini"]);
    }

    #[test]
    fn gemini_filters_on_the_declared_capability_not_the_name() {
        // `embedding` in the name is a hint; `supportedGenerationMethods` is
        // the answer, and Gemini gives it.
        let body = json!({"models": [
            {
                "name": "models/gemini-2.0-flash",
                "displayName": "Gemini 2.0 Flash",
                "inputTokenLimit": 1048576,
                "supportedGenerationMethods": ["generateContent", "countTokens"],
            },
            {
                "name": "models/text-embedding-004",
                "displayName": "Text Embedding 004",
                "supportedGenerationMethods": ["embedContent"],
            },
        ]});
        let models = read_gemini(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-2.0-flash", "the models/ prefix is stripped");
        assert_eq!(models[0].name, "Gemini 2.0 Flash");
        assert_eq!(models[0].context_tokens, 1_048_576);
    }

    #[test]
    fn anthropic_display_names_are_preferred_over_ids() {
        let body = json!({"data": [
            {"id": "claude-opus-5-20260101", "display_name": "Claude Opus 5"},
            {"id": "claude-bare"},
        ]});
        let models = read_anthropic(&body);
        assert_eq!(models[0].name, "Claude Opus 5");
        assert_eq!(models[1].name, "claude-bare", "no display name falls back to the id");
    }

    /// OpenRouter lists hundreds of models, a great many of which cannot call
    /// a tool. An agent without tool calling is worthless, so offering one is
    /// offering a guaranteed failure — and the provider says which is which.
    #[test]
    fn openrouter_offers_only_models_that_can_call_tools() {
        let body = json!({"data": [
            {
                "id": "anthropic/claude-sonnet-4.5",
                "name": "Anthropic: Claude Sonnet 4.5",
                "context_length": 200000,
                "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                "supported_parameters": ["tools", "tool_choice", "max_tokens"],
            },
            {
                "id": "some/completion-only",
                "name": "Completion Only",
                "context_length": 8192,
                "pricing": {"prompt": "0.0000001", "completion": "0.0000002"},
                "supported_parameters": ["max_tokens", "temperature"],
            },
        ]});

        let models = read_openrouter(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4.5");
        assert_eq!(models[0].name, "Anthropic: Claude Sonnet 4.5");
        assert_eq!(models[0].context_tokens, 200_000);
    }

    #[test]
    fn openrouter_prices_are_converted_to_dollars_per_million() {
        // Quoted per-token strings; every other provider quotes per million,
        // and so does the UI.
        assert_eq!(per_million(Some(&json!("0.000003"))), 3.0);
        assert_eq!(per_million(Some(&json!("0"))), 0.0);

        // Absent or unparseable means "say nothing", not "free".
        assert!(per_million(None) < 0.0);
        assert!(per_million(Some(&json!("not a number"))) < 0.0);
        assert!(per_million(Some(&json!(0.000003))) < 0.0, "always quoted");
    }

    #[test]
    fn a_recognised_model_keeps_its_catalog_blurb_and_prices() {
        let mut models = vec![
            ProviderModel {
                id: "gpt-4o".into(),
                name: "gpt-4o".into(),
                provider: String::new(),
                context_tokens: 0,
                blurb: String::new(),
                input_price: -1.0,
                output_price: -1.0,
                curated: false,
            },
            ProviderModel {
                id: "gpt-9-imaginary".into(),
                name: "gpt-9-imaginary".into(),
                provider: String::new(),
                context_tokens: 0,
                blurb: String::new(),
                input_price: -1.0,
                output_price: -1.0,
                curated: false,
            },
        ];
        enrich(Provider::OpenAi, &mut models);

        assert!(models[0].curated);
        assert!(!models[0].blurb.is_empty());
        assert!(models[0].input_price > 0.0);
        assert_eq!(models[0].context_tokens, 128_000, "filled in from the catalog");

        // Anything we do not know is still offered, just without a price.
        assert!(!models[1].curated);
        assert!(models[1].input_price < 0.0);
    }

    #[test]
    fn curated_falls_back_per_provider() {
        let anthropic = curated(Provider::Anthropic);
        assert!(!anthropic.is_empty());
        assert!(anthropic.iter().all(|m| m.provider == "anthropic" && m.curated));
    }

    #[test]
    fn local_is_not_a_provider_you_can_ask() {
        assert!(list_models(Provider::Local).is_err());
    }
}
