//! Cancel a resting order by id, cancel one market, or cancel every open order.
//!
//! ```text
//! # cancel one order (id from the place_order example) — its market routes it:
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example cancel_order <ORDER_ID> <MARKET_ID>
//!
//! # cancel every open order on ONE market (the market-maker reprice path):
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example cancel_order --market BTC-USDX-PERP
//!
//! # cancel ALL open orders, account-wide (omit every argument):
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

    // Argument shapes, narrowest scope first so the broadest, most destructive
    // "cancel all" path is opt-in by omission rather than the default for a
    // stray run:
    //   <ORDER_ID> <MARKET_ID>  cancel one order (its market routes the request)
    //   --market <MARKET_ID>    cancel every open order on that one market
    //   (no args)               cancel everything, account-wide
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--market") => {
            let market_id = args
                .next()
                .ok_or("--market requires a market id, e.g. BTC-USDX-PERP")?;
            let ack = client.cancel_orders_for_market(&market_id).await?;
            println!("cancelled all open orders on {market_id}: {ack}");
        }
        Some(order_id) => {
            // The engine routes a single-order-by-id cancel to the order's owning
            // market, so the market id is required alongside the order id.
            let market_id = args.next().ok_or(
                "cancelling one order needs its market id: cancel_order <ORDER_ID> <MARKET_ID>",
            )?;
            let ack = client.cancel_order(order_id, &market_id).await?;
            println!("cancelled {order_id} on {market_id}: {ack}");
        }
        None => {
            let ack = client.cancel_all_orders().await?;
            println!("cancelled all open orders: {ack}");
        }
    }
    Ok(())
}
