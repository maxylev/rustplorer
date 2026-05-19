use clap::Parser;
use rustplorer::{load_addresses, load_config, run_indexer, AppConfig, DepositResult, Format};
use std::fs::File;
use std::io::Write;
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = CliArgs::parse();

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

    let results = run_indexer(config.chains, config.assets, Arc::new(targets)).await?;

    let output_string = format_results(&results, args.format)?;

    if let Some(path) = args.output {
        let mut file = File::create(&path)?;
        file.write_all(output_string.as_bytes())?;
        eprintln!(
            "[rustplorer] Saved {} deposit records to {:?}",
            results.len(),
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
