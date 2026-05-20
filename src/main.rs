use clap::Parser;
use rustplorer::{load_addresses, load_config, run_indexer, AppConfig, DepositResult, Format};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

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

    #[arg(long, help = "Add address(es) to file and exit (repeatable)")]
    add_address: Option<Vec<String>>,

    #[arg(long, help = "Remove address(es) from file and exit (repeatable)")]
    remove_address: Option<Vec<String>>,
}

#[derive(Clone)]
struct ApiState {
    file_path: PathBuf,
    output_path: Option<PathBuf>,
}

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = CliArgs::parse();

    if let Some(ref addrs) = args.add_address {
        manage_addresses(&args.addresses, addrs, true)?;
        println!("Added {} address(es) to {:?}", addrs.len(), args.addresses);
        return Ok(());
    }
    if let Some(ref addrs) = args.remove_address {
        manage_addresses(&args.addresses, addrs, false)?;
        println!(
            "Removed {} address(es) from {:?}",
            addrs.len(),
            args.addresses
        );
        return Ok(());
    }

    if args.watch || args.api_port.is_some() {
        run_watch_mode(args).await
    } else {
        run_single(args).await
    }
}

async fn run_watch_mode(args: CliArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!(
        "[rustplorer] Starting daemon mode (interval: {}s)",
        args.interval
    );

    if let Some(port) = args.api_port {
        let state = ApiState {
            file_path: args.addresses.clone(),
            output_path: args.output.clone(),
        };
        tokio::spawn(async move {
            let app = axum::Router::new()
                .route(
                    "/",
                    axum::routing::get(|| async {
                        axum::response::Html(include_str!("../index.html"))
                    }),
                )
                .route("/addresses", axum::routing::get(api_list_addresses))
                .route("/addresses", axum::routing::post(api_add_address))
                .route("/addresses", axum::routing::delete(api_remove_address))
                .route("/deposits", axum::routing::get(api_list_deposits))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
                .await
                .unwrap();
            eprintln!("[rustplorer] Address API listening on port {}", port);
            eprintln!("[rustplorer] Dashboard: http://localhost:{}/", port);
            axum::serve(listener, app).await.unwrap();
        });
    }

    let mut last_scanned: HashMap<String, u64> = HashMap::new();

    loop {
        let mut config = load_config(&args.config)?;
        apply_overrides(&mut config, &args);

        for chain in config.chains.iter_mut() {
            if let Some(&last_block) = last_scanned.get(&chain.caip2) {
                chain.start_block = Some(last_block + 1);
                chain.end_block = None;
            }
        }

        if args.verbose {
            eprintln!(
                "[rustplorer] Loaded {} chains, {} assets",
                config.chains.len(),
                config.assets.len()
            );
            eprintln!("[rustplorer] Loading addresses from {:?}", args.addresses);
        }

        let targets = load_addresses(&args.addresses)?;
        if args.verbose {
            eprintln!(
                "[rustplorer] Cached {} target addresses in memory",
                targets.len()
            );
        }

        let index_result = run_indexer(config.chains, config.assets, Arc::new(targets)).await?;

        for (chain, end_block) in index_result.latest_blocks {
            last_scanned.insert(chain, end_block);
        }

        if !index_result.deposits.is_empty() {
            if let Some(ref path) = args.output {
                append_output(path, &index_result.deposits, args.format)?;
            } else {
                println!("{}", format_results(&index_result.deposits, args.format)?);
            }
        }

        if args.verbose {
            eprintln!(
                "[rustplorer] Cycle complete: {} deposits found. Sleeping {}s...",
                index_result.deposits.len(),
                args.interval
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(args.interval)).await;
    }
}

async fn run_single(args: CliArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if args.verbose {
        eprintln!("[rustplorer] Loading config from {:?}", args.config);
    }

    let mut config: AppConfig = load_config(&args.config)?;
    apply_overrides(&mut config, &args);

    if args.verbose {
        eprintln!(
            "[rustplorer] Loaded {} chains, {} assets",
            config.chains.len(),
            config.assets.len()
        );
        eprintln!("[rustplorer] Loading addresses from {:?}", args.addresses);
    }

    let targets = load_addresses(&args.addresses)?;
    eprintln!(
        "[rustplorer] Cached {} target addresses in memory",
        targets.len()
    );

    let index_result = run_indexer(config.chains, config.assets, Arc::new(targets)).await?;

    let output_string = format_results(&index_result.deposits, args.format)?;

    if let Some(path) = args.output {
        let mut file = File::create(&path)?;
        file.write_all(output_string.as_bytes())?;
        eprintln!(
            "[rustplorer] Saved {} deposit records to {:?}",
            index_result.deposits.len(),
            path
        );
    } else {
        println!("{}", output_string);
    }

    Ok(())
}

fn apply_overrides(config: &mut AppConfig, args: &CliArgs) {
    for chain in config.chains.iter_mut() {
        if let Some(ref target_net) = args.network {
            if chain.caip2 != *target_net {
                continue;
            }
        }

        if let Some(sb) = args.start_block {
            if args.verbose {
                eprintln!(
                    "[rustplorer] Override: {} start_block = {}",
                    chain.caip2, sb
                );
            }
            chain.start_block = Some(sb);
        }

        if let Some(eb) = args.end_block {
            if args.verbose {
                eprintln!("[rustplorer] Override: {} end_block = {}", chain.caip2, eb);
            }
            chain.end_block = Some(eb);
        }

        if let Some(ref rpc_str) = args.rpc {
            if args.verbose {
                eprintln!("[rustplorer] Override: {} rpc = {}", chain.caip2, rpc_str);
            }
            chain.rpc = rpc_str.split(',').map(|s| s.trim().to_string()).collect();
        }
    }

    if let Some(target_net) = &args.network {
        config.chains.retain(|c| c.caip2 == *target_net);
    }
}

fn format_results(
    results: &[DepositResult],
    format: Format,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

fn append_output(
    path: &PathBuf,
    results: &[DepositResult],
    format: Format,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

async fn api_list_addresses(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::Json<Vec<String>> {
    let addrs = load_addresses_from_api(&state.file_path).unwrap_or_default();
    axum::Json(addrs.into_iter().collect())
}

async fn api_add_address(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::Json(payload): axum::Json<AddressPayload>,
) -> axum::Json<String> {
    let addrs = payload.into_addrs();
    let count = addrs.len();
    let _ = manage_addresses(&state.file_path, &addrs, true);
    axum::Json(format!("{} address(es) added", count))
}

async fn api_remove_address(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::Json(payload): axum::Json<AddressPayload>,
) -> axum::Json<String> {
    let addrs = payload.into_addrs();
    let count = addrs.len();
    let _ = manage_addresses(&state.file_path, &addrs, false);
    axum::Json(format!("{} address(es) removed", count))
}

fn load_addresses_from_api(
    path: &std::path::Path,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
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

async fn api_list_deposits(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::Json<Vec<DepositResult>> {
    let mut deposits = Vec::new();

    if let Some(out_path) = &state.output_path {
        if out_path.exists() {
            if let Ok(file) = File::open(out_path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Ok(deposit) = serde_json::from_str::<DepositResult>(&line) {
                        deposits.push(deposit);
                    }
                }
            }
        }
    }

    axum::Json(deposits)
}
