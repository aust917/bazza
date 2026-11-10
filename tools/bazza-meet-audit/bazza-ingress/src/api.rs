use std::{collections::HashMap, time::Duration};

use reqwest::{header, Client, RequestBuilder, Response, Url};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;

use crate::{config::Validated, Error, SAMPLE_RATE};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 8;

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Api {
    client: Client,
    base: Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiError {
    Unauthorized,
    Rejected,
    Unavailable,
    InvalidResponse,
}

#[derive(Deserialize)]
pub(crate) struct Bot {
    pub id: String,
    pub state: String,
    deduplication_key: Option<String>,
}

impl Bot {
    fn valid_id(&self) -> bool {
        self.id.len() <= 128
            && self.id.strip_prefix("bot_").is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    }

    pub fn has_left(&self) -> bool {
        matches!(
            self.state.as_str(),
            "post_processing" | "ended" | "fatal_error" | "data_deleted"
        )
    }
}

#[derive(Deserialize)]
struct Page<T> {
    next: Option<String>,
    results: Vec<T>,
}

#[derive(Deserialize)]
struct Participant {
    uuid: String,
    name: Option<String>,
}

impl Api {
    pub fn new(config: &Validated) -> Result<Self, Error> {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, config.authorization.clone());
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .default_headers(headers)
            .build()
            .map_err(|_| Error::RuntimeUnavailable)?;
        Ok(Self {
            client,
            base: config.api.clone(),
        })
    }

    fn endpoint(&self, parts: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("validated hierarchical Attendee API URL")
            .pop_if_empty()
            .extend(parts.iter().copied());
        url
    }

    pub async fn create(
        &self,
        meeting_url: &str,
        callback: &Url,
        deduplication_key: &str,
    ) -> Result<Bot, ApiError> {
        let request = self.client.post(self.endpoint(&["bots"])).json(&json!({
            "meeting_url": meeting_url,
            "bot_name": "Bazza Recorder",
            "deduplication_key": deduplication_key,
            "recording_settings": {"format": "none", "record_async_transcription_audio_chunks": false},
            "transcription_settings": {"meeting_closed_captions": {}},
            "automatic_leave_settings": {"waiting_room_timeout_seconds": 180, "max_uptime_seconds": 28800},
            "websocket_settings": {"per_participant_audio": {"url": callback.as_str(), "sample_rate": SAMPLE_RATE}},
        }));
        let bot: Bot = decode(request).await?;
        let expected = bot
            .deduplication_key
            .as_deref()
            .is_none_or(|key| key == deduplication_key);
        (bot.valid_id() && expected)
            .then_some(bot)
            .ok_or(ApiError::InvalidResponse)
    }

    /// Recover the ID if Attendee accepted the create request but the response was lost.
    pub async fn find_created(&self, deduplication_key: &str) -> Result<Option<Bot>, ApiError> {
        let mut url = self.endpoint(&["bots"]);
        url.query_pairs_mut()
            .append_pair("deduplication_key", deduplication_key);
        let page: Page<Bot> = decode(self.client.get(url)).await?;
        Ok(page.results.into_iter().find(|bot| {
            bot.valid_id() && bot.deduplication_key.as_deref() == Some(deduplication_key)
        }))
    }

    pub async fn get(&self, id: &str) -> Result<Bot, ApiError> {
        let bot: Bot = decode(self.client.get(self.endpoint(&["bots", id]))).await?;
        (bot.id == id)
            .then_some(bot)
            .ok_or(ApiError::InvalidResponse)
    }

    pub async fn leave(&self, id: &str) -> Result<(), ApiError> {
        response(
            self.client
                .post(self.endpoint(&["bots", id, "leave"]))
                .json(&json!({})),
        )
        .await?;
        Ok(())
    }

    pub async fn participants(&self, id: &str) -> Result<HashMap<String, String>, ApiError> {
        let endpoint = self.endpoint(&["bots", id, "participants"]);
        let mut url = endpoint.clone();
        let mut names = HashMap::new();
        for _ in 0..MAX_PAGES {
            let page: Page<Participant> = decode(self.client.get(url)).await?;
            for participant in page.results {
                let Some(name) = participant.name else {
                    continue;
                };
                let name = name.trim();
                if !participant.uuid.is_empty() && participant.uuid.len() <= 512 && !name.is_empty()
                {
                    names.insert(participant.uuid, name.chars().take(256).collect());
                }
            }
            let Some(next) = page.next else {
                return Ok(names);
            };
            // The server supplies a full URL. Keep credentials confined to the configured origin.
            let parsed = endpoint
                .join(&next)
                .map_err(|_| ApiError::InvalidResponse)?;
            let cursor = parsed
                .query_pairs()
                .find_map(|(name, value)| (name == "cursor").then_some(value))
                .ok_or(ApiError::InvalidResponse)?;
            url = endpoint.clone();
            url.query_pairs_mut().append_pair("cursor", &cursor);
        }
        Ok(names)
    }
}

async fn response(request: RequestBuilder) -> Result<Response, ApiError> {
    let response = request.send().await.map_err(|_| ApiError::Unavailable)?;
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else if status == 401 || status == 403 {
        Err(ApiError::Unauthorized)
    } else if status.is_client_error() {
        Err(ApiError::Rejected)
    } else {
        Err(ApiError::Unavailable)
    }
}

async fn decode<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, ApiError> {
    let mut response = response(request).await?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ApiError::Unavailable)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ApiError::InvalidResponse)
}
