//! v0.7.1 surface: bridge Phase A (deposits), cancel-on-disconnect, and the new
//! triggerable/trailing order types. Covers wire (de)serialization, request
//! signing, path-segment encoding, and the local validation guard.

use nexus_exchange::types::{Order, OrderRequest, OrderType, Side, TimeInForce};
use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{body_json, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

// --- Cancel-on-disconnect ----------------------------------------------------

#[tokio::test]
async fn fetch_cancel_on_disconnect_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/cancel-on-disconnect"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "enabled": true, "active": false, "grace_secs": 30
        })))
        .mount(&server)
        .await;
    let cod = authed(server.uri())
        .fetch_cancel_on_disconnect()
        .await
        .unwrap();
    assert!(cod.enabled);
    assert!(!cod.active);
    assert_eq!(cod.grace_secs, Some(30));
}

#[tokio::test]
async fn set_cancel_on_disconnect_puts_body_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/account/cancel-on-disconnect"))
        .and(header_exists("x-signature"))
        .and(body_json(serde_json::json!({ "enabled": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "enabled": true, "active": true, "grace_secs": null
        })))
        .mount(&server)
        .await;
    let cod = authed(server.uri())
        .set_cancel_on_disconnect(true)
        .await
        .unwrap();
    assert!(cod.enabled && cod.active);
    assert_eq!(cod.grace_secs, None);
}

// --- Bridge Phase A ----------------------------------------------------------

#[tokio::test]
async fn fetch_bridge_assets_parses_public() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/bridge/assets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "chains": [{
                "chain": "ethereum", "chain_id": 1,
                "deposit_assets": [{
                    "symbol": "USDC", "decimals": 6, "min_amount": "1.5",
                    "confirmations": 12, "fee": "0", "contract_address": "0xabc"
                }],
                "withdraw_assets": []
            }]
        })))
        .mount(&server)
        .await;
    // Public — no credentials needed.
    let resp = Client::new(Config::with_base_url(server.uri()))
        .fetch_bridge_assets()
        .await
        .unwrap();
    assert_eq!(resp.chains.len(), 1);
    let chain = &resp.chains[0];
    assert_eq!(chain.chain, "ethereum");
    assert_eq!(chain.chain_id, Some(1));
    assert_eq!(chain.deposit_assets[0].symbol, "USDC");
    assert_eq!(chain.deposit_assets[0].min_amount.to_string(), "1.5");
    assert!(chain.withdraw_assets.is_empty());
}

#[tokio::test]
async fn create_bridge_deposit_address_posts_body_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/bridge/deposit-addresses"))
        .and(header_exists("x-signature"))
        .and(body_json(serde_json::json!({ "chain": "base" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "address": "0xdeadbeef", "chain": "base", "accepts": ["USDC", "USDX"],
            "account_id": "0xacc", "created_at": 1_700_000_000_000i64
        })))
        .mount(&server)
        .await;
    let addr = authed(server.uri())
        .create_bridge_deposit_address("base")
        .await
        .unwrap();
    assert_eq!(addr.address, "0xdeadbeef");
    assert_eq!(addr.chain, "base");
    assert_eq!(addr.accepts, vec!["USDC".to_string(), "USDX".to_string()]);
}

#[tokio::test]
async fn create_bridge_deposit_address_blank_chain_rejected_without_request() {
    // No mock: a request escaping the client would surface as a connection error
    // rather than the local validation error.
    let err = authed("http://127.0.0.1:1".to_string())
        .create_bridge_deposit_address("  ")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fetch_bridge_deposit_encodes_id_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/bridge/deposits/dep_42"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "dep_42", "account_id": "0xacc", "chain": "ethereum",
            "asset": "USDC", "amount": "100.25", "address": "0xdeadbeef",
            "status": "confirming", "confirmations": 3, "required_confirmations": 12,
            "tx_hash": null, "created_at": 1_700_000_000_000i64,
            "updated_at": 1_700_000_000_500i64, "credited_at": null
        })))
        .mount(&server)
        .await;
    let dep = authed(server.uri())
        .fetch_bridge_deposit("dep_42")
        .await
        .unwrap();
    assert_eq!(dep.id, "dep_42");
    assert_eq!(dep.asset, "USDC");
    assert_eq!(dep.amount.to_string(), "100.25");
    assert_eq!(dep.status, "confirming");
    assert_eq!(dep.confirmations, Some(3));
    assert_eq!(dep.credited_at, None);
}

// --- Order types -------------------------------------------------------------

#[test]
fn trailing_limit_serializes_offsets_and_wire_type() {
    let req = OrderRequest::trailing_limit(
        "BTC-USDX-PERP",
        Side::Buy,
        "1".parse().unwrap(),
        50,
        10,
        TimeInForce::Gtc,
    );
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["order_type"], "TrailingLimit");
    assert_eq!(v["trailing_offset_bps"], 50);
    assert_eq!(v["limit_offset_bps"], 10);
    // No trigger_price on a trailing order; not serialized.
    assert!(v.get("trigger_price").is_none());
}

#[test]
fn order_readback_surfaces_limit_offset_bps() {
    // The spec's Order response schema carries limit_offset_bps, so a read-back
    // TrailingLimit order must preserve it rather than silently drop the offset.
    let order: Order = serde_json::from_str(
        r#"{
            "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy",
            "order_type": "TrailingLimit", "quantity": "1", "filled_qty": "0",
            "status": "Open", "time_in_force": "GTC", "limit_offset_bps": 10
        }"#,
    )
    .unwrap();
    assert_eq!(order.order_type, OrderType::TrailingLimit);
    assert_eq!(order.limit_offset_bps, Some(10));

    // Absent on non-trailing types (and older payloads): defaults to None.
    let limit: Order = serde_json::from_str(
        r#"{
            "id": "o2", "market_id": "BTC-USDX-PERP", "side": "Sell",
            "order_type": "Limit", "price": "100", "quantity": "1",
            "filled_qty": "0", "status": "Open", "time_in_force": "GTC"
        }"#,
    )
    .unwrap();
    assert_eq!(limit.limit_offset_bps, None);

    // Explicit JSON null (the spec types the field `integer | null`) also
    // deserializes to None, same as when the key is absent.
    let explicit_null: Order = serde_json::from_str(
        r#"{
            "id": "o3", "market_id": "BTC-USDX-PERP", "side": "Sell",
            "order_type": "Limit", "price": "100", "quantity": "1",
            "filled_qty": "0", "status": "Open", "time_in_force": "GTC",
            "limit_offset_bps": null
        }"#,
    )
    .unwrap();
    assert_eq!(explicit_null.limit_offset_bps, None);
}

#[test]
fn stop_limit_serializes_trigger_price() {
    let req = OrderRequest::limit(
        "BTC-USDX-PERP",
        Side::Sell,
        "100".parse().unwrap(),
        "1".parse().unwrap(),
        TimeInForce::Gtc,
    )
    .with_trigger_price("95".parse().unwrap());
    // Flip to a triggerable type; the builder just carries the trigger price.
    let mut req = req;
    req.order_type = OrderType::StopLimit;
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["order_type"], "StopLimit");
    assert_eq!(v["trigger_price"], "95");
    assert!(v.get("trailing_offset_bps").is_none());
}
