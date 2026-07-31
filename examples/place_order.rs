//! Place a limit order, normalizing it to the market's trading rules first.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example place_order
//! ```
//!
//! ⚠️ This example PLACES AN ORDER on whichever network it targets. It defaults
//! to `Network::Testnet`, where balances are synthetic play funds, so a
//! copy-paste run with live credentials cannot move real money. Trading real
//! funds means `Network::Mainnet`, which this release deliberately refuses to
//! target at all. The order is priced far below the market so it rests without
//! filling; cancel it afterwards with the `cancel_order` example.
use nexus_exchange::markets::Rounding;
use nexus_exchange::types::{Decimal, OrderRequest, Side, TimeInForce};
use nexus_exchange::{Client, Config, Network};

const MARKET: &str = "BTC-USDX-PERP";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Play funds by default — placing/cancelling orders mutates account state.
    let client = Client::new(Config::new(Network::Testnet).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    // Fetch the market's trading rules so we can snap price/size to its grid.
    // Submitting an off-tick price or sub-minimum size just earns an opaque
    // rejection, so validate locally first.
    let market = client
        .fetch_markets()
        .await?
        .into_iter()
        .find(|m| m.market_id == MARKET)
        .ok_or("market not found")?;

    // A deliberately low bid that should rest in the book rather than fill.
    let want_price = "10000".parse::<Decimal>()?;
    let want_size = "0.001".parse::<Decimal>()?;

    // Round to tick/lot and validate against min/max in one step. Sizes round
    // *down* by default, so we never submit more risk than we asked for.
    let (price, size) = market.normalize_order(want_price, want_size, Rounding::Down)?;
    println!("submitting {size} @ {price} (normalized to market rules)");

    let order = OrderRequest::limit(MARKET, Side::Buy, price, size, TimeInForce::Gtc);
    let placed = client.create_order(&order).await?;
    println!(
        "placed order {} — status {}",
        placed.order.id, placed.order.status
    );
    println!(
        "cancel it with: cargo run --example cancel_order {}",
        placed.order.id
    );
    Ok(())
}
