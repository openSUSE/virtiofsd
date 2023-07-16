use clap::Parser;
use std::process;
use virtiofsd::idmap::*;

mod sandbox;

/// virtiofsd sandboxing tool
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// virtiofsd executable path
    #[arg(long)]
    virtiofsd_path: String,

    /// vhost-user socket path
    #[arg(long)]
    socket_path: String,

    /// Shared directory path
    #[arg(long)]
    shared_dir: String,

    /// UID mapping
    #[arg(long)]
    uid_map: Option<UidMap>,

    /// GID mapping
    #[arg(long)]
    gid_map: Option<GidMap>,

    /// Arguments to virtiofsd
    #[arg(last = true)]
    virtiofsd_args: Vec<String>,
}

fn main() {
    init_logging();

    let args = Args::parse();
    if args.virtiofsd_args.contains(&"--shared-dir".to_owned()) {
        println!("ERROR: --shared-dir argument to virtiofsd must be omitted.");
        process::exit(1);
    }

    sandbox::Sandbox::new(
        args.virtiofsd_path,
        args.socket_path,
        args.shared_dir,
        args.uid_map,
        args.gid_map,
        args.virtiofsd_args,
    )
    .enter()
    .unwrap_or_else(|error| {
        println!("Error entering sandbox: {}", error);
        process::exit(1)
    });
}

fn init_logging() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();
}
