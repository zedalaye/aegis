//! Live model lists, plus the URL each authentication kind talks to.
//!
//! Settings asks this instead of making the user type a model id. A failed
//! fetch is not a command failure: the form still has a fallback list and a
//! sentence saying why the live one did not arrive.

use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

use crate::oauth::{self, Resolved};
use crate::secrets::ApiKey;
use crate::store::AuthKind;

/// What [`list`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ModelCatalog {
    /// Model ids the picker can offer, live ones first when the server answered.
    pub models: Vec<String>,
    /// Whether `models` came from the provider just now.
    pub live: bool,
    /// Empty on a live list. Otherwise why the fallback was used.
    pub message: String,
}

/// Models used when the provider cannot be asked.
pub fn fallback(kind: AuthKind) -> &'static [&'static str] {
    match kind {
        AuthKind::ApiKey => &["gpt-4o", "gpt-4o-mini", "o4-mini"],
        AuthKind::ClaudeCli => motosan_ai::ANTHROPIC_MODELS,
        AuthKind::CodexCli => &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
        ],
        AuthKind::GrokCli => &["grok-4", "grok-4.5", "grok-3", "grok-3-mini", "grok-2"],
    }
}

/// The URL this kind uses when the field is empty.
pub fn effective_base_url(kind: AuthKind, override_url: &str) -> String {
    let trimmed = override_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        kind.default_base_url().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Anthropic origin: motosan appends `/v1/messages`, so a pasted `/v1` is stripped.
pub fn anthropic_origin(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_owned()
}

/// Codex Responses endpoint. The form stores the `/codex` base; motosan posts
/// to `/responses`.
pub fn codex_responses_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.ends_with("/responses") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/responses")
    }
}

/// Where `GET` lists models for this kind.
pub fn models_url(kind: AuthKind, override_url: &str) -> Option<String> {
    let base = effective_base_url(kind, override_url);
    if base.is_empty() {
        return None;
    }
    Some(match kind {
        AuthKind::ApiKey | AuthKind::GrokCli => format!("{base}/models"),
        AuthKind::ClaudeCli => format!("{}/v1/models", anthropic_origin(&base)),
        AuthKind::CodexCli => {
            let base = base
                .trim_end_matches('/')
                .trim_end_matches("/responses")
                .trim_end_matches('/');
            format!("{base}/models")
        }
    })
}

/// Asks the configured provider for the models this login can use.
pub async fn list(
    kind: AuthKind,
    override_url: &str,
    client: Option<&Client>,
    api_key: Option<&ApiKey>,
) -> ModelCatalog {
    let fallback_ids: Vec<String> = fallback(kind).iter().map(|id| (*id).to_owned()).collect();
    let catalog = |models: Vec<String>, live: bool, message: String| ModelCatalog {
        models,
        live,
        message,
    };

    let Some(url) = models_url(kind, override_url) else {
        return catalog(
            fallback_ids,
            false,
            "No base URL is set, so there is no models endpoint to ask.".to_owned(),
        );
    };

    let Some(http) = client else {
        return catalog(
            fallback_ids,
            false,
            "Aegis has no HTTP client on this machine.".to_owned(),
        );
    };

    let request = match authorized_get(http, kind, &url, override_url, api_key).await {
        Ok(request) => request,
        Err(message) => return catalog(fallback_ids, false, message),
    };

    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            return catalog(
                fallback_ids,
                false,
                format!("The models list could not be reached ({err})."),
            );
        }
    };

    let status = response.status();
    let body = match response.text().await {
        Ok(body) => body,
        Err(err) => {
            return catalog(
                fallback_ids,
                false,
                format!("The models list could not be read ({err})."),
            );
        }
    };

    if !status.is_success() {
        return catalog(
            fallback_ids,
            false,
            format!("The models endpoint answered {status}."),
        );
    }

    let value: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => {
            return catalog(
                fallback_ids,
                false,
                "The models endpoint did not return JSON.".to_owned(),
            );
        }
    };

    let mut models = parse_ids(&value);
    if models.is_empty() {
        return catalog(
            fallback_ids,
            false,
            "The models endpoint answered, but named no models.".to_owned(),
        );
    }

    models.sort();
    models.dedup();
    catalog(models, true, String::new())
}

