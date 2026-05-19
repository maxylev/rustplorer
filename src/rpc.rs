use serde_json::Value;

pub async fn execute_rpc(
    client: &reqwest::Client,
    urls: &[String],
    payload: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    if urls.is_empty() {
        return Err("No RPC endpoints provided".into());
    }

    let mut last_error: Option<String> = None;

    for (index, url) in urls.iter().enumerate() {
        match client.post(url).json(payload).send().await {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<Value>().await {
                        Ok(json_res) => {
                            if json_res.get("error").is_none() {
                                return Ok(json_res);
                            } else {
                                let err_msg = json_res["error"]["message"]
                                    .as_str()
                                    .unwrap_or("Unknown node error");
                                last_error = Some(format!(
                                    "RPC endpoint [{}] (index {}) returned error: {}",
                                    url, index, err_msg
                                ));
                            }
                        }
                        Err(e) => {
                            last_error =
                                Some(format!("Failed to parse JSON from [{}]: {}", url, e));
                        }
                    }
                } else {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    last_error = Some(format!(
                        "RPC endpoint [{}] returned HTTP {}: {}",
                        url,
                        status,
                        body.chars().take(200).collect::<String>()
                    ));
                }
            }
            Err(e) => {
                last_error = Some(format!("Connection failed to [{}]: {}", url, e));
            }
        }

        if let Some(ref err) = last_error {
            eprintln!("[rustplorer] {} -> trying next endpoint...", err);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    }

    Err(format!(
        "All {} RPC endpoints failed. Last error: {}",
        urls.len(),
        last_error.unwrap_or_default()
    )
    .into())
}
