use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};
use std::io::Read;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 5;

pub fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(Into::into)
}

/// Read a response body, refusing anything larger than `max_bytes`.
///
/// The upstream files only ever grow, and a timeout alone does not bound how much a
/// server can hand a statusline that has to render in milliseconds.
pub fn read_body_limited(response: Response, max_bytes: u64) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length()
        && len > max_bytes
    {
        anyhow::bail!("response is {len} bytes, over the {max_bytes} byte limit");
    }

    let mut buf = Vec::new();
    response
        .take(max_bytes + 1)
        .read_to_end(&mut buf)
        .context("Failed to read response body")?;

    if buf.len() as u64 > max_bytes {
        anyhow::bail!("response exceeded the {max_bytes} byte limit");
    }
    Ok(buf)
}
