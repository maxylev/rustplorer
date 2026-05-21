use anyhow::Result;
use clap::Parser;
use rustplorer::{AppConfig, DepositResult, Format, load_addresses, load_config, run_indexer};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use toml_edit::{DocumentMut, Item, Table};

// ---------------------------------------------------------------------------
// JSON:API Response Wrappers
// ---------------------------------------------------------------------------

/// Top-level success response with `data` and `meta` keys.
/// Follows JSON:API conventions: every response is a JSON object
/// (never a bare array), allowing future extensibility without breaking changes.
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self { data, meta: None }
    }

    pub fn with_meta(data: T, meta: serde_json::Value) -> Self {
        Self {
            data,
            meta: Some(meta),
        }
    }
}

/// Error object following JSON:API error format.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub status: &'static str,
    pub title: &'static str,
    pub detail: String,
}

/// Top-level error response with `errors` array.
#[derive(Debug, Serialize)]
pub struct ApiErrors {
    pub errors: Vec<ApiError>,
}

impl ApiErrors {
    pub fn bad_request(detail: impl Into<String>) -> (axum::http::StatusCode, axum::Json<Self>) {
        (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(Self {
                errors: vec![ApiError {
                    status: "400",
                    title: "Bad Request",
                    detail: detail.into(),
                }],
            }),
        )
    }

    pub fn not_found(detail: impl Into<String>) -> (axum::http::StatusCode, axum::Json<Self>) {
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(Self {
                errors: vec![ApiError {
                    status: "404",
                    title: "Not Found",
                    detail: detail.into(),
                }],
            }),
        )
    }

    pub fn conflict(detail: impl Into<String>) -> (axum::http::StatusCode, axum::Json<Self>) {
        (
            axum::http::StatusCode::CONFLICT,
            axum::Json(Self {
                errors: vec![ApiError {
                    status: "409",
                    title: "Conflict",
                    detail: detail.into(),
                }],
            }),
        )
    }
}

/// Helper: create a JSON meta object with a single `total` key.
fn meta_total(total: usize) -> serde_json::Value {
    serde_json::json!({ "total": total })
}

// ---------------------------------------------------------------------------
// CLI Arguments
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "rustplorer",
    version,
    about = "High-performance multi-chain deposit detector using only public RPC endpoints"
)]
struct CliArgs {
    #[arg(short, long, default_value = "Config.toml")]
    config: PathBuf,

    #[arg(short, long, help = "Text file with target addresses (one per line)")]
    addresses: PathBuf,

    #[arg(short, long, value_enum, default_value_t = Format::Json, help = "Output format")]
    format: Format,

    #[arg(short, long, help = "Save output to file (stdout if omitted)")]
    output: Option<PathBuf>,

    #[arg(long, help = "Override network CAIP-2 to scan (e.g. eip155:1)")]
    network: Option<String>,

    #[arg(long, help = "Override start block (defaults to latest if omitted)")]
    start_block: Option<u64>,

    #[arg(long, help = "Override end block (defaults to latest if omitted)")]
    end_block: Option<u64>,

    #[arg(long, help = "Override RPC endpoints (comma-separated)")]
    rpc: Option<String>,

    #[arg(long, default_value_t = false, help = "Show verbose progress output")]
    verbose: bool,

    #[arg(long, help = "Run continuously in daemon mode")]
    watch: bool,

    #[arg(
        long,
        default_value_t = 60,
        help = "Polling interval in seconds (watch mode)"
    )]
    interval: u64,

    #[arg(long, help = "Start HTTP API on port for dynamic address management")]
    api_port: Option<u16>,

    #[arg(long, help = "Bind the API to this host (default: 127.0.0.1)")]
    host: Option<String>,

    #[arg(long, help = "Add address(es) to file and exit (repeatable)")]
    add_address: Option<Vec<String>>,

    #[arg(long, help = "Remove address(es) from file and exit (repeatable)")]
    remove_address: Option<Vec<String>>,

    #[arg(
        long,
        help = "Add a chain to Config.toml (Format: NAME,CAIP2,RPC_URL1,RPC_URL2)"
    )]
    add_chain: Option<String>,

    #[arg(long, help = "Remove a chain from Config.toml by name")]
    remove_chain: Option<String>,

    #[arg(
        long,
        help = "Add an asset to Config.toml (Format: CHAIN_NAME,ASSET_NAME,CONTRACT,DECIMALS)"
    )]
    add_asset: Option<String>,

    #[arg(
        long,
        help = "Remove an asset from Config.toml (Format: CHAIN_NAME,ASSET_NAME)"
    )]
    remove_asset: Option<String>,
}

