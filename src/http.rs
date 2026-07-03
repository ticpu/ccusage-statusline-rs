use anyhow::Result;
use reqwest::blocking::Client;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 5;

pub fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(Into::into)
}
