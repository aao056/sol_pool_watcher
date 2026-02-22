use crate::constants::DEFAULT_RPC_TIMEOUT_SECS;
use reqwest::Client as HttpClient;
use serde_json::json;
use tokio::time::Duration;
use tracing::warn;

#[derive(Clone, Debug)]
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
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
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let payload = json!({
            "chat_id": self.chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });

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
