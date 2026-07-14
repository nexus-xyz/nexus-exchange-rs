//! Bridge deposit flow: discover assets, get-or-create a deposit address, and
//! inspect deposits. Send USDC/USDX to the printed address, then poll
//! `fetch_bridge_deposits` (or `fetch_bridge_deposit` by id) until the deposit
//! reaches `Credited`.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example bridge_deposit
//! ```
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    // 1. Discover bridgeable chains and assets.
    let assets = client.fetch_bridge_assets().await?;
    for chain in &assets.chains {
        let symbols: Vec<_> = chain.deposit_assets.iter().map(|a| a.symbol).collect();
        println!("{:<10} deposits: {:?}", chain.chain, symbols);
    }

    // 2. Get-or-create a deposit address on the first chain (idempotent).
    let chain = assets
        .chains
        .first()
        .map(|c| c.chain.clone())
        .ok_or("no bridgeable chains available")?;
    let addr = client.create_bridge_deposit_address(&chain).await?;
    println!(
        "\nDeposit {:?} to {} on {}",
        addr.accepts, addr.address, addr.chain
    );
    println!("Send USDC or USDX there; it will appear as a deposit below.\n");

    // 3. Inspect deposits. Poll this until the newest reaches Credited/Failed.
    let deposits = client
        .fetch_bridge_deposits(Some(5), Some(&chain), None, None)
        .await?;
    if deposits.is_empty() {
        println!("no deposits yet — send funds to the address above, then re-run.");
    }
    for d in &deposits {
        let confs = match (d.confirmations, d.required_confirmations) {
            (Some(c), Some(r)) => format!("{c}/{r} confs"),
            _ => "—".to_string(),
        };
        println!(
            "{} {:?} {} {:?} ({confs})",
            d.id, d.asset, d.amount, d.status
        );
    }
    Ok(())
}
