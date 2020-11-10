use std::fs::Permissions;
use std::io::{Error, Write};
use std::path::Path;

use clap::{App, Arg, ArgMatches, SubCommand};
use regex::Regex;
use reqwest::blocking::Client;
use users::get_current_uid;

use crate::github_filesystem::http::json_request;
use crate::github_filesystem::mount;

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
        .subcommand(SubCommand::with_name("token")
            .about("manage personal access token")
            .subcommand(SubCommand::with_name("set")
                .about("Register access token."))
            .subcommand(SubCommand::with_name("remove")
                .about("Remove access token.")))
        .subcommand(SubCommand::with_name("mount")
            .about("Mount a repository.")
            .arg(Arg::with_name("URL")
                .help("github repository URL")
                .takes_value(true)
                .required(true))
            .arg(Arg::with_name("PATH")
                .help("path to directory for mount")
                .takes_value(true)
                .required(true))
        )
        .get_matches();

    if let Some(token_arg) = matches.subcommand_matches("token") {
        token_subcommand(token_arg);
        return;
    }
    let matches = if let Some(matches) = matches.subcommand_matches("mount") {
        matches
    } else {
        println!("{}", matches.usage());
        return;
    };

    let (username, pass) = get_auth_info();

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

fn get_auth_info() -> (String, String) {
    let path = get_token_file_path();
    if let Ok(s) = std::fs::read_to_string(&path) {
        let regex = Regex::new("^(?P<n>.+)\n(?P<p>.+)\n$").unwrap();
        if let Some(t) = regex.captures(&s) {
            return (t["n"].to_string(), t["p"].to_string());
        }
    }
    print!("GitHub Username: ");
    std::io::stdout().flush().unwrap();
    let mut username = String::new();
    std::io::stdin().read_line(&mut username).unwrap();
    let username = username.trim().to_string();

    let pass = rpassword::prompt_password_stdout("Password or Token: ").unwrap();
    (username, pass)
}

fn token_subcommand(token_arg: &ArgMatches) {
    let path = get_token_file_path();
    if let Some(_) = token_arg.subcommand_matches("set") {
        // command [ghfs token set]

        print!("GitHub Username: ");
        std::io::stdout().flush().unwrap();
        let mut username = String::new();
        std::io::stdin().read_line(&mut username).unwrap();
        let username = username.trim().to_string();
        let pass = rpassword::prompt_password_stdout("Token: ").unwrap();

        std::fs::write(&path, format!("{}\n{}\n", username, pass))
            .expect("write failed.");

        let permission = <Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600);
        std::fs::set_permissions(&path, permission)
            .expect("chmod failed");
    } else if let Some(_) = token_arg.subcommand_matches("remove") {
        // command [ghfs token remove]
        std::fs::remove_file(&path);
    } else {
        // command [ghfs token]
        if Path::new(&path).exists() {
            println!("Token is registered.");
        } else {
            println!("Token is not registered.");
        }
    }
}

fn get_token_file_path() -> String {
    let uid = get_current_uid();
    format!("./{}", uid)
}