async fn authorized_get(
    http: &Client,
    kind: AuthKind,
    url: &str,
    override_url: &str,
    api_key: Option<&ApiKey>,
) -> Result<reqwest::RequestBuilder, String> {
    let mut request = http.get(url);

    match kind {
        AuthKind::ApiKey => {
            let key = api_key.ok_or_else(|| {
                "No API key. Save one in Settings, or start Aegis with AEGIS_API_KEY set."
                    .to_owned()
            })?;
            request = bearer(request, key.expose())?;
        }
        AuthKind::ClaudeCli => {
            let Resolved::Anthropic { access_token } =
                oauth::resolve(kind, Some(http), override_url)
                    .await
                    .map_err(|missing| missing.message)?
            else {
                return Err("The Claude Code login could not be used.".to_owned());
            };
            request = bearer(request, access_token.expose())?
                .header("anthropic-version", "2023-06-01")
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14",
                )
                .header("x-app", "cli");
        }
        AuthKind::CodexCli => {
            let Resolved::Codex {
                access_token,
                account_id,
            } = oauth::resolve(kind, Some(http), override_url)
                .await
                .map_err(|missing| missing.message)?
            else {
                return Err("The Codex login could not be used.".to_owned());
            };
            request = bearer(request, access_token.expose())?
                .header("chatgpt-account-id", account_id)
                .header("originator", "codex_cli_rs")
                .header("openai-beta", "responses=experimental");
        }
        AuthKind::GrokCli => {
            let Resolved::Grok { access_token, .. } =
                oauth::resolve(kind, Some(http), override_url)
                    .await
                    .map_err(|missing| missing.message)?
            else {
                return Err("The Grok CLI login could not be used.".to_owned());
            };
            request = bearer(request, access_token.expose())?;
            for (name, value) in oauth::grok_headers() {
                request = request.header(name, value);
            }
        }
    }

    Ok(request)
}

fn bearer(
    request: reqwest::RequestBuilder,
    token: &str,
) -> Result<reqwest::RequestBuilder, String> {
    let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "The access token cannot be sent in a header.".to_owned())?;
    header.set_sensitive(true);
    Ok(request.header(AUTHORIZATION, header))
}

/// Pulls model ids out of the shapes the four endpoints actually send.
fn parse_ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    collect_ids(value, &mut ids);
    ids
}

fn collect_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_ids(item, ids);
            }
        }
        Value::String(id) => push_id(ids, id),
        Value::Object(map) => {
            for key in ["id", "slug", "model", "name"] {
                if let Some(Value::String(id)) = map.get(key) {
                    push_id(ids, id);
                    break;
                }
            }
            for key in ["data", "models", "items"] {
                if let Some(nested) = map.get(key) {
                    collect_ids(nested, ids);
                }
            }
        }
        _ => {}
    }
}

fn push_id(ids: &mut Vec<String>, id: &str) {
    let id = id.trim();
    if !id.is_empty() && !ids.iter().any(|found| found == id) {
        ids.push(id.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_override_uses_the_kind_default() {
        assert_eq!(
            effective_base_url(AuthKind::ClaudeCli, "  "),
            "https://api.anthropic.com"
        );
        assert_eq!(
            effective_base_url(AuthKind::GrokCli, ""),
            AuthKind::GrokCli.default_base_url()
        );
    }

    #[test]
    fn a_pasted_anthropic_v1_is_stripped_for_the_origin() {
        assert_eq!(
            anthropic_origin("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            anthropic_origin("https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn codex_gains_responses_unless_it_already_has_it() {
        assert_eq!(
            codex_responses_url("https://chatgpt.com/backend-api/codex"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            codex_responses_url("https://chatgpt.com/backend-api/codex/responses"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn models_urls_match_each_kind() {
        assert_eq!(
            models_url(AuthKind::ApiKey, "https://api.openai.com/v1").as_deref(),
            Some("https://api.openai.com/v1/models")
        );
        assert_eq!(
            models_url(AuthKind::ClaudeCli, "").as_deref(),
            Some("https://api.anthropic.com/v1/models")
        );
        assert_eq!(
            models_url(AuthKind::CodexCli, "").as_deref(),
            Some("https://chatgpt.com/backend-api/codex/models")
        );
        assert_eq!(
            models_url(AuthKind::GrokCli, "").as_deref(),
            Some("https://cli-chat-proxy.grok.com/v1/models")
        );
    }

    #[test]
    fn openai_and_anthropic_lists_are_read() {
        let openai = serde_json::json!({
            "data": [{ "id": "gpt-4o" }, { "id": "gpt-4o-mini" }]
        });
        assert_eq!(parse_ids(&openai), vec!["gpt-4o", "gpt-4o-mini"]);

        let anthropic = serde_json::json!({
            "data": [{ "id": "claude-sonnet-4-6", "display_name": "Sonnet" }]
        });
        assert_eq!(parse_ids(&anthropic), vec!["claude-sonnet-4-6"]);
    }

    #[test]
    fn a_codex_slug_list_is_read() {
        let body = serde_json::json!({
            "models": [{ "slug": "gpt-5.5" }, { "slug": "gpt-5.4-mini" }]
        });
        assert_eq!(parse_ids(&body), vec!["gpt-5.5", "gpt-5.4-mini"]);
    }
}
