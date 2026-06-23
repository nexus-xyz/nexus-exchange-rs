//! Cancel a resting order by id, or cancel every open order at once.
//!
//! ```text
//! # cancel one order (id from the place_order example):
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example cancel_order <ORDER_ID>
//!
//! # cancel ALL open orders (omit the id):
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example cancel_order
//! ```
//!
//! ⚠️ Defaults to `Network::Beta` so a copy-paste run does not touch production.
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Beta).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    // The order id is an optional positional argument. With it, cancel that one
    // order; without it, cancel everything — so the destructive "cancel all"
    // path is opt-in by omission rather than the default for a stray run.
    match std::env::args().nth(1) {
        Some(order_id) => {
            let ack = client.cancel_order(&order_id).await?;
            println!("cancelled {order_id}: {ack}");
        }
        None => {
            let ack = client.cancel_all_orders().await?;
            println!("cancelled all open orders: {ack}");
        }
    }
    Ok(())
}
