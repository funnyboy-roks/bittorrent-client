use ::reqwest::Url;
use anyhow::Context;
use clap::Parser;
use cli::{Cli, SubCmd};
use core::str;
use decode::{decode, Decoded};
use peer::Client;
use rand::{Rng, RngCore};
use reqwest::blocking as reqwest;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    str::FromStr,
};

pub mod cli;
pub mod decode;
pub mod peer;

#[derive(Debug, Clone, Deserialize)]
pub struct PeersResponse {
    pub interval: usize,
    pub peers: Vec<u8>,
}

impl PeersResponse {
    pub fn peers(&self) -> impl Iterator<Item = SocketAddr> + use<'_> {
        self.peers.chunks_exact(6).map(|chunk| {
            let (ip, port) = chunk.split_at(4);
            let [a, b, c, d] = ip.try_into().unwrap();
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
                u16::from_be_bytes(port.try_into().unwrap()),
            )
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorrentInfo {
    pub length: usize,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pub pieces: Vec<u8>,
}

impl TorrentInfo {
    fn pieces(&self) -> impl Iterator<Item = &[u8]> {
        self.pieces.chunks_exact(20)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Torrent {
    pub announce: String,
    pub info: TorrentInfo,
}

pub fn serde<S, D>(s: &S) -> anyhow::Result<D>
where
    S: Serialize,
    D: DeserializeOwned,
{
    Ok(
        serde_json::from_str(&serde_json::to_string(s).context("serializing value")?)
            .context("deserializing value")?,
    )
}

fn get_peers(data: &Torrent, info_hash: [u8; 20]) -> anyhow::Result<Vec<SocketAddr>> {
    let mut url = Url::from_str(&data.announce)?;
    url.query_pairs_mut()
        .append_pair("info_hash", unsafe { str::from_utf8_unchecked(&info_hash) })
        .append_pair("peer_id", "20 chars is too shor")
        .append_pair("port", "6881")
        .append_pair("uploaded", "0")
        .append_pair("downloaded", "0")
        .append_pair("left", &data.info.length.to_string())
        .append_pair("compact", "1");
    let res = reqwest::get(url)?;
    let text = res.bytes()?;
    let (_, res) = decode(&text).unwrap();
    let res: PeersResponse = serde(&res)?;

    Ok(res.peers().collect())
}

fn get_info_hash(value: &Decoded<'_>) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(value["info"].source.unwrap());
    hasher.finalize().into()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.subcommand {
        SubCmd::Decode { string } => {
            let (_, value) = decode(&string.as_bytes()).unwrap();
            println!("{}", serde_json::to_string(&value)?);
            let mut vec = Vec::new();
            value.encode(&mut vec)?;
            eprintln!("{}", std::str::from_utf8(&vec)?);
        }
        SubCmd::DecodeFile { torrent_file: path } => {
            let file = std::fs::read(path)?;
            let (_, value) = decode(&file).unwrap();
            println!("{}", value);
        }
        SubCmd::Info { torrent_file: path } => {
            let file = std::fs::read(path)?;
            let (_, value) = decode(&file).unwrap();
            let info_hash = get_info_hash(&value);
            let data: Torrent = serde(&value)?;

            println!("Tracker URL: {}", data.announce);
            println!("Length: {}", data.info.length);
            println!("Info Hash: {}", hex::encode(info_hash));
            println!("Piece Length: {}", data.info.piece_length);
            println!("Piece Hashes:");
            for piece in data.info.pieces() {
                println!("{}", hex::encode(piece));
            }
        }
        SubCmd::Peers { torrent_file: path } => {
            let file = std::fs::read(path)?;
            let (_, value) = decode(&file).unwrap();

            let data: Torrent = serde(&value)?;
            let info_hash = get_info_hash(&value);

            eprintln!("Tracker URL: {}", data.announce);
            eprintln!("Length: {}", data.info.length);
            eprintln!("Info Hash: {}", hex::encode(info_hash));
            eprintln!("Piece Length: {}", data.info.piece_length);
            eprintln!("Piece Hashes:");
            for piece in data.info.pieces() {
                eprintln!("{}", hex::encode(piece));
            }

            let peers = get_peers(&data, info_hash.into())?;

            for peer in peers {
                println!("{}", peer);
            }
        }
        SubCmd::Handshake {
            torrent_file: path,
            addr,
        } => {
            let file = std::fs::read(path)?;
            let (_, value) = decode(&file).unwrap();
            let data: Torrent = serde(&value)?;
            let info_hash = get_info_hash(&value);

            for piece in data.info.pieces() {
                eprintln!("{}", hex::encode(piece));
            }

            let handler = Client::connect(addr, data, info_hash);
            // eprintln!("Peer ID: {}", hex::encode(peer_id));
        }
        SubCmd::DownloadPiece {
            out,
            torrent_file,
            index,
        } => {
            let file = std::fs::read(torrent_file)?;
            let (_, value) = decode(&file).unwrap();
            let data: Torrent = serde(&value)?;
            let info_hash = get_info_hash(&value);

            let piece = data.info.pieces().nth(index);

            let peers = get_peers(&data, info_hash.into())?;
            // let peer = peers[rand::thread_rng().gen_range(0..peers.len())];

            let handler = Client::connect(peers[0], data, info_hash);
        }
    }
    Ok(())
}
