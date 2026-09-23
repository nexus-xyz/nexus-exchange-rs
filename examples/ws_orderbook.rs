//! Stream live order-book snapshots from the public market-data socket
//! (`/stream`). No credentials.
//!
//! ```text
//! cargo run --example ws_orderbook
//! cargo run --example ws_orderbook -- ETH-USDX-PERP
//! ```
//!
//! Every book frame is a full top-20 snapshot, so the local book is replaced,
//! never merged. The public load balancer closes the socket about every 30 s;
//! the client reconnects and resubscribes on its own, so those show up here as
//! a `disconnected` line followed by `connected`. This example prints a
//! handful of events and then closes; a real consumer would loop indefinitely.
use nexus_exchange::stream::{StreamChannel, StreamEvent};
use nexus_exchange::{Client, Config, Network};

/// The default market. Swap for any symbol from `fetch_markets`.
const DEFAULT_MARKET: &str = "BTC-USDX-PERP";

/// Stop after this many data events so the example terminates on its own.
const MAX_EVENTS: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let market = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MARKET.to_string());
    let client = Client::new(Config::new(Network::Testnet));

    // Book, trades and market status, in the one subscribe message the
    // server reads.
    let mut stream = client.market_stream(StreamChannel::all_for(market.clone()))?;

    let mut seen = 0usize;
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Connected => println!("connected, subscribed to {market}"),
            StreamEvent::Disconnected { reason } => println!("disconnected: {reason}"),
            StreamEvent::Book(book) => {
                let best = |levels: &[nexus_exchange::stream::BookLevel]| {
                    levels
                        .first()
                        .map(|l| format!("{} x {}", l.price, l.quantity))
                        .unwrap_or_else(|| "-".to_string())
                };
                // `sequence` only increases, but skips values; a jump is not a
                // missed update.
                println!(
                    "book {} seq {}: {} bids / {} asks, best bid {}, best ask {}",
                    book.market_id,
                    book.sequence,
                    book.bids.len(),
                    book.asks.len(),
                    best(&book.bids),
                    best(&book.asks),
                );
                seen += 1;
            }
            StreamEvent::Trade { trade, .. } => {
                println!("trade {:?} {} @ {}", trade.side, trade.amount, trade.price);
                seen += 1;
            }
            StreamEvent::MarketHalted { reason, .. } => println!("halted: {reason}"),
            StreamEvent::MarketResumed { .. } => println!("resumed"),
            // Updates were dropped (by the server, or by us reading too
            // slowly). Nothing is replayed: refetch over REST if it matters.
            StreamEvent::Gap { missed } => println!("gap: server dropped {missed} updates"),
            StreamEvent::Lagged { dropped } => println!("lagged: dropped {dropped} events"),
            other => println!("other: {other:?}"),
        }
        if seen >= MAX_EVENTS {
            break;
        }
    }

    stream.close().await;
    Ok(())
}
