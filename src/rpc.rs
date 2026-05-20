use serde_json::Value;

pub async fn execute_rpc(
    client: &reqwest::Client,
    urls: &[String],
    payload: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    if urls.is_empty() {
        return Err("No RPC endpoints provided".into());
    }

    let max_rounds = 3;

    for round in 0..max_rounds {
        for url in urls {
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
                                        "[rustplorer] RPC endpoint [{}] returned error ({}): {}",
                                        url, err_code, err_msg
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "[rustplorer] Failed to parse JSON from [{}]: {}",
                                    url, e
                                );
                            }
                        }
                    } else {
                        let status = res.status();
                        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            eprintln!(
                                "[rustplorer] RPC endpoint [{}] rate limited (429), trying next...",
                                url
                            );
                        } else {
                            let body = res.text().await.unwrap_or_default();
                            eprintln!(
                                "[rustplorer] RPC endpoint [{}] returned HTTP {}: {}",
                                url,
                                status,
                                body.chars().take(200).collect::<String>()
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[rustplorer] Connection failed to [{}]: {}", url, e);
                }
            }
        }

        if round < max_rounds - 1 {
            let backoff_ms = 500u64 * 2u64.pow(round as u32).min(10_000);
            eprintln!(
                "[rustplorer] All endpoints failed in round {}/{}, retrying in {}ms...",
                round + 1,
                max_rounds,
                backoff_ms
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
        }
    }

    Err(format!(
        "All {} RPC endpoints exhausted after {} rounds",
        urls.len(),
        max_rounds,
    )
    .into())
}
