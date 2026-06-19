//! Stream private per-account events (fills, orders) over the WebSocket.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example ws_user_events
//! ```
//!
//! The private stream is gated by a short-lived, single-use token minted over
//! the signed REST API, then presented in an auth frame before subscribing.
//!
//! ⚠️ The minted token is single-use. The client re-sends its frames verbatim
//! after an automatic reconnect, so the replayed (now-spent) token will not
//! re-authenticate. A long-running consumer should mint a fresh token and
//! reconnect itself on `Disconnected`; this example simply reports the drop and
//! stops.
use nexus_exchange::ws::Event;
use nexus_exchange::{Client, Config, Network};
use serde_json::json;

/// Stop after this many message frames so the example terminates on its own.
const MAX_MESSAGES: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    // Mint a single-use WS token over the signed REST API just before connecting.
    let token = client.mint_web_socket_token().await?.token;

    // Authenticate first, then subscribe. Frames are sent (and replayed on
    // reconnect) in order, so the auth frame precedes the channel subscriptions.
    let mut sub = client.connect(vec![
        json!({ "type": "auth", "token": token }),
        json!({ "type": "subscribe", "channel": "fills" }),
        json!({ "type": "subscribe", "channel": "orders" }),
    ]);

    let mut seen = 0usize;
    while let Some(event) = sub.next().await {
        match event {
            Event::Connected => println!("connected — authenticated user stream"),
            Event::Disconnected(reason) => {
                // The single-use token cannot survive an auto-reconnect; bail out
                // rather than spin replaying a spent token.
                println!("disconnected: {reason} — mint a fresh token to resume");
                break;
            }
            Event::Lagged { dropped } => println!("lagged: dropped {dropped} frames"),
            Event::Message(msg) => {
                println!("event: {msg}");
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

    sub.close().await;
    Ok(())
}
