//! Local launcher control files and authenticated, bounded readiness probes.
use crate::{
    error::{Error, Result},
    json,
};
use futures_util::StreamExt;
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};
use zeroize::Zeroizing;

pub fn private_file(path: &Path, expected_uid: u32, maximum: usize) -> Result<Zeroizing<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::RuntimeBlocked)?;
    let metadata = file.metadata().map_err(|_| Error::RuntimeBlocked)?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || (metadata.uid() != 0 && metadata.uid() != expected_uid)
        || metadata.len() > maximum as u64
    {
        return Err(Error::RuntimeBlocked);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::RuntimeBlocked)?;
    if bytes.len() > maximum {
        return Err(Error::ResourceLimit);
    }
    Ok(bytes)
}
pub fn token(path: &Path, expected_uid: u32) -> Result<Zeroizing<String>> {
    let raw = private_file(path, expected_uid, 8192)?;
    let token = std::str::from_utf8(&raw)
        .map_err(|_| Error::RuntimeBlocked)?
        .trim();
    if !(24..=512).contains(&token.len()) || !token.bytes().all(|b| (33..=126).contains(&b)) {
        return Err(Error::RuntimeBlocked);
    }
    Ok(Zeroizing::new(token.into()))
}
pub async fn readiness_document(port: u16, token: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| Error::RuntimeBlocked)?;
    let response = client
        .get(format!("http://127.0.0.1:{port}/ready"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| Error::RuntimeBlocked)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::RuntimeBlocked);
    }
    let mut source = response.bytes_stream();
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = source.next().await {
        let chunk = chunk.map_err(|_| Error::RuntimeBlocked)?;
        if bytes.len() + chunk.len() > 8192 {
            return Err(Error::ResourceLimit);
        }
        bytes.extend_from_slice(&chunk);
    }
    let document = json::from_slice(&bytes)?;
    if !document.is_object() {
        return Err(Error::RuntimeBlocked);
    }
    Ok(document)
}
