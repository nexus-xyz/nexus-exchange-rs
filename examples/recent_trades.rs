//! List recent public trades for one market — no authentication required.
//!
//! ```text
//! cargo run --example recent_trades
//! ```
use nexus_exchange::{Client, Config, Network};

/// The market to read trades from. Swap for any symbol from `fetch_markets`.
const MARKET: &str = "BTC-USDX-PERP";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable));

    // `None` lets the server pick its default page size; pass `Some(n)` to cap it.
    let trades = client.fetch_trades(MARKET, Some(20)).await?;
    println!("{} recent trades for {MARKET}", trades.len());

    for t in &trades {
        // `side` is the taker's direction; `is_liquidation` flags forced trades.
        let tag = if t.is_liquidation {
            " [liquidation]"
        } else {
            ""
        };
        println!(
            "  {} {:<4?} {} @ {} (cost {}){tag}",
            t.datetime, t.side, t.amount, t.price, t.cost,
        );
    }
    Ok(())
}
