//! Show account equity and open positions.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example account_balances
//! ```
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    let account = client.fetch_balance().await?;
    println!(
        "equity {} | available margin {}",
        account.equity, account.available_margin
    );

    for p in client.fetch_positions().await? {
        println!(
            "  {} {} size {} @ {}",
            p.market_id, p.side, p.size, p.entry_price
        );
    }
    Ok(())
}
