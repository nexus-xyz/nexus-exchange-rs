//! Fetch an order-book snapshot for one market — no authentication required.
//!
//! ```text
//! cargo run --example orderbook_snapshot
//! ```
use nexus_exchange::{Client, Config, Network};

/// The market to snapshot. Swap for any symbol from `fetch_markets`.
const MARKET: &str = "BTC-USDX-PERP";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable));

    let book = client.fetch_order_book(MARKET).await?;
    println!("{} order book (nonce {})", book.symbol, book.nonce);

    // Bids descend, asks ascend (CCXT convention), so the top of book is the
    // first level on each side. The book can be empty, so guard the lookups.
    let best_bid = book.bids.first();
    let best_ask = book.asks.first();
    if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
        let spread = ask.price() - bid.price();
        println!(
            "best bid {} x {} | best ask {} x {} | spread {}",
            bid.price(),
            bid.amount(),
            ask.price(),
            ask.amount(),
            spread,
        );
    } else {
        println!("book is empty on at least one side");
    }

    println!("\ntop 5 bids:");
    for level in book.bids.iter().take(5) {
        println!("  {:>12} x {}", level.price(), level.amount());
    }
    println!("top 5 asks:");
    for level in book.asks.iter().take(5) {
        println!("  {:>12} x {}", level.price(), level.amount());
    }
    Ok(())
}
