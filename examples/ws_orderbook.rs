//! Stream live order-book updates over the WebSocket — no authentication.
//!
//! ```text
//! cargo run --example ws_orderbook
//! ```
//!
//! The client reconnects automatically with backoff and re-sends the
//! subscription after each reconnect. This example prints a handful of updates
//! and then closes; a real consumer would loop indefinitely.
use nexus_exchange::ws::Event;
use nexus_exchange::{Client, Config, Network};
use serde_json::json;

const MARKET: &str = "BTC-USDX-PERP";

/// Stop after this many message frames so the example terminates on its own.
const MAX_MESSAGES: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable));

    let mut sub = client.connect(vec![json!({
        "type": "subscribe",
        "channel": "orderbook",
        "market_id": MARKET,
    })]);

    let mut seen = 0usize;
    while let Some(event) = sub.next().await {
        match event {
            Event::Connected => println!("connected — subscribed to {MARKET} orderbook"),
            Event::Disconnected(reason) => println!("disconnected: {reason} (will reconnect)"),
            // `Lagged` means we read too slowly and the client dropped frames to
            // keep the socket drained. Surfaced so we can detect the gap.
            Event::Lagged { dropped } => println!("lagged: dropped {dropped} frames"),
            Event::Message(msg) => {
                println!("update: {msg}");
                seen += 1;
                if seen >= MAX_MESSAGES {
                    break;
                }
            }
            // `Event` is `#[non_exhaustive]`: ignore variants added in future
            // SDK versions rather than fail to compile against them.
            _ => {}
        }
    }

    // Close gracefully and wait for the background task to wind down.
    sub.close().await;
    Ok(())
}
