use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Parser)]
pub struct Cli {
    #[clap(subcommand)]
    pub subcommand: SubCmd,
}

#[derive(Debug, Clone, Subcommand)]
#[clap(rename_all = "snake_case")]
pub enum SubCmd {
    Decode {
        string: String,
    },
    Info {
        torrent_file: PathBuf,
    },
    Peers {
        torrent_file: PathBuf,
    },
    Handshake {
        torrent_file: PathBuf,
        addr: SocketAddr,
    },
    DownloadPiece {
        #[clap(short)]
        out: PathBuf,
        torrent_file: PathBuf,
        index: usize,
    },
}
