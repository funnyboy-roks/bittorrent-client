use ::reqwest::Url;
use anyhow::Context;
use core::str;
use decode::{decode_bencoded_value, Decoded};
use rand::RngCore;
use reqwest::blocking as reqwest;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    env,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    str::FromStr,
};

pub mod decode;

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

fn get_peers(data: Torrent, info_hash: [u8; 20]) -> anyhow::Result<Vec<SocketAddr>> {
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
    let (_, res) = decode_bencoded_value(&text).unwrap();
    let res: PeersResponse = serde(&res)?;

    Ok(res.peers().collect())
}

fn perform_handshake(addr: SocketAddr, info_hash: [u8; 20]) -> anyhow::Result<[u8; 20]> {
    let mut tcp = TcpStream::connect(addr)?;
    let prot_str = b"BitTorrent protocol";
    tcp.write_all(&[prot_str.len() as u8])?;
    tcp.write_all(prot_str)?;
    tcp.write_all(&[0; 8])?;
    tcp.write_all(&info_hash)?;
    let mut my_id = [0; 20];
    rand::thread_rng().fill_bytes(&mut my_id);
    tcp.write_all(&my_id)?;
    eprintln!("my_id = {:02x?}", my_id);

    let mut buf = [0; 20];
    tcp.read_exact(&mut buf)?;
    assert_eq!(buf[0], 19);
    assert_eq!(buf[1..], *prot_str);
    let mut buf = [0; 8];
    tcp.read_exact(&mut buf)?;
    let mut buf = [0; 20];
    tcp.read_exact(&mut buf)?;
    assert_eq!(&buf[..], &info_hash[..]);
    let mut peer_id = [0; 20];
    tcp.read_exact(&mut peer_id)?;
    eprintln!("Peer ID: {}", hex::encode(peer_id));

    Ok(peer_id)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    match &*args[1] {
        "decode" => {
            let (_, value) = decode_bencoded_value(&args[2].as_bytes()).unwrap();
            println!("{}", serde_json::to_string(&value)?);
        }
        "info" => {
            let file = std::fs::read(&args[2])?;
            let (_, value) = decode_bencoded_value(&file).unwrap();

            let data: Torrent = serde(&value)?;

            println!("Tracker URL: {}", data.announce);
            println!("Length: {}", data.info.length);
            let mut hasher = Sha1::new();
            hasher.update(value["info"].source);
            let info_hash = hasher.finalize();
            println!("Info Hash: {}", hex::encode(info_hash));
            println!("Piece Length: {}", data.info.piece_length);
            println!("Piece Hashes:");
            for piece in data.info.pieces() {
                println!("{}", hex::encode(piece));
            }
        }
        "peers" => {
            let file = std::fs::read(&args[2])?;
            let (_, value) = decode_bencoded_value(&file).unwrap();

            let data: Torrent = serde(&value)?;

            eprintln!("Tracker URL: {}", data.announce);
            eprintln!("Length: {}", data.info.length);
            let mut hasher = Sha1::new();
            hasher.update(value["info"].source);
            let info_hash = hasher.finalize();
            eprintln!("Info Hash: {}", hex::encode(info_hash));
            eprintln!("Piece Length: {}", data.info.piece_length);
            eprintln!("Piece Hashes:");
            for piece in data.info.pieces() {
                eprintln!("{}", hex::encode(piece));
            }

            let peers = get_peers(data, info_hash.into())?;

            for peer in peers {
                println!("{}", peer);
            }
        }
        "handshake" => {
            let file = std::fs::read(&args[2])?;
            let addr: SocketAddr = args[3].parse()?;
            let (_, value) = decode_bencoded_value(&file).unwrap();
            let data: Torrent = serde(&value)?;
            let mut hasher = Sha1::new();
            hasher.update(value["info"].source);
            let info_hash = hasher.finalize();

            for piece in data.info.pieces() {
                eprintln!("{}", hex::encode(piece));
            }

            let peer_id = perform_handshake(addr, info_hash.into())?;
            eprintln!("Peer ID: {}", hex::encode(peer_id));
        }
        command => {
            panic!("Unknown command '{}'", command)
        }
    }
    Ok(())
}
