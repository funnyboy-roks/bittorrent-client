use ::reqwest::Url;
use anyhow::Context;
use core::str;
use nom::{
    branch::alt,
    bytes::streaming::take,
    character::streaming::{char, i64, u64},
    multi::many0,
    sequence::{delimited, terminated, tuple},
    IResult,
};
use reqwest::blocking as reqwest;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    env,
    fmt::Display,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Index,
    str::FromStr,
};

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

#[derive(Debug, Clone)]
pub struct Decoded<'a> {
    pub source: &'a [u8],
    pub kind: DecodedKind<'a>,
}

impl Serialize for Decoded<'_> {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.kind.serialize(s)
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum DecodedKind<'a> {
    Bytes(&'a [u8]),
    String(&'a str),
    Int(i64),
    List(Vec<Decoded<'a>>),
    Dict(HashMap<&'a str, Decoded<'a>>),
}

impl<'a> DecodedKind<'a> {
    pub fn into_decoded(self, source: &'a [u8]) -> Decoded<'a> {
        Decoded { source, kind: self }
    }
}

impl<'a> Index<&'_ str> for Decoded<'a> {
    type Output = Decoded<'a>;

    fn index(&self, index: &'_ str) -> &Self::Output {
        match &self.kind {
            DecodedKind::Dict(d) => &d[index],
            _ => panic!("Cannot index with string into type other than dictionary"),
        }
    }
}

impl<'a> Index<usize> for Decoded<'a> {
    type Output = Decoded<'a>;

    fn index(&self, index: usize) -> &Self::Output {
        match &self.kind {
            DecodedKind::List(d) => &d[index],
            _ => panic!("Cannot index with usize into type other than list"),
        }
    }
}

impl Display for Decoded<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            DecodedKind::Bytes(b) => {
                write!(f, "0x")?;
                for b in b.iter() {
                    write!(f, "{:02x}", b)?;
                }
                Ok(())
            }
            DecodedKind::String(s) => write!(f, "{}", s),
            DecodedKind::Int(n) => write!(f, "{}", n),
            DecodedKind::List(l) => {
                write!(f, "[")?;
                for (i, d) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", d)?;
                }
                write!(f, "]")
            }
            DecodedKind::Dict(l) => {
                write!(f, "{{")?;
                for (i, (key, value)) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", key, value)?;
                }
                write!(f, "}}")
            }
        }
    }
}

fn string(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, len) = terminated(u64, char(':'))(encoded)?;
    let (rest, s) = take(len)(rest)?;
    let source = encoded
        .strip_suffix(rest)
        .expect("rest is the end of `encoded`");
    if let Ok(string) = std::str::from_utf8(s) {
        Ok((rest, DecodedKind::String(string).into_decoded(source)))
    } else {
        Ok((rest, DecodedKind::Bytes(s).into_decoded(source)))
    }
}

fn int(encoded: &[u8]) -> IResult<&[u8], Decoded> {
    let (rest, n) = delimited(char('i'), i64, char('e'))(encoded)?;
    let slice = encoded
        .strip_suffix(rest)
        .expect("rest is the end of `encoded`");
    Ok((rest, DecodedKind::Int(n).into_decoded(slice)))
}

fn list(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, vec) = delimited(char('l'), many0(decode_bencoded_value), char('e'))(encoded)?;
    let slice = encoded
        .strip_suffix(rest)
        .expect("rest is the end of `encoded`");
    Ok((rest, DecodedKind::List(vec).into_decoded(slice)))
}

fn dict_entry(encoded: &[u8]) -> IResult<&[u8], (&str, Decoded<'_>)> {
    let (rest, (key, value)) = tuple((string, decode_bencoded_value))(encoded)?;
    let DecodedKind::String(key) = key.kind else {
        panic!("should always be string");
    };
    Ok((rest, (key, value)))
}

fn dict(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, vec) = delimited(char('d'), many0(dict_entry), char('e'))(encoded)?;
    let slice = encoded
        .strip_suffix(rest)
        .expect("rest is the end of `encoded`");
    Ok((
        rest,
        DecodedKind::Dict(vec.into_iter().collect()).into_decoded(slice),
    ))
}

fn decode_bencoded_value(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    alt((string, int, list, dict))(encoded)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    match &*args[1] {
        "decode" => {
            let (_, value) = decode_bencoded_value(&args[2].as_bytes()).unwrap();
            dbg!(&value);
            println!("{}", serde_json::to_string_pretty(&value)?);
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

            let mut url = Url::from_str(&data.announce)?;
            url.query_pairs_mut()
                .append_pair("info_hash", unsafe { str::from_utf8_unchecked(&info_hash) })
                .append_pair("peer_id", "20 chars is too shor")
                .append_pair("port", "6881")
                .append_pair("uploaded", "0")
                .append_pair("downloaded", "0")
                .append_pair("left", &data.info.length.to_string())
                .append_pair("compact", "1");
            dbg!(url.to_string());
            let res = reqwest::get(url)?;
            let text = res.bytes()?;
            dbg!(&text);
            let (_, res) = decode_bencoded_value(&text).unwrap();
            let res: PeersResponse = serde(&res)?;
            dbg!(&res);

            for peer in res.peers() {
                println!("{}", peer);
            }
        }
        command => {
            panic!("Unknown command '{}'", command)
        }
    }
    Ok(())
}