// ---------------------------------------------------------------------------
// API State — holds an in-memory ring buffer for instant deposit serving
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ApiState {
    file_path: PathBuf,
    config_path: PathBuf,
    /// O(1) in-memory ring buffer — the `/deposits` endpoint reads from here
    /// instead of re-parsing the entire JSONL file from disk on every request.
    recent_deposits: Arc<RwLock<VecDeque<DepositResult>>>,
}

// ---------------------------------------------------------------------------
// API Payload Types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
struct AddressPayload {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    addresses: Option<Vec<String>>,
}

impl AddressPayload {
    fn into_addrs(self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(a) = self.address {
            out.push(a);
        }
        if let Some(list) = self.addresses {
            out.extend(list);
        }
        out
    }
}

#[derive(Deserialize)]
struct AddChainPayload {
    name: String,
    caip2: String,
    rpc: Vec<String>,
    #[serde(default)]
    start_block: Option<u64>,
    #[serde(default)]
    end_block: Option<u64>,
}

#[derive(Deserialize)]
struct AddAssetPayload {
    chain: String,
    name: String,
    contract: String,
    decimals: u32,
}

// ---------------------------------------------------------------------------
// Entry Point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with env-filter support (RUST_LOG=info, etc.)
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = CliArgs::parse();

    // CLI: Address management
    if let Some(ref addrs) = args.add_address {
        manage_addresses(&args.addresses, addrs, true)?;
        tracing::info!("Added {} address(es) to {:?}", addrs.len(), args.addresses);
        return Ok(());
    }
    if let Some(ref addrs) = args.remove_address {
        manage_addresses(&args.addresses, addrs, false)?;
        tracing::info!(
            "Removed {} address(es) from {:?}",
            addrs.len(),
            args.addresses
        );
        return Ok(());
    }

    // CLI: Chain management (using toml_edit for comment-preserving edits)
    if let Some(chain_str) = args.add_chain {
        manage_chains_cli(&args.config, &chain_str, true)?;
        return Ok(());
    }
    if let Some(name) = args.remove_chain {
        manage_chains_cli(&args.config, &name, false)?;
        return Ok(());
    }

    // CLI: Asset management
    if let Some(asset_str) = args.add_asset {
        manage_assets_cli(&args.config, &asset_str, true)?;
        return Ok(());
    }
    if let Some(name) = args.remove_asset {
        manage_assets_cli(&args.config, &name, false)?;
        return Ok(());
    }

    if args.watch || args.api_port.is_some() {
        run_watch_mode(args).await
    } else {
        run_single(args).await
    }
}

// ---------------------------------------------------------------------------
// Watch Mode (Daemon)
// ---------------------------------------------------------------------------

