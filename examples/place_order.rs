//! Place a limit order, then cancel it.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example place_order
//! ```
use nexus_exchange::types::{Decimal, OrderRequest, Side, TimeInForce};
use nexus_exchange::{Client, Config, Network};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    let order = OrderRequest::limit(
        "BTC-USDX-PERP",
        Side::Buy,
        "50000".parse::<Decimal>()?,
        "0.001".parse::<Decimal>()?,
        TimeInForce::Gtc,
    );
    let placed = client.create_order(&order).await?;
    println!("placed {} ({})", placed.order.id, placed.order.status);

    let ack = client.cancel_order(&placed.order.id).await?;
    println!("cancelled: {ack}");
    Ok(())
}
