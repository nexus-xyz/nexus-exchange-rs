//! Signed trading actions: the EIP-712 struct an order-path request carries
//! (spec "Signed trading actions", ENG-20652).
//!
//! The eight order-path writes (`POST /orders`, `POST /orders/batch`,
//! `PATCH /orders/{id}`, `DELETE /orders/{id}`, `DELETE /orders`,
//! `POST /account/margin`, `POST /account/margin-mode`, `POST /leverage`) each
//! sign one struct. This module is a port of the server's
//! `exchange-sec-utils::trading_request`, which rebuilds the struct from the
//! request it receives. The struct is built here from the exact method, path,
//! query and body bytes the client sends, so client and server read the same
//! request the same way.

use serde_json::{Map, Value};
use sha3::{Digest, Keccak256};

use super::eth::{address_word, finalize32, u256, EIP712_DOMAIN_NAME, EIP712_DOMAIN_VERSION};
use crate::{Error, Result};

/// The domain `chainId` every trading struct signs under. The spec fixes the
/// domain to `{name: "Nexus Exchange", version: "1", chainId: 20056}`, with no
/// `verifyingContract` and no `salt` (the `WithdrawIntent` domain).
const TRADING_CHAIN_ID: u64 = 20056;

/// `OrderParams`, the nested type of `PlaceOrder` and `PlaceOrders`.
const ORDER_PARAMS: &str = "OrderParams(string marketId,string side,string orderType,\
string price,string quantity,string timeInForce,bool reduceOnly,string stopPrice,\
string triggerPrice,string trailingOffsetBps,string limitOffsetBps,string stp,\
string clientId,string maxSlippageBps)";

/// The fields every struct shares besides the action.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Envelope<'a> {
    /// The account the action is for: the acting account, else the signer's own.
    pub(crate) account: [u8; 20],
    /// The deployment's name, e.g. `devnet`.
    pub(crate) domain: &'a str,
    pub(crate) timestamp_ms: u64,
    pub(crate) nonce: u64,
}

