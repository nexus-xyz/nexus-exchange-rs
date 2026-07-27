//! Portfolio view for the authenticated account: consolidated state (summary +
//! positions with per-position risk detail), the effective fee schedule, and the
//! equity/PnL/volume time series.
//!
//! ```text
//! NEXUS_API_KEY=nx_… NEXUS_API_SECRET=<hex> cargo run --example portfolio
//! ```
use nexus_exchange::types::PortfolioWindow;
use nexus_exchange::{Client, Config, Network};

/// Render an optional risk field, showing why a value is missing rather than
/// printing a misleading `0`. The server nulls a risk field it cannot derive and
/// puts a machine-readable reason in the companion `*_error`.
fn show<T: std::fmt::Display>(value: Option<T>, reason: Option<&str>) -> String {
    match (value, reason) {
        (Some(v), _) => v.to_string(),
        (None, Some(why)) => format!("n/a ({why})"),
        (None, None) => "n/a".to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(Network::Stable).api_key(
        std::env::var("NEXUS_API_KEY")?,
        std::env::var("NEXUS_API_SECRET")?,
    ));

    // One call for the aggregates AND the positions: both halves come from a
    // single server-side read, so they cannot disagree the way separate
    // `fetch_account_summary` + `fetch_positions` calls can.
    let state = client.fetch_account_state().await?;
    let s = &state.summary;

    println!("== account ==");
    println!("equity          {}", s.total_equity);
    println!("collateral      {}", s.collateral);
    println!("unrealized PnL  {}", s.total_unrealized_pnl);
    println!("realized PnL 24h {}", s.total_realized_pnl_24h);
    println!("volume 24h      {}", s.total_volume_24h);
    println!("margin used     {}", s.margin_used);
    println!("free margin     {}", s.available_margin);
    // Gate a withdrawal on `withdrawable`, not `available_margin`: it is the
    // engine-authoritative figure, floored at zero. `None` only when the server
    // predates the field — fall back rather than reporting a wrong number.
    match s.withdrawable {
        Some(w) => println!("withdrawable    {w}"),
        None => println!("withdrawable    n/a (not reported by this server)"),
    }
    println!("open positions  {}", s.open_positions_count);
    println!("open orders     {}", s.open_orders_count);

    println!("\n== positions ==");
    if state.positions.is_empty() {
        println!("(none)");
    }
    for p in &state.positions {
        println!(
            "{} {} size {} @ {}",
            p.market_id, p.side, p.size, p.entry_price
        );
        println!(
            "    uPnL {} | notional {} | ROE {}",
            p.unrealized_pnl,
            show(p.notional_value, p.notional_value_error.as_deref()),
            show(p.roe, p.roe_error.as_deref()),
        );
        println!(
            "    margin {} | leverage {} | max leverage {}",
            show(p.margin_used, p.margin_used_error.as_deref()),
            show(p.leverage, p.leverage_error.as_deref()),
            show(p.max_leverage, p.max_leverage_error.as_deref()),
        );
        // Paid-positive: a negative value means the position RECEIVED funding.
        println!("    funding paid {}", show(p.funding_paid, None));
    }

    println!("\n== fees ==");
    let fees = client.fetch_account_fees().await?;
    // Maker bps is signed — negative is a rebate paid TO the maker.
    println!(
        "maker {} bps | taker {} bps | tier {} | schedule {}",
        fees.maker_fee_bps, fees.taker_fee_bps, fees.tier, fees.schedule
    );
    println!(
        "30d volume {}{}",
        fees.volume_30d,
        if fees.volume_30d_estimated {
            " (estimated — may undercount)"
        } else {
            ""
        }
    );

    println!("\n== 7-day portfolio history ==");
    // `None` limit returns the whole window; the served window is read back off
    // the response rather than assumed from the request.
    let history = client
        .fetch_portfolio_history(Some(PortfolioWindow::Week), None)
        .await?;
    println!(
        "window {} | cadence {}ms | {} point(s), oldest first",
        history.window,
        history.cadence_ms,
        history.points.len()
    );
    // Print the first and last sample so the output stays short whatever the
    // window's point capacity is.
    if let (Some(first), Some(last)) = (history.points.first(), history.points.last()) {
        for (label, p) in [("from", first), ("to  ", last)] {
            println!(
                "  {} t={} equity {} | PnL {} | volume {}",
                label, p.timestamp_ms, p.equity, p.pnl, p.volume
            );
        }
    }
    Ok(())
}
