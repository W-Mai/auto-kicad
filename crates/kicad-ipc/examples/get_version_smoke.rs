use std::env;

use kicad_ipc::{ClientConfig, KiCadClient, NngTransport};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::args()
        .nth(1)
        .unwrap_or_else(|| "ipc:///tmp/kicad/api.sock".to_owned());
    let config = ClientConfig::new(endpoint.clone(), "auto-kicad-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let first = client.get_version()?;
    let second = client.get_version()?;
    println!(
        "endpoint={endpoint} first={}.{}.{} second={}.{}.{}",
        first.major, first.minor, first.patch, second.major, second.minor, second.patch
    );
    Ok(())
}
