// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath admin …`: bootstrap and recovery commands. They act as the
//! system, run non-interactively, and print JSON with `--output json`.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::json;
use slath_core::Secret;
use slath_core::authz::Subject;
use slath_service::{ServiceError, Services};

use crate::config;
use crate::output::{OutputFormat, fail, print_json, runtime};

/// Opens the database and builds the services for one command.
pub async fn services(path: &Path) -> Result<Services, String> {
    let cfg = config::load(path)?;
    let url = config::database_url(&cfg)?;
    let mut pc = slath_db::PoolConfig::new(url);
    pc.max_connections = 2;
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    slath_db::check_schema(&pool)
        .await
        .map_err(|e| e.to_string())?;
    // Commands don't issue CSRF tokens; any key will do.
    let key = slath_auth::MasterKey::generate().map_err(|e| e.to_string())?;
    let home = cfg.storage.data_dir.join("git-home");
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let runner = slath_git::Runner::new(slath_git::RunnerConfig::new(cfg.git.binary.clone(), home))
        .map_err(|e| e.to_string())?;
    Ok(Services::new(pool, Arc::new(cfg.cfg), &key, runner))
}

/// A readable description of a service error for the terminal.
pub fn describe(e: &ServiceError) -> String {
    match e {
        ServiceError::Invalid(errors) => errors
            .iter()
            .map(|f| format!("{}: {}", f.field, f.message))
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

/// Generates a random password: 24 characters from an unambiguous alphabet.
fn random_password() -> Result<String, String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = slath_auth::token::random_bytes::<24>().map_err(|e| e.to_string())?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect())
}

pub struct CreateUser {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub admin: bool,
    pub password_stdin: bool,
}

pub fn create_user(path: &Path, args: CreateUser, format: OutputFormat) -> ExitCode {
    let (password, generated) = if args.password_stdin {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            return fail(format, &format!("cannot read the password from stdin: {e}"));
        }
        (s.trim_end_matches(['\r', '\n']).to_owned(), false)
    } else {
        match random_password() {
            Ok(p) => (p, true),
            Err(e) => return fail(format, &e),
        }
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        svc.create_account(
            &Subject::System,
            slath_service::accounts::NewAccount {
                username: args.username.clone(),
                email: args.email.clone(),
                display_name: args.display_name.clone(),
                password: Some(Secret::new(password.clone())),
                is_admin: args.admin,
            },
        )
        .await
        .map_err(|e| describe(&e))
    });
    match result {
        Ok(id) => {
            match format {
                OutputFormat::Text => {
                    println!(
                        "created user {} ({id}){}",
                        args.username,
                        if args.admin {
                            " as site administrator"
                        } else {
                            ""
                        }
                    );
                    if generated {
                        println!("password: {password}");
                        println!("Store it now; it can't be shown again.");
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "user": {"id": id.to_string(), "username": args.username, "admin": args.admin},
                    "password": if generated { Some(password) } else { None },
                })),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}
