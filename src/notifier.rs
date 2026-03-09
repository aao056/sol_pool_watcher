use crate::constants::DEFAULT_RPC_TIMEOUT_SECS;
use reqwest::Client as HttpClient;
use serde_json::json;
use tokio::time::Duration;
use tracing::warn;

#[derive(Clone, Debug)]
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
    default_thread_id: Option<i64>,
    error_thread_id: Option<i64>,
    sim_thread_id: Option<i64>,
    client: HttpClient,
}

impl TelegramNotifier {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("TG_BOT_API_TOKEN").ok();
        let chat_id = std::env::var("TG_CHAT_ID").ok();

        match (token, chat_id) {
            (Some(bot_token), Some(chat_id))
                if !bot_token.trim().is_empty() && !chat_id.trim().is_empty() =>
            {
                let client = match HttpClient::builder()
                    .timeout(Duration::from_secs(DEFAULT_RPC_TIMEOUT_SECS))
                    .build()
                {
                    Ok(c) => c,
                    Err(err) => {
                        warn!(
                            ?err,
                            "failed to initialize telegram HTTP client; notifications disabled"
                        );
                        return None;
                    }
                };
                Some(Self {
                    bot_token,
                    chat_id,
                    default_thread_id: parse_i64_env("TG_MESSAGE_THREAD_ID"),
                    error_thread_id: parse_i64_env("TG_ERROR_MESSAGE_THREAD_ID"),
                    sim_thread_id: parse_i64_env("TG_SIM_THREAD_ID"),
                    client,
                })
            }
            (None, None) => None,
            _ => {
                warn!(
                    "incomplete telegram configuration: set both TG_BOT_API_TOKEN and TG_CHAT_ID to enable notifications",
                );
                None
            }
        }
    }

    pub async fn send_event(&self, text: &str) {
        self.send_with_thread(text, self.default_thread_id).await;
    }

    pub async fn send_error(&self, text: &str) {
        self.send_with_thread(text, self.error_thread_id.or(self.default_thread_id))
            .await;
    }

    pub async fn send_sim(&self, text: &str) {
        self.send_with_thread(text, self.sim_thread_id.or(self.default_thread_id))
            .await;
    }

    async fn send_with_thread(&self, text: &str, thread_id: Option<i64>) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let mut payload = json!({
            "chat_id": self.chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(thread_id) = thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }

        match self.client.post(url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                warn!(status = %resp.status(), "telegram notification failed");
            }
            Err(err) => {
                warn!(?err, "telegram notification request error");
            }
        }
    }
}

fn parse_i64_env(name: &str) -> Option<i64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
}
