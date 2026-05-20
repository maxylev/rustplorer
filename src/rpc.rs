use serde_json::Value;

pub async fn execute_rpc(
    client: &reqwest::Client,
    urls: &[String],
    payload: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    if urls.is_empty() {
        return Err("No RPC endpoints provided".into());
    }

    let max_total_attempts = urls.len() * 3;

    for attempt in 0..max_total_attempts {
        let url = &urls[attempt % urls.len()];

        match client.post(url).json(payload).send().await {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<Value>().await {
                        Ok(json_res) => {
                            if json_res.get("error").is_none_or(|e| e.is_null()) {
                                return Ok(json_res);
                            } else {
                                let err_code = json_res["error"]["code"].as_i64().unwrap_or(0);
                                let err_msg = json_res["error"]["message"]
                                    .as_str()
                                    .unwrap_or("Unknown node error");
                                eprintln!(
                                    "[rustplorer] RPC endpoint [{}] returned error ({}): {} -> retrying...",
                                    url, err_code, err_msg
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("[rustplorer] Failed to parse JSON from [{}]: {}", url, e);
                        }
                    }
                } else {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    eprintln!(
                        "[rustplorer] RPC endpoint [{}] returned HTTP {}: {} -> retrying...",
                        url,
                        status,
                        body.chars().take(200).collect::<String>()
                    );
                }
            }
            Err(e) => {
                eprintln!("[rustplorer] Connection failed to [{}]: {}", url, e);
            }
        }

        let backoff_ms = 150u64 * 2u64.pow(attempt as u32).min(10_000);
        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
    }

    Err(format!(
        "All {} RPC endpoints exhausted after {} total attempts",
        urls.len(),
        max_total_attempts,
    )
    .into())
}
