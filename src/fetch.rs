use regex::Regex;
use std::io::Read as _;
use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

const BASE_URL: &str = "https://miniblox.io";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let resp = agent.get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut s = String::new();
    reader.read_to_string(&mut s).map_err(|e| format!("read {url}: {e}"))?;
    Ok(s)
}

/// Fetch the current entry bundle from miniblox.io.
///
/// Returns `(asset_hash, source)`, e.g. `("C-KDXQfU", "...")`.
pub fn fetch_current() -> Result<(String, String), String> {
    let agent = agent();
    let html = get(&agent, BASE_URL)?;
    let re = Regex::new(r#"assets/index-([A-Za-z0-9_-]+)\.js"#).unwrap();
    let caps = re
        .captures(&html)
        .ok_or_else(|| "could not find assets/index-*.js in miniblox.io index".to_string())?;
    let hash = caps[1].to_string();
    let url = format!("{BASE_URL}/assets/index-{hash}.js");
    let source = get(&agent, &url)?;
    Ok((hash, source))
}
