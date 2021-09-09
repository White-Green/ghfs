use std::fmt::Debug;
use std::time::Instant;

use reqwest::Client;
use serde::de::DeserializeOwned;

use crate::github_filesystem::APP_NAME;

pub(crate) struct HTTPWrapper {
    client: Client,
    username: String,
    password: String,
}

impl HTTPWrapper {
    pub(crate) fn new(client: Client, username: &str, password: &str) -> Self {
        HTTPWrapper { client, username: username.to_string(), password: password.to_string() }
    }

    pub(crate) async fn request<T: DeserializeOwned + Debug>(&self, url: &str) -> Result<T, reqwest::Error> {
        let instant = Instant::now();
        let result = self.client.get(url).header("User-Agent", APP_NAME).basic_auth(&self.username, Some(&self.password)).send().await?.json().await;
        log::debug!("request for {} latency: {} μs", url, instant.elapsed().as_micros());
        result
    }
}
