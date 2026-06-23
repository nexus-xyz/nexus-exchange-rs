//! List open positions for the authenticated account, with PnL.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example positions
//! ```
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    let positions = client.fetch_positions().await?;
    if positions.is_empty() {
        println!("no open positions");
        return Ok(());
    }

    for p in &positions {
        // `liquidation_price` is absent in flat / cross-margin states, so show a
        // placeholder rather than assuming it is always present.
        let liq = p
            .liquidation_price
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{} {} size {} @ {} | uPnL {} | rPnL {} | liq {}",
            p.market_id, p.side, p.size, p.entry_price, p.unrealized_pnl, p.realized_pnl, liq,
        );
    }
    Ok(())
}
