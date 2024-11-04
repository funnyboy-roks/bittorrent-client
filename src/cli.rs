use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Parser)]
pub struct Cli {
    #[clap(subcommand)]
    pub subcommand: SubCmd,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SubCmd {
    Decode { string: String },
    Info { path: PathBuf },
    Peers { path: PathBuf },
    Handshake { path: PathBuf, addr: SocketAddr },
}
