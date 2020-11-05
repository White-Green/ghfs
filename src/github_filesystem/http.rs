use json_flex::JFObject;
use reqwest::blocking::Client;

use crate::github_filesystem::APP_NAME;

pub(crate) fn json_request(client: &Client, url: &str, username: &str, password: &str) -> Box<JFObject> {
    let json = client.get(url)
        .header("User-Agent", APP_NAME)
        .basic_auth(username, Some(password))
        .send().unwrap()
        .text().unwrap();
    log::debug!("request {} => {}", url, &json);
    json_flex::decode(json)
}
