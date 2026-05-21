use serde_json::Value;

/// Execute a JSON-RPC call against a list of RPC endpoints with exponential
/// backoff retry across endpoints.
///
/// Tries each URL in order. On 429 (rate limit) or 5xx responses, it moves
/// to the next URL. On JSON-RPC error responses, it also tries the next URL.
/// After exhausting all URLs in a round, it waits with exponential backoff
/// before retrying. After `max_rounds` rounds, returns an error.
pub async fn execute_rpc(
    client: &reqwest::Client,
    urls: &[String],
    payload: &Value,
) -> Result<Value, anyhow::Error> {
    if urls.is_empty() {
        anyhow::bail!("No RPC endpoints provided");
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
                                    tracing::warn!(
                                        endpoint = url,
                                        code = err_code,
                                        message = err_msg,
                                        "RPC returned JSON-RPC error"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    endpoint = url,
                                    error = %e,
                                    "Failed to parse JSON from RPC response"
                                );
                            }
                        }
                    } else {
                        let status = res.status();
                        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            tracing::warn!(endpoint = url, "RPC rate limited (429), trying next");
                        } else {
                            let body = res.text().await.unwrap_or_default();
                            tracing::warn!(
                                endpoint = url,
                                status = status.as_u16(),
                                body = body.chars().take(200).collect::<String>(),
                                "RPC returned non-success HTTP status"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        endpoint = url,
                        error = %e,
                        "Connection failed to RPC endpoint"
                    );
                }
            }
        }

        if round < max_rounds - 1 {
            let backoff_ms = 500u64 * 2u64.pow(round as u32).min(10_000);
            tracing::warn!(
                round = round + 1,
                max_rounds = max_rounds,
                backoff_ms = backoff_ms,
                "All endpoints failed, retrying with backoff"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
        }
    }

    anyhow::bail!(
        "All {} RPC endpoints exhausted after {} rounds",
        urls.len(),
        max_rounds,
    )
}
