//! Browse public market data — no authentication required.
//!
//! ```text
//! cargo run --example public_endpoints
//! ```
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable));

    let markets = client.fetch_markets().await?;
    println!("{} markets", markets.len());

    if let Some(m) = markets.first() {
        let ticker = client.fetch_ticker(&m.market_id).await?;
        println!(
            "{}: last={:?} mark={:?}",
            ticker.symbol, ticker.last, ticker.mark_price
        );

        let book = client.fetch_order_book(&m.market_id).await?;
        println!(
            "top of book: bid {:?} / ask {:?}",
            book.bids.first().map(|l| l.price()),
            book.asks.first().map(|l| l.price()),
        );
    }
    Ok(())
}