/// A trading route, read from the method and the trailing path segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route<'p> {
    PlaceOrder,
    PlaceOrders,
    AmendOrder(&'p str),
    CancelOrder(&'p str),
    CancelAllOrders,
    AdjustMargin,
    SetMarginMode,
    SetLeverage,
}

/// Whether `method` + `path` is one of the eight trading routes. A prefix such
/// as `/v1` does not change the route; `POST /orders/preview` and reads are not
/// one.
pub(crate) fn is_trading_route(method: &str, path: &str) -> bool {
    route(method, path).is_some()
}

/// The EIP-712 digest of the trading action `method` + `path` + `query` +
/// `body` carries, under `envelope`, or `None` when the request is not a
/// trading route. Refuses a body the server would refuse to rebuild.
pub(crate) fn request_digest(
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
    envelope: &Envelope<'_>,
) -> Result<Option<[u8; 32]>> {
    let Some(route) = route(method, path) else {
        return Ok(None);
    };
    let market_query = query_value(query, "market_id");
    let required_query = || {
        market_query
            .as_deref()
            .map(text_word)
            .ok_or_else(|| refused("`market_id` is missing from the query"))
    };
    let parsed = match route {
        Route::CancelOrder(_) | Route::CancelAllOrders => Value::Null,
        _ => serde_json::from_slice(body).map_err(|_| refused("the body is not JSON"))?,
    };
    let (fields, words): (&str, Vec<[u8; 32]>) = match route {
        Route::PlaceOrder => ("OrderParams order", vec![order_hash(object(&parsed)?)?]),
        Route::PlaceOrders => {
            let entries = parsed
                .as_array()
                .ok_or_else(|| refused("the batch body is not a JSON array"))?;
            let mut hasher = Keccak256::new();
            for entry in entries {
                hasher.update(order_hash(object(entry)?)?);
            }
            ("OrderParams[] orders", vec![finalize32(hasher)])
        }
        Route::AmendOrder(order_id) => {
            let body = object(&parsed)?;
            (
                "string marketId,string orderId,string price,string size",
                vec![
                    required_query()?,
                    text_word(order_id),
                    text_word(optional_text(body, "price")?),
                    text_word(optional_text(body, "size")?),
                ],
            )
        }
        Route::CancelOrder(order_id) => (
            "string marketId,string orderId",
            vec![required_query()?, text_word(order_id)],
        ),
        Route::CancelAllOrders => {
            if market_query.as_deref() == Some("") {
                return Err(empty_field("market_id"));
            }
            (
                "string marketId",
                vec![text_word(market_query.as_deref().unwrap_or_default())],
            )
        }
        Route::AdjustMargin => {
            let body = object(&parsed)?;
            (
                "string marketId,string amount,string direction",
                vec![
                    text_word(text(body, "market_id")?),
                    text_word(text(body, "amount")?),
                    text_word(text(body, "direction")?),
                ],
            )
        }
        Route::SetMarginMode => {
            let body = object(&parsed)?;
            (
                "string marketId,string marginMode",
                vec![
                    text_word(text(body, "market_id")?),
                    text_word(text(body, "margin_mode")?),
                ],
            )
        }
        Route::SetLeverage => {
            let body = object(&parsed)?;
            let leverage = optional_count(body, "leverage")?
                .ok_or_else(|| refused("`leverage` is missing"))?;
            (
                "string marketId,uint32 leverage",
                vec![
                    text_word(text(body, "market_id")?),
                    u256(u64::from(leverage)),
                ],
            )
        }
    };

    let name = match route {
        Route::PlaceOrder => "PlaceOrder",
        Route::PlaceOrders => "PlaceOrders",
        Route::AmendOrder(_) => "AmendOrder",
        Route::CancelOrder(_) => "CancelOrder",
        Route::CancelAllOrders => "CancelAllOrders",
        Route::AdjustMargin => "AdjustMargin",
        Route::SetMarginMode => "SetMarginMode",
        Route::SetLeverage => "SetLeverage",
    };
    let mut type_string =
        format!("{name}(address account,string domain,{fields},uint64 timestampMs,uint64 nonce)");
    if fields.starts_with("OrderParams") {
        type_string.push_str(ORDER_PARAMS);
    }

    let mut message = Keccak256::new_with_prefix(Keccak256::digest(type_string.as_bytes()));
    message.update(address_word(&envelope.account));
    message.update(text_word(envelope.domain));
    for word in words {
        message.update(word);
    }
    message.update(u256(envelope.timestamp_ms));
    message.update(u256(envelope.nonce));

    let mut digest = Keccak256::new_with_prefix([0x19, 0x01]);
    digest.update(domain_separator());
    digest.update(finalize32(message));
    Ok(Some(finalize32(digest)))
}

/// `{name: "Nexus Exchange", version: "1", chainId: 20056}`, no salt.
fn domain_separator() -> [u8; 32] {
    let mut hasher = Keccak256::new_with_prefix(Keccak256::digest(
        b"EIP712Domain(string name,string version,uint256 chainId)",
    ));
    hasher.update(text_word(EIP712_DOMAIN_NAME));
    hasher.update(text_word(EIP712_DOMAIN_VERSION));
    hasher.update(u256(TRADING_CHAIN_ID));
    finalize32(hasher)
}

fn route<'p>(method: &str, path: &'p str) -> Option<Route<'p>> {
    let path = path.split('?').next().unwrap_or(path);
    let mut segments = path.rsplit('/').filter(|segment| !segment.is_empty());
    let last = segments.next()?;
    let previous = segments.next();
    match (method.to_ascii_uppercase().as_str(), previous, last) {
        ("POST", _, "orders") => Some(Route::PlaceOrder),
        ("DELETE", _, "orders") => Some(Route::CancelAllOrders),
        ("POST", Some("orders"), "batch") => Some(Route::PlaceOrders),
        ("PATCH", Some("orders"), order_id) => Some(Route::AmendOrder(order_id)),
        ("DELETE", Some("orders"), order_id) => Some(Route::CancelOrder(order_id)),
        ("POST", Some("account"), "margin") => Some(Route::AdjustMargin),
        ("POST", Some("account"), "margin-mode") => Some(Route::SetMarginMode),
        ("POST", _, "leverage") => Some(Route::SetLeverage),
        _ => None,
    }
}

/// `hashStruct(OrderParams)` for one order body.
fn order_hash(fields: &Map<String, Value>) -> Result<[u8; 32]> {
    let reduce_only = match fields.get("reduce_only") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| refused("`reduce_only` is not a boolean"))?,
    };
    let mut hasher = Keccak256::new_with_prefix(Keccak256::digest(ORDER_PARAMS.as_bytes()));
    for word in [
        text_word(text(fields, "market_id")?),
        text_word(text(fields, "side")?),
        text_word(text(fields, "order_type")?),
        text_word(optional_text(fields, "price")?),
        text_word(text(fields, "quantity")?),
        text_word(text(fields, "time_in_force")?),
        u256(u64::from(reduce_only)),
        text_word(optional_text(fields, "stop_price")?),
        text_word(optional_text(fields, "trigger_price")?),
        text_word(&digits(optional_count(fields, "trailing_offset_bps")?)),
        text_word(&digits(optional_count(fields, "limit_offset_bps")?)),
        text_word(optional_text(fields, "stp")?),
        text_word(optional_text(fields, "client_id")?),
        text_word(&digits(optional_count(fields, "max_slippage_bps")?)),
    ] {
        hasher.update(word);
    }
    Ok(finalize32(hasher))
}

