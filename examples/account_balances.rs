//! Show the authenticated account's balance, collateral and margin.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example account_balances
//! ```
//!
//! See the `positions` example for a breakdown of open positions.
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    let account = client.fetch_balance().await?;
    println!("balance          {}", account.balance);
    println!("collateral       {}", account.collateral);
    println!("equity           {}", account.equity);
    println!("available margin {}", account.available_margin);
    println!("open positions   {}", account.positions.len());
    Ok(())
}
