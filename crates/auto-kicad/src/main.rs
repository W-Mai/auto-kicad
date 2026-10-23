use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    Ok(())
}