/// A `string` field's word: `keccak256(utf8(value))`.
fn text_word(value: &str) -> [u8; 32] {
    Keccak256::digest(value.as_bytes()).into()
}

fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| refused("the body is not a JSON object"))
}

fn text<'v>(fields: &'v Map<String, Value>, name: &str) -> Result<&'v str> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| refused(&format!("`{name}` is missing or not a string")))
}

/// Absent and `null` sign as `""`; an explicit `""` is refused, since it would
/// sign the same as absent and the server refuses it.
fn optional_text<'v>(fields: &'v Map<String, Value>, name: &str) -> Result<&'v str> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(value)) if value.is_empty() => Err(empty_field(name)),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(refused(&format!("`{name}` is not a string"))),
    }
}

/// A `u32` the server reads with `as_u64`, or `None` when absent or `null`.
fn optional_count(fields: &Map<String, Value>, name: &str) -> Result<Option<u32>> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| refused(&format!("`{name}` is not a whole number in the u32 range"))),
    }
}

/// An optional integer signs as its decimal digits, or `""` when absent.
fn digits(value: Option<u32>) -> String {
    value.map(|number| number.to_string()).unwrap_or_default()
}

/// The first `name=` value in `query`, key and value percent-decoded.
fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(key) == name).then(|| percent_decode(value))
    })
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let escaped = (byte == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .filter(|pair| pair.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match escaped {
            Some(value) => {
                decoded.push(value);
                index += 3;
            }
            None => {
                decoded.push(if byte == b'+' { b' ' } else { byte });
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn refused(reason: &str) -> Error {
    Error::invalid_request(format!("cannot sign this trading action: {reason}"))
}

fn empty_field(name: &str) -> Error {
    refused(&format!(
        "`{name}` was sent as \"\", which signs the same as absent and the server \
         refuses it; omit the field instead"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENVELOPE: Envelope<'static> = Envelope {
        account: [0x11; 20],
        domain: "prd-testnet",
        timestamp_ms: 1_700_000_000_000,
        nonce: 7,
    };
    const ORDER_ID: &str = "6f1c2b9e-3d4a-4f5b-8c7d-9e0f1a2b3c4d";
    const LIMIT_BODY: &str = r#"{"market_id":"BTC-USDX-PERP","side":"Buy","order_type":"Limit","price":"65000.5","quantity":"0.25","time_in_force":"GTC","stp":"CancelNewest","client_id":"order-1","max_slippage_bps":50}"#;
    const TRAILING_BODY: &str = r#"{"market_id":"ETH-USDX-PERP","side":"Sell","order_type":"TrailingStop","quantity":"1.5","time_in_force":"IOC","reduce_only":true,"trailing_offset_bps":25}"#;

    fn digest(method: &str, path: &str, query: &str, body: &str) -> Result<String> {
        request_digest(method, path, query, body.as_bytes(), &ENVELOPE)
            .map(|digest| hex::encode(digest.expect("a trading route")))
    }

    /// The server's pins, which viem's `hashTypedData` reproduces. The first
    /// eight are `exchange-sec-utils/src/trading_intent.rs ::
    /// tests::the_digests_are_pinned` (one per struct); the last three are
    /// `trading_request.rs`'s `*_rebuilds_the_pinned_digest` tests, which set
    /// the fields the first eight leave empty. Each body is the one
    /// `trading_request.rs` and the terminal's `trading-intent.test.ts` rebuild
    /// the same pin from. Copied, not captured from this code.
    #[test]
    fn requests_rebuild_the_servers_pinned_digests() {
        let order_path = format!("/orders/{ORDER_ID}");
        let batch = format!("[{LIMIT_BODY},{TRAILING_BODY}]");
        let cases: [(&str, &str, &str, &str, &str); 11] = [
            (
                "POST",
                "/orders",
                "",
                LIMIT_BODY,
                "15c5dd8665e1f92b194fb0b4932c561532a5100da669f8dbd402b8335fd58c7b",
            ),
            (
                "POST",
                "/orders/batch",
                "",
                &batch,
                "53c15273f220674d0510aff5eee8d892730917d499297d33595ac893d0b8ce2f",
            ),
            (
                "PATCH",
                &order_path,
                "market_id=BTC-USDX-PERP",
                r#"{"price":"65100"}"#,
                "723bc9cf519a7319623275969385653b538b3b8256835f0f1bb73b11378c9c06",
            ),
            (
                "DELETE",
                &order_path,
                "market_id=BTC-USDX-PERP",
                "",
                "04e7207aba00c39b57ba72949490c5a76bef60ad4079fc548df4fe622e032eef",
            ),
            (
                "DELETE",
                "/orders",
                "",
                "",
                "d9b1603bf0dda6e8c534997901394243f134c2ad36ce0b272c488c8fad285a5f",
            ),
            (
                "POST",
                "/account/margin",
                "",
                r#"{"market_id":"BTC-USDX-PERP","amount":"100","direction":"add"}"#,
                "56ee08e84884de1e3baebcaa9f36d5ae9a8568a29b2650003e06827d388cf896",
            ),
            (
                "POST",
                "/account/margin-mode",
                "",
                r#"{"market_id":"BTC-USDX-PERP","margin_mode":"isolated"}"#,
                "b4888215bfc76f851acda76338aa6163d05f894bf752efb2c092024fd187d5bc",
            ),
            (
                "POST",
                "/leverage",
                "",
                r#"{"market_id":"BTC-USDX-PERP","leverage":10}"#,
                "f2740853950fe99dc8fcc1acc8f6f345c37414666469bf4c4d82c4011a2ede5d",
            ),
            (
                "POST",
                "/orders",
                "",
                r#"{"market_id":"BTC-USDX-PERP","side":"Sell","order_type":"StopLimit","price":"64900","quantity":"0.5","time_in_force":"GTC","stop_price":"65000","trigger_price":"64950","limit_offset_bps":15}"#,
                "21f7423a134a55e4bc11d15ca9c4bd8e618825740ed5a935f57110451a44be97",
            ),
            (
                "PATCH",
                &order_path,
                "market_id=BTC-USDX-PERP",
                r#"{"price":"65100","size":"0.75"}"#,
                "0deda3a52b83258a40b4f2e4522d15977b107f2ecf0e7096fb1232967815182b",
            ),
            (
                "DELETE",
                "/orders",
                "market_id=BTC-USDX-PERP",
                "",
                "52ae8f0733f20bb01ca01651dfe35694fc6e30c32c0a7833bb245d765ecab489",
            ),
        ];
        for (method, path, query, body, pinned) in cases {
            assert_eq!(
                digest(method, path, query, body).unwrap(),
                pinned,
                "{method} {path}?{query}"
            );
        }
    }

    #[test]
    fn a_path_prefix_and_an_encoded_query_do_not_change_the_action() {
        let bare = digest("POST", "/orders", "", LIMIT_BODY).unwrap();
        assert_eq!(digest("post", "/v1/orders/", "", LIMIT_BODY).unwrap(), bare);
        let path = format!("/orders/{ORDER_ID}");
        assert_eq!(
            digest("DELETE", &path, "market_id=BTC%2DUSDX%2DPERP", "").unwrap(),
            "04e7207aba00c39b57ba72949490c5a76bef60ad4079fc548df4fe622e032eef"
        );
    }

    #[test]
    fn null_signs_as_absent_and_zero_as_its_digits() {
        let nulls = LIMIT_BODY.replace(r#""side":"Buy","#, r#""side":"Buy","stop_price":null,"#);
        assert_eq!(
            digest("POST", "/orders", "", &nulls).unwrap(),
            digest("POST", "/orders", "", LIMIT_BODY).unwrap()
        );
        let zero = LIMIT_BODY.replace(r#""max_slippage_bps":50"#, r#""max_slippage_bps":0"#);
        let absent = LIMIT_BODY.replace(r#","max_slippage_bps":50"#, "");
        assert_ne!(
            digest("POST", "/orders", "", &zero).unwrap(),
            digest("POST", "/orders", "", &absent).unwrap()
        );
    }

    #[test]
    fn preview_and_reads_are_not_trading_routes() {
        assert!(!is_trading_route("POST", "/orders/preview"));
        assert!(!is_trading_route("GET", "/orders"));
        assert!(!is_trading_route("GET", "/leverage"));
        assert!(is_trading_route("DELETE", "/orders"));
        assert_eq!(
            request_digest("POST", "/orders/preview", "", b"{}", &ENVELOPE).unwrap(),
            None
        );
    }

    #[test]
    fn an_empty_optional_field_and_a_missing_market_are_refused() {
        let empty = LIMIT_BODY.replace(r#""client_id":"order-1""#, r#""client_id":"""#);
        assert!(digest("POST", "/orders", "", &empty).is_err());
        assert!(digest("DELETE", "/orders", "market_id=", "").is_err());
        assert!(digest("DELETE", &format!("/orders/{ORDER_ID}"), "", "").is_err());
        assert!(digest("POST", "/leverage", "", r#"{"market_id":"BTC-USDX-PERP"}"#).is_err());
    }
}
