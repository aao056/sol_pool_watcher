use crate::domain::{HolderShare, RugCheckReport};
use reqwest::Client as HttpClient;
use std::time::Duration;
use tokio::time::sleep;
use tracing::debug;

pub async fn fetch_rugcheck_report(
    http_client: &HttpClient,
    token_mint: &str,
) -> Option<RugCheckReport> {
    const MAX_ATTEMPTS: usize = 3;
    let url = format!("https://api.rugcheck.xyz/v1/tokens/{token_mint}/report");

    for attempt in 1..=MAX_ATTEMPTS {
        let response = match http_client.get(&url).send().await {
            Ok(resp) => resp,
            Err(err) => {
                debug!(
                    token = %token_mint,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    error = %err,
                    "rugcheck request failed"
                );
                if attempt < MAX_ATTEMPTS {
                    sleep(Duration::from_millis(250 * attempt as u64)).await;
                    continue;
                }
                return None;
            }
        };

        if !response.status().is_success() {
            debug!(
                token = %token_mint,
                attempt,
                max_attempts = MAX_ATTEMPTS,
                status = %response.status(),
                "rugcheck returned non-success status"
            );
            if attempt < MAX_ATTEMPTS {
                sleep(Duration::from_millis(250 * attempt as u64)).await;
                continue;
            }
            return None;
        }

        match response.json::<RugCheckReport>().await {
            Ok(report) => return Some(report),
            Err(err) => {
                debug!(
                    token = %token_mint,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    error = %err,
                    "rugcheck response decode failed"
                );
                if attempt < MAX_ATTEMPTS {
                    sleep(Duration::from_millis(250 * attempt as u64)).await;
                    continue;
                }
                return None;
            }
        }
    }

    None
}

pub fn holders_from_rugcheck(report: &RugCheckReport, top_n: usize) -> Vec<HolderShare> {
    report
        .top_holders
        .iter()
        .filter_map(|h| {
            let pct = h.pct?;
            let address = h
                .owner
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| h.address.as_deref().filter(|s| !s.trim().is_empty()))?
                .to_string();
            Some(HolderShare { address, pct })
        })
        .take(top_n)
        .collect()
}

pub fn rug_risk_classification(report: &RugCheckReport) -> &'static str {
    if report.rugged.unwrap_or(false) {
        return "Likely Rug";
    }
    let danger_count = report
        .risks
        .iter()
        .filter(|r| r.level.as_deref() == Some("danger"))
        .count();
    let score = report.score_normalised.unwrap_or(0);
    if score >= 70 || danger_count >= 3 {
        "High Risk"
    } else if score >= 40 || danger_count >= 1 {
        "Medium Risk"
    } else {
        "Low Risk"
    }
}

pub fn rug_risk_lines(report: &RugCheckReport, max_items: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for risk in report.risks.iter().take(max_items) {
        let mut line = format!("- {}", risk.name);
        if let Some(v) = risk
            .value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            line.push(' ');
            line.push_str(v);
        }
        lines.push(line);
    }

    let mutable_in_risks = report
        .risks
        .iter()
        .any(|r| r.name.to_ascii_lowercase().contains("mutable"));
    if report.token_meta.mutable.unwrap_or(false) && !mutable_in_risks {
        lines.push("- Mutable metadata".to_string());
    }

    lines
}
