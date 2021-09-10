use std::fs::Permissions;
use std::io::Write;
use std::path::Path;

use clap::{crate_authors, crate_description, crate_name, crate_version, Arg, ArgMatches, SubCommand};
use regex::Regex;
use users::get_current_username;

use crate::github_filesystem::mount;

mod github_filesystem;

fn main() -> anyhow::Result<()> {
    // env::set_var("RUST_LOG", "debug");
    env_logger::init();

    let matches = clap::app_from_crate!()
        .subcommand(SubCommand::with_name("token").about("manage personal access token").subcommand(SubCommand::with_name("set").about("Register access token.")).subcommand(SubCommand::with_name("remove").about("Remove access token.")))
        .subcommand(
            SubCommand::with_name("mount")
                .about("Mount a repository.")
                .arg(Arg::with_name("URL").help("github repository e.g. https://github.com/White-Green/ghfs,\nWhite-Green/ghfs,\nWhite-Green/ghfs?branch=main,\nhttps://github.com/White-Green/ghfs?rev=3639d3315140fea4844df82de71de18f6081c3cc").takes_value(true).required(true))
                .arg(Arg::with_name("PATH").help("path to directory for mount").takes_value(true).required(true)),
        )
        .get_matches();

    match matches.subcommand() {
        ("token", Some(arg)) => token_subcommand(arg)?,
        ("mount", Some(arg)) => {
            let (username, pass) = get_auth_info();

            // println!("{}:{}", username, pass);

            let url = arg.value_of("URL").unwrap();
            let path = arg.value_of("PATH").unwrap();
            std::fs::create_dir_all(path)?;
            mount(&url, &path, &username, &pass)?;
        }
        _ => println!("{}", matches.usage()),
    }

    Ok(())
}

fn get_auth_info() -> (String, String) {
    let path = get_token_file_path();
    if let Ok(s) = std::fs::read_to_string(&path) {
        let regex = Regex::new("^(?P<n>.+)\n(?P<p>.+)\n$").unwrap();
        if let Some(captures) = regex.captures(&s) {
            return (captures.name("n").unwrap().as_str().to_string(), captures.name("p").unwrap().as_str().to_string());
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

fn token_subcommand(token_arg: &ArgMatches) -> anyhow::Result<()> {
    let path = get_token_file_path();
    if let Some(_) = token_arg.subcommand_matches("set") {
        // command [ghfs token set]

        print!("GitHub Username: ");
        std::io::stdout().flush().unwrap();
        let mut username = String::new();
        std::io::stdin().read_line(&mut username).unwrap();
        let username = username.trim().to_string();
        let pass = rpassword::prompt_password_stdout("Token: ").unwrap();

        std::fs::DirBuilder::new().recursive(true).create(<str as AsRef<Path>>::as_ref(&path).parent().unwrap()).expect("Failed to create directories.");

        std::fs::write(&path, format!("{}\n{}\n", username, pass)).expect("Failed to write token.");

        let permission = <Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600);
        std::fs::set_permissions(&path, permission).expect("Failed to chmod.");
    } else if let Some(_) = token_arg.subcommand_matches("remove") {
        // command [ghfs token remove]
        std::fs::remove_file(&path)?;
    } else {
        // command [ghfs token]
        if Path::new(&path).exists() {
            println!("Token is registered.");
        } else {
            println!("Token is not registered.");
        }
    }
    Ok(())
}

fn get_token_file_path() -> String {
    let username = get_current_username().unwrap();
    format!("/home/{}/.config/ghfs/token", username.to_str().unwrap())
}
