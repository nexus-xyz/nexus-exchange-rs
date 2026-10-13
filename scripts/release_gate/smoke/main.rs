//! Pre-publish smoke test (ENG-18798): one unauthenticated read against the public testnet,
//! through the crate exactly as `cargo package` packs it.
//!
//! `.github/workflows/pre-publish.yml` builds this file as the only source of a fresh binary crate
//! whose one dependency is the unpacked `.crate`, so it sees what a `cargo add nexus-exchange` user
//! would get, not the source tree. The read lists markets, the same read every SDK's smoke test
//! makes: `fetch_markets_summary`, the keyless way to enumerate them (`tests/public.rs` covers it
//! offline). `examples/public_endpoints.rs` lists them with `fetch_markets` instead, which cannot
//! decode what testnet serves while the spec pin sits at v0.8.1 (ENG-18801), on main and on the
//! published 0.11.1 alike.
//!
//! Three outcomes, kept apart by exit code, because "could not reach testnet" must never read as a
//! pass and is not the SDK's fault either:
//!
//!   0  passed       the read decoded and returned at least one market
//!   1  failed       the SDK got an answer and could not use it (decode, 4xx, empty list)
//!   2  unreachable  no usable answer from testnet: network, timeout, 5xx, rate limit
//!
//! No keys, no writes, no orders. `NEXUS_SMOKE_BASE_URL` points it elsewhere, for testing the
//! outcomes themselves.
use nexus_exchange::{Client, Config, CustomNetwork, Error, Funds, Network, TransientError};

#[tokio::main]
async fn main() {
    let network = match std::env::var("NEXUS_SMOKE_BASE_URL") {
        Ok(url) if !url.is_empty() => match CustomNetwork::new("smoke", url, Funds::Unknown) {
            Ok(custom) => Network::Custom(custom),
            Err(e) => finish(1, &format!("NEXUS_SMOKE_BASE_URL is not usable: {e}")),
        },
        _ => Network::Testnet,
    };
    let target = network.base_url().to_string();
    let client = Client::new(Config::new(network));

    match client.fetch_markets_summary().await {
        Ok(markets) if !markets.is_empty() => finish(
            0,
            &format!(
                "fetch_markets_summary decoded {} markets from {target} (first: {})",
                markets.len(),
                markets[0].market_id
            ),
        ),
        Ok(_) => finish(
            1,
            &format!("fetch_markets_summary decoded an EMPTY list from {target}"),
        ),
        Err(Error::Transient(
            e @ (TransientError::Network(_)
            | TransientError::Timeout
            | TransientError::Unavailable { .. }
            | TransientError::RateLimited { .. }),
        )) => finish(2, &format!("{target} gave no usable answer: {e}")),
        Err(e) => finish(
            1,
            &format!("fetch_markets_summary against {target} failed: {e:?}"),
        ),
    }
}

fn finish(code: i32, message: &str) -> ! {
    let outcome = match code {
        0 => "passed",
        2 => "unreachable",
        _ => "failed",
    };
    println!("smoke: {outcome}: {message}");
    std::process::exit(code)
}
