//! Client-side request-signing benchmark (ENG-15689).
//!
//! Measures only the signer — no HTTP, no serialization of the order — for the
//! two request-auth schemes the API ships, on one fixed order-placement
//! request:
//!
//! - `hmac`  — `Credentials::ApiKey` (`hmacAuth`): HMAC-SHA256 over the
//!   canonical string, secret hex-decoded per call exactly as the client does.
//! - `agent` — `AgentSigner` (`agentAuth`): secp256k1 ECDSA over keccak256 of
//!   the canonical string, low-S, 65-byte `r||s||v`. A fresh nonce is issued on
//!   every iteration, as on a real write.
//!
//! Two passes: criterion's own estimate, then an individually-timed pass that
//! prints p50 / p95 per signature and signatures/sec, in the same format as the
//! TypeScript and Python SDK benches so the numbers line up.
//!
//! Run: `cargo bench --bench signing`

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, Criterion};
use nexus_exchange::auth::SigningContext;
use nexus_exchange::{AgentSigner, Credential, Credentials};

// Shared fixture — identical bytes in all three SDK benches.
const METHOD: &str = "POST";
const PATH: &str = "/api/v1/orders";
const QUERY: &str = "";
const BODY: &[u8] = br#"{"market_id":"BTC-USDX-PERP","side":"Buy","order_type":"Limit","price":"50000","quantity":"0.1","time_in_force":"GTC","client_order_id":"bench-0000000001"}"#;
const TIMESTAMP_MS: u64 = 1_776_033_900_000;
const HMAC_KEY_ID: &str = "nx_bench";
const HMAC_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
// Well-known public test key — never fund it.
const AGENT_KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

const WARMUP: Duration = Duration::from_secs(2);
const SAMPLES: usize = 20_000;

fn ctx() -> SigningContext<'static> {
    SigningContext::new(METHOD, PATH, QUERY, BODY, TIMESTAMP_MS)
}

fn signers() -> [(&'static str, Box<dyn Credential>); 2] {
    [
        (
            "hmac",
            Box::new(Credentials::api_key(HMAC_KEY_ID, HMAC_SECRET)),
        ),
        ("agent", Box::new(AgentSigner::from_hex(AGENT_KEY).unwrap())),
    ]
}

fn criterion_bench(c: &mut Criterion) {
    let ctx = ctx();
    for (name, signer) in signers() {
        c.bench_function(&format!("sign/{name}"), |b| {
            b.iter(|| black_box(signer.auth_headers(black_box(&ctx)).unwrap()))
        });
    }
}

/// Time `SAMPLES` individual signatures after a warm-up and print percentiles.
fn percentile_pass() {
    let ctx = ctx();
    for (name, signer) in signers() {
        let start = Instant::now();
        while start.elapsed() < WARMUP {
            black_box(signer.auth_headers(&ctx).unwrap());
        }
        let mut ns: Vec<u64> = Vec::with_capacity(SAMPLES);
        let wall = Instant::now();
        for _ in 0..SAMPLES {
            let t = Instant::now();
            black_box(signer.auth_headers(black_box(&ctx)).unwrap());
            ns.push(t.elapsed().as_nanos() as u64);
        }
        let wall = wall.elapsed();
        ns.sort_unstable();
        let pct = |p: f64| ns[((ns.len() as f64 * p) as usize).min(ns.len() - 1)] as f64 / 1e3;
        println!(
            "RESULT sdk=rs scheme={name} n={SAMPLES} p50_us={:.2} p95_us={:.2} sig_per_s={:.0}",
            pct(0.50),
            pct(0.95),
            SAMPLES as f64 / wall.as_secs_f64(),
        );
    }
}

criterion_group!(benches, criterion_bench);

fn main() {
    benches();
    Criterion::default().configure_from_args().final_summary();
    percentile_pass();
}
