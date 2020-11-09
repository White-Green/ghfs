use std::io::Write;

use clap::{App, Arg, SubCommand};
use regex::Regex;
use reqwest::blocking::Client;

use crate::github_filesystem::http::json_request;
use crate::github_filesystem::mount;
use users::get_current_uid;
use std::fs::Permissions;

mod github_filesystem;

fn to_tree_url(url: &str, client: &Client, username: &str, password: &str) -> Result<String, ()> {
    let regex = Regex::new("^https://api\\.github\\.com/repos/.+/.+/git/trees/[a-f0-9]{40}$").unwrap();
    if regex.is_match(url) {
        return Ok(url.to_string());
    }
    let regex = Regex::new("^https://github.com/([^/]+)/([^/]+)$").unwrap();
    if regex.is_match(url) {
        let url = regex.replace(url, "https://api.github.com/repos/$1/$2").to_string();
        let json = json_request(client, &url, username, password);
        let url = json["branches_url"].unwrap_string().replace("{/branch}", &format!("/{}", json["default_branch"].unwrap_string()));
        let json = json_request(client, &url, username, password);
        return Ok(json["commit"]["commit"]["tree"]["url"].unwrap_string().clone());
    }
    Err(())
}

fn main() {
    // env::set_var("RUST_LOG", "debug");
    env_logger::init();

    let matches = App::new("ghfs")
        .author("White-Green")
        .about("mount GitHub repository into local filesystem.")
        .arg(Arg::with_name("URL")
            .help("github tree URL")
            .takes_value(true)
            .required(true))
        .arg(Arg::with_name("PATH")
            .help("path to directory for mount")
            .takes_value(true)
            .required(true))
        .subcommand(SubCommand::with_name("token")
            .help("manage personal access token")
            .subcommand(SubCommand::with_name("set")
                .arg(Arg::with_name("token")
                    .help("personal access token")
                    .takes_value(true)
                    .required(true)))
            .subcommand(SubCommand::with_name("remove")))
        .get_matches();

    if let Some(token_arg) = matches.subcommand_matches("token") {
        let uid = get_current_uid();
        let path = format!("./{}", uid);
        if let Some(token_set_arg) = token_arg.subcommand_matches("set") {
            // command [ghfs token set]
            std::fs::write(&path, token_set_arg.value_of("token").unwrap())
                .expect("write failed.");
            let permission = <Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600);
            std::fs::set_permissions(&path, permission)
                .expect("chmod failed");
        } else if let Some(token_remove_arg) = token_arg.subcommand_matches("remove") {
            // command [ghfs token remove]
            std::fs::remove_file(&path);
        } else {
            // command [ghfs token]
        }
    }

    print!("GitHub Username: ");
    std::io::stdout().flush().unwrap();
    let mut username = String::new();
    std::io::stdin().read_line(&mut username).unwrap();
    let username = username.trim().to_string();

    let pass = rpassword::prompt_password_stdout("Password: ").unwrap();
    // println!("{}:{}", username, pass);

    let client = Client::new();

    let url = match to_tree_url(&matches.value_of("URL").unwrap(), &client, &username, &pass) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("invalid URL");
            return;
        }
    };
    println!("url:{}", url);
    let path = matches.value_of("PATH").unwrap();
    mount(&url, client, &path, username, pass);
}
