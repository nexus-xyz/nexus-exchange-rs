//! Every name R2.25 retired (ENG-17743) still compiles, as a `#[deprecated]`
//! alias, and sends exactly the request its canonical method sends.
//!
//! Each pair is called against a server that answers everything `400`, so the
//! decode never runs and only the request matters. Method, path, query and body
//! are compared; headers are not, because the HMAC signature carries a fresh
//! timestamp on every call.
#![allow(deprecated)]

use nexus_exchange::types::{AmendOrder, Decimal, MarginDirection};
use nexus_exchange::{Client, Config, EthSigner};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

type Seen = (String, String, Option<String>, Vec<u8>);

/// The one request the last call issued. Fails if it issued none, so a pair
/// that sends nothing cannot pass by comparing two stale entries.
async fn last_request(server: &MockServer, before: usize) -> (Seen, usize) {
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), before + 1, "expected exactly one request");
    let r = requests.last().unwrap();
    let seen = (
        r.method.to_string(),
        r.url.path().to_string(),
        r.url.query().map(str::to_string),
        r.body.clone(),
    );
    (seen, requests.len())
}

/// Call `$new` then `$old` and assert they sent the same request.
macro_rules! same_request {
    ($server:expr, $count:ident, $old:literal => $old_call:expr, $new_call:expr) => {{
        let _ = $new_call.await;
        let (new, n) = last_request(&$server, $count).await;
        let _ = $old_call.await;
        let (old, n) = last_request(&$server, n).await;
        assert_eq!(
            old, new,
            "`{}` must send what its canonical method sends",
            $old
        );
        $count = n;
    }};
}

#[tokio::test]
async fn every_old_name_forwards_to_its_canonical_method() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let client = Client::new(Config::with_base_url(server.uri()).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ));
    let amount = "10".parse::<Decimal>().unwrap();
    let amend = AmendOrder::new().price("100".parse::<Decimal>().unwrap());
    // Canonical Hardhat/ethers account #0, as in tests/auth.rs.
    let signer =
        EthSigner::from_hex("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
            .unwrap();
    let mut n = 0;

    same_request!(server, n, "fetch_account_fees" =>
        client.fetch_account_fees(), client.fetch_trading_fees());
    same_request!(server, n, "adjust_margin" =>
        client.adjust_margin("BTC-USDX-PERP", MarginDirection::Add, amount),
        client.add_margin("BTC-USDX-PERP", amount));
    same_request!(server, n, "adjust_margin" =>
        client.adjust_margin("BTC-USDX-PERP", MarginDirection::Remove, amount),
        client.remove_margin("BTC-USDX-PERP", amount));
    same_request!(server, n, "fetch_account_adl_history" =>
        client.fetch_account_adl_history("0xabc", Some(5)),
        client.fetch_adl_history("0xabc", Some(5)));
    same_request!(server, n, "fetch_market_adl_events" =>
        client.fetch_market_adl_events("BTC-USDX-PERP", Some(5)),
        client.fetch_adl_events("BTC-USDX-PERP", Some(5)));
    same_request!(server, n, "fetch_tier_overrides" =>
        client.fetch_tier_overrides(), client.fetch_tiers());
    same_request!(server, n, "set_account_tier" =>
        client.set_account_tier("0xabc", "Pro"), client.set_tier("0xabc", "Pro"));
    same_request!(server, n, "reset_account_tier" =>
        client.reset_account_tier("0xabc"), client.delete_tier("0xabc"));
    same_request!(server, n, "fetch_account_funding" =>
        client.fetch_account_funding(Some(5)), client.fetch_funding_history(Some(5)));
    same_request!(server, n, "fetch_market_summaries" =>
        client.fetch_market_summaries(), client.fetch_markets_summary());
    same_request!(server, n, "fetch_funding_premium_samples" =>
        client.fetch_funding_premium_samples("BTC-USDX-PERP", Some(5)),
        client.fetch_funding_samples("BTC-USDX-PERP", Some(5)));
    same_request!(server, n, "fetch_order_history" =>
        client.fetch_order_history(Some(5)), client.fetch_orders(Some(5)));
    same_request!(server, n, "fetch_order_history_paginated" =>
        client.fetch_order_history_paginated().next_page(),
        client.fetch_orders_paginated().next_page());
    same_request!(server, n, "fetch_closed_positions" =>
        client.fetch_closed_positions(Some(5)), client.fetch_positions_history(Some(5)));
    same_request!(server, n, "fetch_closed_positions_paginated" =>
        client.fetch_closed_positions_paginated().next_page(),
        client.fetch_positions_history_paginated().next_page());
    same_request!(server, n, "amend_order" =>
        client.amend_order("o-1", "BTC-USDX-PERP", &amend),
        client.edit_order("o-1", "BTC-USDX-PERP", &amend));
    same_request!(server, n, "health_check" =>
        client.health_check(), client.fetch_status());
    same_request!(server, n, "mint_web_socket_token" =>
        client.mint_web_socket_token(), client.create_ws_token());
    same_request!(server, n, "sign_in" =>
        client.sign_in(&signer), client.login(&signer.sign_in().unwrap().signature));
    let _ = n;
}