async fn run_watch_mode(args: CliArgs) -> Result<()> {
    tracing::info!(interval = args.interval, "Starting daemon mode");

    // In-memory ring buffer for the API — holds the last 100 deposits for
    // instant serving, eliminating the catastrophic disk I/O from reading
    // the entire JSONL file on every HTTP poll.
    let recent_deposits = Arc::new(RwLock::new(VecDeque::with_capacity(100)));

    if let Some(port) = args.api_port {
        let state = ApiState {
            file_path: args.addresses.clone(),
            config_path: args.config.clone(),
            recent_deposits: Arc::clone(&recent_deposits),
        };

        tokio::spawn(async move {
            let app = axum::Router::new()
                .route(
                    "/",
                    axum::routing::get(|| async {
                        axum::response::Html(include_str!("../index.html"))
                    }),
                )
                .route("/v1/addresses", axum::routing::get(api_list_addresses))
                .route("/v1/addresses", axum::routing::post(api_add_address))
                .route(
                    "/v1/addresses/{addr}",
                    axum::routing::delete(api_remove_address),
                )
                .route("/v1/deposits", axum::routing::get(api_list_deposits))
                .route("/v1/config", axum::routing::get(api_get_config))
                .route("/v1/chains", axum::routing::post(api_add_chain))
                .route("/v1/chains/{name}", axum::routing::delete(api_remove_chain))
                .route("/v1/assets", axum::routing::post(api_add_asset))
                .route(
                    "/v1/assets/{chain}/{asset}",
                    axum::routing::delete(api_remove_asset),
                )
                .with_state(state);

            // SECURITY: Bind to localhost by default instead of 0.0.0.0.
            let host = "127.0.0.1";
            let bind_addr = format!("{}:{}", host, port);

            match tokio::net::TcpListener::bind(&bind_addr).await {
                Ok(listener) => {
                    tracing::info!("API listening on {}", bind_addr);
                    tracing::info!("Dashboard: http://localhost:{}/", port);
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::error!(error = %e, "API server error");
                    }
                }
                Err(e) => {
                    tracing::error!(addr = %bind_addr, error = %e, "Failed to bind API listener");
                }
            }
        });
    }

    let mut last_scanned: HashMap<String, u64> = HashMap::new();

    loop {
        let mut config = load_config(&args.config)?;
        apply_overrides(&mut config, &args);

        for chain in config.chains.values_mut() {
            if let Some(&last_block) = last_scanned.get(&chain.caip2) {
                chain.start_block = Some(last_block + 1);
                chain.end_block = None;
            }
        }

        if args.verbose {
            tracing::info!(chains = config.chains.len(), "Loaded configuration");
        }

        let targets = load_addresses(&args.addresses)?;

        if args.verbose {
            tracing::info!(count = targets.len(), "Cached target addresses in memory");
        }

        let index_result = tokio::select! {
            result = run_indexer(config.chains, Arc::new(targets)) => result?,
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!("Graceful shutdown initiated. Halting watch mode...");
                return Ok(());
            }
        };

        for (chain_key, end_block) in index_result.latest_blocks {
            last_scanned.insert(chain_key, end_block);
        }

        if !index_result.deposits.is_empty() {
            if let Some(ref path) = args.output {
                append_output(path, &index_result.deposits, args.format)?;
            } else {
                println!("{}", format_results(&index_result.deposits, args.format)?);
            }

            // PERFORMANCE: Update the in-memory ring buffer for instant API responses.
            let mut cache = recent_deposits.write().await;
            for d in &index_result.deposits {
                if cache.len() == 100 {
                    cache.pop_back();
                }
                cache.push_front(d.clone());
            }
        }

        if args.verbose {
            tracing::info!(
                deposits = index_result.deposits.len(),
                interval = args.interval,
                "Cycle complete, sleeping"
            );
        }

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(args.interval)) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!("Graceful shutdown initiated. Halting watch mode...");
                return Ok(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Single Run Mode
// ---------------------------------------------------------------------------

async fn run_single(args: CliArgs) -> Result<()> {
    if args.verbose {
        tracing::info!(path = ?args.config, "Loading config");
    }

    let mut config: AppConfig = load_config(&args.config)?;
    apply_overrides(&mut config, &args);

    if args.verbose {
        tracing::info!(chains = config.chains.len(), "Loaded configuration");
    }

    let targets = load_addresses(&args.addresses)?;
    tracing::info!(count = targets.len(), "Cached target addresses in memory");

    let index_result = run_indexer(config.chains, Arc::new(targets)).await?;

    let output_string = format_results(&index_result.deposits, args.format)?;

    if let Some(path) = args.output {
        let mut file = File::create(&path)?;
        file.write_all(output_string.as_bytes())?;
        tracing::info!(
            count = index_result.deposits.len(),
            path = ?path,
            "Saved deposit records"
        );
    } else {
        println!("{}", output_string);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Config Overrides
// ---------------------------------------------------------------------------

fn apply_overrides(config: &mut AppConfig, args: &CliArgs) {
    for chain in config.chains.values_mut() {
        if let Some(ref target_net) = args.network
            && chain.caip2 != *target_net
        {
            continue;
        }

        if let Some(sb) = args.start_block {
            if args.verbose {
                tracing::info!(chain = %chain.caip2, start_block = sb, "Override");
            }
            chain.start_block = Some(sb);
        }

        if let Some(eb) = args.end_block {
            if args.verbose {
                tracing::info!(chain = %chain.caip2, end_block = eb, "Override");
            }
            chain.end_block = Some(eb);
        }

        if let Some(ref rpc_str) = args.rpc {
            if args.verbose {
                tracing::info!(chain = %chain.caip2, rpc = %rpc_str, "Override");
            }
            chain.rpc = rpc_str.split(',').map(|s| s.trim().to_string()).collect();
        }
    }

    if let Some(target_net) = &args.network {
        config.chains.retain(|_, chain| chain.caip2 == *target_net);
    }
}

// ---------------------------------------------------------------------------
// Output Formatting
// ---------------------------------------------------------------------------

fn format_results(results: &[DepositResult], format: Format) -> Result<String> {
    match format {
        Format::Json => Ok(serde_json::to_string_pretty(results)?),
        Format::Csv => {
            let mut wtr = csv::Writer::from_writer(vec![]);
            for record in results {
                wtr.serialize(record)?;
            }
            Ok(String::from_utf8(wtr.into_inner()?)?)
        }
    }
}

fn append_output(path: &PathBuf, results: &[DepositResult], format: Format) -> Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    match format {
        Format::Json => {
            for res in results {
                let json_line = serde_json::to_string(res)?;
                writeln!(file, "{}", json_line)?;
            }
        }
        Format::Csv => {
            let add_header = file.metadata()?.len() == 0;
            let mut wtr = csv::WriterBuilder::new()
                .has_headers(add_header)
                .from_writer(vec![]);
            for record in results {
                wtr.serialize(record)?;
            }
            file.write_all(&wtr.into_inner()?)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Address File Management
// ---------------------------------------------------------------------------

fn manage_addresses(path: &PathBuf, addresses: &[String], add: bool) -> io::Result<()> {
    let mut contents = String::new();
    if path.exists() {
        let mut file = File::open(path)?;
        file.read_to_string(&mut contents)?;
    }

    let mut lines: Vec<String> = contents
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if add {
        let existing: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();
        for addr in addresses.iter().rev() {
            let lower = addr.to_lowercase();
            if !existing.contains(&lower) {
                lines.push(addr.clone());
            }
        }
    } else {
        let to_remove: Vec<String> = addresses.iter().map(|a| a.to_lowercase()).collect();
        lines.retain(|l| !to_remove.contains(&l.to_lowercase()));
    }

    let mut file = File::create(path)?;
    for line in lines {
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

fn load_addresses_from_api(path: &std::path::Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = File::open(path)?;
    let mut addrs = Vec::new();
    for line in BufReader::new(file).lines() {
        let addr = line?.trim().to_string();
        if !addr.is_empty() {
            addrs.push(addr);
        }
    }
    Ok(addrs)
}

// ---------------------------------------------------------------------------
// TOML Config Management (comment-preserving via toml_edit)
// ---------------------------------------------------------------------------

/// Manage chains via CLI.
///
/// Add format: `NAME,CAIP2,RPC_URL1,RPC_URL2`
/// Remove: by chain name
fn manage_chains_cli(path: &PathBuf, input: &str, add: bool) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc = content.parse::<DocumentMut>()?;

    if add {
        let parts: Vec<&str> = input.split(',').collect();
        if parts.len() < 3 {
            anyhow::bail!("Invalid format. Use: NAME,CAIP2,RPC_URL1,RPC_URL2...");
        }
        let chain_name = parts[0];
        let caip2 = parts[1];

        let mut new_chain = Table::new();
        new_chain.insert("caip2", toml_edit::value(caip2));

        let mut rpc_array = toml_edit::Array::new();
        for url in &parts[2..] {
            rpc_array.push(*url);
        }
        new_chain.insert("rpc", Item::Value(rpc_array.into()));

        if !doc.contains_key("chains") {
            doc.insert("chains", Item::Table(Table::new()));
        }
        doc["chains"]
            .as_table_mut()
            .unwrap()
            .insert(chain_name, Item::Table(new_chain));

        tracing::info!("Added chain '{}' ({}) to {:?}", chain_name, caip2, path);
    } else {
        // Remove chain by name
        if let Some(chains) = doc.get_mut("chains").and_then(|i| i.as_table_mut()) {
            if chains.remove(input).is_some() {
                tracing::info!("Removed chain '{}' from {:?}", input, path);
            } else {
                tracing::warn!("Chain '{}' not found in {:?}", input, path);
            }
        }
    }

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// Manage assets via CLI.
///
/// Add format: `CHAIN_NAME,ASSET_NAME,CONTRACT,DECIMALS`
/// Remove format: `CHAIN_NAME,ASSET_NAME`
fn manage_assets_cli(path: &PathBuf, input: &str, add: bool) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc = content.parse::<DocumentMut>()?;

    if add {
        let parts: Vec<&str> = input.split(',').collect();
        if parts.len() != 4 {
            anyhow::bail!("Invalid format. Use: CHAIN_NAME,ASSET_NAME,CONTRACT,DECIMALS");
        }
        let (chain_name, asset_name, contract, decimals) = (parts[0], parts[1], parts[2], parts[3]);
        let dec_val: i64 = decimals.parse()?;

        // Navigate to [chains.<chain_name>]
        let chains_table = doc
            .get_mut("chains")
            .and_then(|i| i.as_table_mut())
            .ok_or_else(|| anyhow::anyhow!("No [chains] section found in config"))?;

        let chain_table = chains_table
            .get_mut(chain_name)
            .and_then(|i| i.as_table_mut())
            .ok_or_else(|| anyhow::anyhow!("Chain '{}' not found in config", chain_name))?;

        // Ensure [chains.<chain_name>.assets] sub-table exists
        if !chain_table.contains_key("assets") {
            chain_table.insert("assets", Item::Table(Table::new()));
        }

        let assets_table = chain_table["assets"].as_table_mut().ok_or_else(|| {
            anyhow::anyhow!("Failed to create assets table for chain '{}'", chain_name)
        })?;

        let mut asset_table = Table::new();
        asset_table.insert("contract", toml_edit::value(contract));
        asset_table.insert("decimals", toml_edit::value(dec_val));

        assets_table.insert(asset_name, Item::Table(asset_table));
        tracing::info!(
            "Added asset '{}' to chain '{}' in {:?}",
            asset_name,
            chain_name,
            path
        );
    } else {
        // Remove format: CHAIN_NAME,ASSET_NAME
        let parts: Vec<&str> = input.split(',').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid format. Use: CHAIN_NAME,ASSET_NAME");
        }
        let (chain_name, asset_name) = (parts[0], parts[1]);

        if let Some(chains_table) = doc.get_mut("chains").and_then(|i| i.as_table_mut())
            && let Some(chain_table) = chains_table
                .get_mut(chain_name)
                .and_then(|i| i.as_table_mut())
            && let Some(assets_table) = chain_table.get_mut("assets").and_then(|i| i.as_table_mut())
        {
            if assets_table.remove(asset_name).is_some() {
                tracing::info!(
                    "Removed asset '{}' from chain '{}' in {:?}",
                    asset_name,
                    chain_name,
                    path
                );
            } else {
                tracing::warn!(
                    "Asset '{}' not found in chain '{}' in {:?}",
                    asset_name,
                    chain_name,
                    path
                );
            }
        }
    }

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API Handlers
// ---------------------------------------------------------------------------

async fn api_list_addresses(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::Json<ApiResponse<Vec<String>>> {
    let addrs = load_addresses_from_api(&state.file_path).unwrap_or_default();
    let total = addrs.len();
    axum::Json(ApiResponse::with_meta(addrs, meta_total(total)))
}

async fn api_add_address(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::Json(payload): axum::Json<AddressPayload>,
) -> (
    axum::http::StatusCode,
    axum::Json<ApiResponse<serde_json::Value>>,
) {
    let addrs = payload.into_addrs();
    if addrs.is_empty() {
        let (status, body) = ApiErrors::bad_request("no addresses provided");
        return (
            status,
            axum::Json(ApiResponse::new(
                serde_json::json!({"errors": body.0.errors}),
            )),
        );
    }
    let count = addrs.len();
    let _ = manage_addresses(&state.file_path, &addrs, true);
    let all = load_addresses_from_api(&state.file_path).unwrap_or_default();
    (
        axum::http::StatusCode::CREATED,
        axum::Json(ApiResponse::with_meta(
            serde_json::json!({ "added": count }),
            meta_total(all.len()),
        )),
    )
}

async fn api_remove_address(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path(addr): axum::extract::Path<String>,
) -> axum::Json<ApiResponse<serde_json::Value>> {
    let addrs = vec![addr];
    let _ = manage_addresses(&state.file_path, &addrs, false);
    let all = load_addresses_from_api(&state.file_path).unwrap_or_default();
    axum::Json(ApiResponse::with_meta(
        serde_json::json!({ "removed": 1 }),
        meta_total(all.len()),
    ))
}

/// O(1) Memory read for the Dashboard UI — reads from the in-memory ring
/// buffer instead of re-parsing the entire JSONL file from disk.
async fn api_list_deposits(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::Json<ApiResponse<Vec<DepositResult>>> {
    let cache = state.recent_deposits.read().await;
    let total = cache.len();
    axum::Json(ApiResponse::with_meta(
        cache.iter().cloned().collect(),
        meta_total(total),
    ))
}

/// Serve the current configuration so the UI can list existing chains/assets.
/// Returns the nested structure: `{ "data": { "chains": { ... } } }`.
async fn api_get_config(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::Json<ApiResponse<AppConfig>> {
    match load_config(&state.config_path) {
        Ok(config) => axum::Json(ApiResponse::new(config)),
        Err(_) => axum::Json(ApiResponse::new(AppConfig {
            chains: HashMap::new(),
        })),
    }
}

// POST /v1/chains — Add a new chain via API (hot-reloaded on next watch cycle)
async fn api_add_chain(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::Json(payload): axum::Json<AddChainPayload>,
) -> Result<
    (
        axum::http::StatusCode,
        axum::Json<ApiResponse<serde_json::Value>>,
    ),
    (axum::http::StatusCode, axum::Json<ApiErrors>),
> {
    // Validate CAIP-2 format
    if !payload.caip2.contains(':') {
        return Err(ApiErrors::bad_request(format!(
            "invalid CAIP-2 format: {} (expected namespace:reference)",
            payload.caip2
        )));
    }

    let content = std::fs::read_to_string(&state.config_path).unwrap_or_default();
    let mut doc = match content.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => {
            return Err(ApiErrors::bad_request("invalid TOML format in config file"));
        }
    };

    // Check if chain already exists
    if let Some(chains) = doc.get("chains").and_then(|i| i.as_table())
        && chains.contains_key(&payload.name)
    {
        return Err(ApiErrors::conflict(format!(
            "chain already exists: {}",
            payload.name
        )));
    }

    let mut new_chain = Table::new();
    new_chain.insert("caip2", toml_edit::value(payload.caip2.clone()));

    let mut rpc_array = toml_edit::Array::new();
    for url in &payload.rpc {
        rpc_array.push(url.as_str());
    }
    new_chain.insert("rpc", Item::Value(rpc_array.into()));

    if let Some(sb) = payload.start_block {
        new_chain.insert("start_block", toml_edit::value(sb as i64));
    }
    if let Some(eb) = payload.end_block {
        new_chain.insert("end_block", toml_edit::value(eb as i64));
    }

    if !doc.contains_key("chains") {
        doc.insert("chains", Item::Table(Table::new()));
    }
    doc["chains"]
        .as_table_mut()
        .unwrap()
        .insert(&payload.name, Item::Table(new_chain));

    let _ = std::fs::write(&state.config_path, doc.to_string());

    let chain_data = serde_json::json!({
        "name": payload.name,
        "caip2": payload.caip2,
        "rpc": payload.rpc,
    });

    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(ApiResponse::new(chain_data)),
    ))
}

// DELETE /v1/chains/:name — Remove a chain by name via API
async fn api_remove_chain(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<
    axum::Json<ApiResponse<serde_json::Value>>,
    (axum::http::StatusCode, axum::Json<ApiErrors>),
> {
    let content = std::fs::read_to_string(&state.config_path).unwrap_or_default();
    let mut doc = match content.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => {
            return Err(ApiErrors::bad_request("invalid TOML format in config file"));
        }
    };

    if let Some(chains) = doc.get_mut("chains").and_then(|i| i.as_table_mut())
        && chains.remove(&name).is_some()
    {
        let remaining: Vec<String> = chains.iter().map(|(k, _)| k.to_string()).collect();
        let _ = std::fs::write(&state.config_path, doc.to_string());

        return Ok(axum::Json(ApiResponse::with_meta(
            serde_json::json!({ "removed": name }),
            serde_json::json!({ "remaining_chains": remaining }),
        )));
    }

    Err(ApiErrors::not_found(format!("chain not found: {}", name)))
}

// POST /v1/assets — Add a new asset to a chain via API
async fn api_add_asset(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::Json(payload): axum::Json<AddAssetPayload>,
) -> Result<
    (
        axum::http::StatusCode,
        axum::Json<ApiResponse<serde_json::Value>>,
    ),
    (axum::http::StatusCode, axum::Json<ApiErrors>),
> {
    let content = std::fs::read_to_string(&state.config_path).unwrap_or_default();
    let mut doc = match content.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => {
            return Err(ApiErrors::bad_request("invalid TOML format in config file"));
        }
    };

    // Navigate to [chains.<chain_name>]
    let chains_table = match doc.get_mut("chains").and_then(|i| i.as_table_mut()) {
        Some(t) => t,
        None => return Err(ApiErrors::not_found("no [chains] section found")),
    };

    let chain_table = match chains_table
        .get_mut(&payload.chain)
        .and_then(|i| i.as_table_mut())
    {
        Some(t) => t,
        None => {
            return Err(ApiErrors::not_found(format!(
                "chain not found: {}",
                payload.chain
            )));
        }
    };

    // Ensure [chains.<chain_name>.assets] sub-table exists
    if !chain_table.contains_key("assets") {
        chain_table.insert("assets", Item::Table(Table::new()));
    }

    let assets_table = match chain_table["assets"].as_table_mut() {
        Some(t) => t,
        None => {
            return Err(ApiErrors::bad_request("failed to access assets table"));
        }
    };

    // Check if asset already exists
    if assets_table.contains_key(&payload.name) {
        return Err(ApiErrors::conflict(format!(
            "asset already exists on chain {}: {}",
            payload.chain, payload.name
        )));
    }

    let mut asset_table = Table::new();
    asset_table.insert("contract", toml_edit::value(&payload.contract));
    asset_table.insert("decimals", toml_edit::value(payload.decimals as i64));

    assets_table.insert(&payload.name, Item::Table(asset_table));

    let _ = std::fs::write(&state.config_path, doc.to_string());

    let asset_data = serde_json::json!({
        "chain": payload.chain,
        "name": payload.name,
        "contract": payload.contract,
        "decimals": payload.decimals,
    });

    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(ApiResponse::new(asset_data)),
    ))
}

// DELETE /v1/assets/:chain/:asset — Remove an asset via API
async fn api_remove_asset(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path((chain, asset)): axum::extract::Path<(String, String)>,
) -> Result<
    axum::Json<ApiResponse<serde_json::Value>>,
    (axum::http::StatusCode, axum::Json<ApiErrors>),
> {
    let content = std::fs::read_to_string(&state.config_path).unwrap_or_default();
    let mut doc = match content.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => {
            return Err(ApiErrors::bad_request("invalid TOML format in config file"));
        }
    };

    if let Some(chains_table) = doc.get_mut("chains").and_then(|i| i.as_table_mut())
        && let Some(chain_table) = chains_table.get_mut(&chain).and_then(|i| i.as_table_mut())
        && let Some(assets_table) = chain_table.get_mut("assets").and_then(|i| i.as_table_mut())
    {
        if assets_table.remove(&asset).is_some() {
            let remaining: Vec<String> = assets_table.iter().map(|(k, _)| k.to_string()).collect();
            let _ = std::fs::write(&state.config_path, doc.to_string());

            return Ok(axum::Json(ApiResponse::with_meta(
                serde_json::json!({ "removed": { "chain": chain, "name": asset } }),
                serde_json::json!({ "remaining_assets_on_chain": remaining }),
            )));
        } else {
            return Err(ApiErrors::not_found(format!(
                "asset not found on chain {}: {}",
                chain, asset
            )));
        }
    }

    Err(ApiErrors::not_found(format!("chain not found: {}", chain)))
}
