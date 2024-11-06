use anyhow::Context;
use clap::Parser;
use cli::{Cli, SubCmd};
use core::str;
use decode::{decode, Decoded};
use peer::{Client, Piece};
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    io::SeekFrom,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    str::FromStr,
};
use tokio::{
    fs::File,
    io::{AsyncSeek, AsyncSeekExt, AsyncWriteExt},
    sync::oneshot,
    task::JoinSet,
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
    pub length: u32,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: u32,
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

impl Torrent {
    pub async fn read_file<P>(path: P) -> anyhow::Result<([u8; 20], Self)>
    where
        P: AsRef<Path>,
    {
        let file = tokio::fs::read(path).await?;
        let (_, value) = decode(&file).unwrap();
        let info_hash = get_info_hash(&value);
        Ok((info_hash, serde(&value)?))
    }
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

async fn get_peers(data: &Torrent, info_hash: [u8; 20]) -> anyhow::Result<Vec<SocketAddr>> {
    let mut url = Url::from_str(&data.announce)?;
    url.query_pairs_mut()
        .append_pair("info_hash", unsafe { str::from_utf8_unchecked(&info_hash) })
        .append_pair("peer_id", "20 chars is too shor")
        .append_pair("port", "6881")
        .append_pair("uploaded", "0")
        .append_pair("downloaded", "0")
        .append_pair("left", &data.info.length.to_string())
        .append_pair("compact", "1");
    let res = reqwest::get(url).await?;
    let text = res.bytes().await?;
    let (_, res) = decode(&text).unwrap();
    let res: PeersResponse = serde(&res)?;

    Ok(res.peers().collect())
}

fn get_info_hash(value: &Decoded<'_>) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(value["info"].source.unwrap());
    hasher.finalize().into()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
            let file = tokio::fs::read(path).await?;
            let (_, value) = decode(&file).unwrap();
            println!("{}", value);
        }
        SubCmd::Info { torrent_file } => {
            let (info_hash, data) = Torrent::read_file(torrent_file).await?;

            println!("Tracker URL: {}", data.announce);
            println!("Length: {}", data.info.length);
            println!("Info Hash: {}", hex::encode(info_hash));
            println!("Piece Length: {}", data.info.piece_length);
            println!("Piece Hashes:");
            for piece in data.info.pieces() {
                println!("{}", hex::encode(piece));
            }
        }
        SubCmd::Peers { torrent_file } => {
            let (info_hash, data) = Torrent::read_file(torrent_file).await?;

            eprintln!("Tracker URL: {}", data.announce);
            eprintln!("Length: {}", data.info.length);
            eprintln!("Info Hash: {}", hex::encode(info_hash));
            eprintln!("Piece Length: {}", data.info.piece_length);
            eprintln!("Piece Hashes:");
            for piece in data.info.pieces() {
                eprintln!("{}", hex::encode(piece));
            }

            let peers = get_peers(&data, info_hash.into()).await?;

            for peer in peers {
                println!("{}", peer);
            }
        }
        SubCmd::Handshake { torrent_file, addr } => {
            let (info_hash, data) = Torrent::read_file(torrent_file).await?;

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
            let (info_hash, data) = Torrent::read_file(torrent_file).await?;

            let piece_length = if data.info.pieces().count() as u32 - 1 == index {
                data.info.length % data.info.piece_length
            } else {
                data.info.piece_length
            };

            let peers = get_peers(&data, info_hash.into()).await?;
            // let peer = peers[rand::thread_rng().gen_range(0..peers.len())];

            let mut handler = Client::connect(peers[0], data, info_hash).await?;

            let mut set = JoinSet::new();
            let mut pieces = Vec::with_capacity(piece_length.div_ceil(2 << 13) as usize);
            for begin in (0..piece_length).step_by(2 << 13) {
                let length = std::cmp::min(piece_length - begin, 2 << 13);
                let piece = Piece {
                    index,
                    begin,
                    length,
                };
                eprintln!("Requesting piece {:?}", piece);
                let (tx, rx) = oneshot::channel();
                pieces.push((piece, tx));
                set.spawn(rx);
            }

            let mut file = File::create(out).await?;
            if !handler.request_pieces(pieces).await? {
                while let Some(res) = set.join_next().await {
                    if let Some(piece) = res?? {
                        file.seek(SeekFrom::Start(piece.begin.into()))
                            .await
                            .context("seeking in file")?;
                        file.write(&piece.block).await.context("writing in file")?;
                    }
                }
            }
        }
        SubCmd::DownloadFile { out, torrent_file } => {
            let (info_hash, data) = Torrent::read_file(torrent_file).await?;

            let data_piece_length = data.info.piece_length;
            let peers = get_peers(&data, info_hash.into()).await?;
            let mut handler = Client::connect(peers[0], data.clone(), info_hash).await?;

            let mut set = JoinSet::new();
            let mut pieces = Vec::new();
            for (index, _) in (0..data.info.length)
                .step_by(data.info.piece_length as usize)
                .enumerate()
            {
                let index = index as u32;

                let piece_length = if data.info.pieces().count() as u32 - 1 == index {
                    data.info.length % data_piece_length
                } else {
                    data.info.piece_length
                };
                // let peer = peers[rand::thread_rng().gen_range(0..peers.len())];

                for begin in (0..piece_length).step_by(2 << 13) {
                    let length = std::cmp::min(piece_length - begin, 2 << 13);
                    let piece = Piece {
                        index,
                        begin,
                        length,
                    };
                    eprintln!("Requesting piece {:?}", piece);
                    let (tx, rx) = oneshot::channel();
                    pieces.push((piece, tx));
                    set.spawn(rx);
                }
            }

            let mut file = File::create(&out).await?;
            if !handler.request_pieces(pieces).await? {
                while let Some(res) = set.join_next().await {
                    if let Some(piece) = res?? {
                        file.seek(SeekFrom::Start(
                            (piece.begin + piece.index * data_piece_length) as u64,
                        ))
                        .await
                        .context("seeking in file")?;
                        file.write(&piece.block).await.context("writing in file")?;
                    }
                }
            }
        }
    }
    Ok(())
}
