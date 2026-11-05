use rmcp::ServiceExt;
use sha2::{Digest, Sha256};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => {}
        [arg] if arg == "--version" => {
            println!("auto-kicad {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [arg] if arg == "--build-info" => {
            println!(
                "{}",
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "contract_schema": auto_kicad::CONTRACT_SCHEMA_VERSION,
                    "target_os": std::env::consts::OS,
                    "target_arch": std::env::consts::ARCH,
                    "cargo_lock_sha256": hex::encode(Sha256::digest(include_bytes!("../../../Cargo.lock"))),
                    "native_versions": ["10.0.7"],
                    "transport": "mcp-stdio",
                    "distribution": "local-only"
                })
            );
            return Ok(());
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "auto-kicad {}\n\nUsage: auto-kicad [--version | --build-info | --help]\n\nWithout arguments, serve MCP over standard input/output.",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        _ => return Err("unsupported arguments; use --help".into()),
    }
    let _cleanup = runtime::owned_temp::cleanup_stale()?;
    let _config = runtime::RuntimeConfig::default();
    let state_dir = match std::env::var_os("AUTO_KICAD_STATE_DIR") {
        Some(path) => std::path::PathBuf::from(path),
        None => std::path::PathBuf::from(
            std::env::var_os("HOME").ok_or("HOME or AUTO_KICAD_STATE_DIR is required")?,
        )
        .join(".local/state/auto-kicad"),
    };
    let service = auto_kicad::Server::default()
        .with_state_dir(&state_dir)?
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    let _cleanup = runtime::owned_temp::cleanup_stale()?;
    Ok(())
}
