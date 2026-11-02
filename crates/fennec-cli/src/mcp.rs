//! `fennec mcp`: the MCP server over stdio, for a client that starts its
//! tools as processes -- Claude Code's and Cursor's `command`, Claude
//! Desktop's configuration.
//!
//! Over a file (`--file`) the session holds the database itself, as the
//! native library does: it takes `<file>.lock`, so a server or an app over
//! the same file refuses it and it refuses them, and each write is fsynced
//! before it is answered. `--read-only` takes no lock and reads the file
//! as it stood when the session began, beside a server writing it. Over a
//! server (`--url`) it posts each message to the server's endpoint, where
//! the tools run under the server's options and the token's rules: the
//! token comes from the environment, `FENNEC_TOKEN`, never from argv, which
//! `ps` shows.

use fennec_http::mcp;
use std::io;
use std::sync::{Arc, RwLock};

const USAGE: &str = "\
usage: fennec mcp --file <path> [options]
       fennec mcp --url <http://host:port/_mcp>

MCP, the Model Context Protocol, over stdio: a message a line on stdin, an
answer a line on stdout, logs on stderr.

  --file <path>           the database: made when missing, locked while the
                          session lasts (<path>.lock), each write fsynced
    --read-only           take no lock and write nothing: the file as it
                          stood when the session began, beside a server
    --write               the write tool: put, insert, set and del
    --memory <collection> remember, recall and forget, in this collection
    --max-rows <n>        the most rows a tool answers; past it the call is
                          refused, never cut  default: 100
    --max-bytes <size>    the most bytes a tool answers  default: 64K
  --url <url>             a server started with --mcp: its endpoint,
                          http://127.0.0.1:8080/_mcp, or /t/<tenant>/_mcp. The
                          tools are the server's. https:// is dialed over TLS
    --tls-ca <pem>        the CAs an https:// endpoint is held to, in place
                          of Mozilla's roots
    --token-env <name>    the environment variable holding the token
                          default: FENNEC_TOKEN

Claude Code: claude mcp add fennecdb -- fennec mcp --file notes.fennec --memory memories
";

pub fn main(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut url: Option<String> = None;
    let mut read_only = false;
    let mut token_env = String::from("FENNEC_TOKEN");
    // A local session's agent is the file's one caller: a bound would
    // only stop it.
    let mut opts = mcp::Options {
        calls_per_sec: 0,
        ..mcp::Options::default()
    };
    let mut local_flag: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = || -> Option<String> {
            i += 1;
            args.get(i).cloned()
        };
        let ok = match flag {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "--file" => value().map(|v| file = Some(v)).is_some(),
            "--url" => value().map(|v| url = Some(v)).is_some(),
            "--tls-ca" => match value() {
                Some(v) => match fennec_http::dial::trust_ca(&v) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("fennec mcp: {e}");
                        return 2;
                    }
                },
                None => false,
            },
            "--token-env" => value().map(|v| token_env = v).is_some(),
            "--read-only" => {
                read_only = true;
                true
            }
            "--write" => {
                local_flag = Some("--write");
                opts.write = true;
                true
            }
            "--memory" => {
                local_flag = Some("--memory");
                value().map(|v| opts.memory = Some(v)).is_some()
            }
            "--max-rows" => {
                local_flag = Some("--max-rows");
                value()
                    .and_then(|v| v.parse::<usize>().ok().filter(|n| *n > 0))
                    .map(|n| opts.max_rows = n.min(10_000))
                    .is_some()
            }
            "--max-bytes" => {
                local_flag = Some("--max-bytes");
                value()
                    .and_then(|v| fennec_http::quota::size(&v).filter(|n| *n > 0))
                    .map(|n| opts.max_bytes = n as usize)
                    .is_some()
            }
            other => {
                eprintln!("fennec mcp: unknown option `{other}`\n\n{USAGE}");
                return 2;
            }
        };
        if !ok {
            eprintln!("fennec mcp: {flag} expects a value\n\n{USAGE}");
            return 2;
        }
        i += 1;
    }
    if let Some(m) = &opts.memory {
        let ok = m
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && m.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !ok {
            eprintln!("fennec mcp: --memory names a collection, and `{m}` is not a name");
            return 2;
        }
    }
    match (file, url) {
        (Some(path), None) => local(&path, read_only, opts),
        (None, Some(url)) => {
            if let Some(f) = local_flag {
                eprintln!(
                    "fennec mcp: {f} is the server's to set (fennec-server --mcp ...): over --url the tools are its own"
                );
                return 2;
            }
            remote(&url, &token_env)
        }
        _ => {
            eprintln!("fennec mcp: give --file or --url\n\n{USAGE}");
            2
        }
    }
}

fn local(path: &str, read_only: bool, opts: mcp::Options) -> i32 {
    if read_only && opts.write {
        eprintln!("fennec mcp: --write and --read-only: pick one");
        return 2;
    }
    let lock = match read_only {
        true => None,
        false => match crate::held::take(path, crate::held::Way::Mcp) {
            Ok(l) => Some(l),
            Err(why) => {
                eprintln!("fennec mcp: {why}");
                return 1;
            }
        },
    };
    let opened = match read_only {
        true => fennec_core::fs::open_read_only(path),
        false => fennec_core::fs::open(path),
    };
    let db = match opened {
        Ok(db) => Arc::new(RwLock::new(db)),
        Err(e) => {
            eprintln!("fennec mcp: could not open {path}: {e}");
            return 1;
        }
    };
    let cfg = fennec_http::Config {
        read_only,
        // An agent's write is answered once it is on the disk, as the
        // native library answers an app's.
        sync_on_write: !read_only,
        ..fennec_http::Config::default()
    };
    let who = fennec_http::access::Who::Full;
    let ctx = mcp::Ctx {
        db: &db,
        cfg: &cfg,
        who: &who,
        opts: &opts,
        run: mcp::Run::local(),
    };
    eprintln!(
        "fennec mcp {}: {path}{}, over stdio",
        fennec_core::VERSION,
        if read_only { ", read only" } else { "" }
    );
    let served = mcp::serve_lines(&ctx, io::stdin().lock(), io::stdout().lock());
    let mut code = match served {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fennec mcp: {e}");
            1
        }
    };
    if !read_only {
        let mut g = db.write().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = g.sync() {
            eprintln!("fennec mcp: could not push the writes to disk: {e}");
            code = 1;
        }
    }
    drop(lock);
    code
}

fn remote(url: &str, token_env: &str) -> i32 {
    let token = std::env::var(token_env).ok().filter(|t| !t.is_empty());
    let up = match mcp::bridge::Upstream::parse(url, token) {
        Ok(up) => up,
        Err(e) => {
            eprintln!("fennec mcp: {e}");
            return 2;
        }
    };
    eprintln!(
        "fennec mcp {}: {url}{}, over stdio",
        fennec_core::VERSION,
        if up.token.is_some() {
            ""
        } else {
            " with no token"
        }
    );
    match mcp::bridge::bridge(&up, io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fennec mcp: {e}");
            1
        }
    }
}
