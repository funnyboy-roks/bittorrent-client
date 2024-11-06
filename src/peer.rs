use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use anyhow::{bail, Context};
use rand::RngCore;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};

use crate::Torrent;

pub trait AsyncReadExt {
    fn read_bytes<const N: usize>(
        &mut self,
    ) -> impl std::future::Future<Output = tokio::io::Result<[u8; N]>>;
}

impl<T> AsyncReadExt for T
where
    T: AsyncRead + Unpin,
{
    async fn read_bytes<const N: usize>(&mut self) -> tokio::io::Result<[u8; N]> {
        let mut buf = [0u8; N];
        self.read_exact(&mut buf).await?;
        Ok(buf)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have {},
    Bitfield(Vec<u8>),
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
    Cancel {},
    Port {},
}

impl Message {
    pub async fn read_from<R>(r: &mut R) -> anyhow::Result<Self>
    where
        R: AsyncRead + Unpin,
    {
        let len = r.read_u32().await? as usize;
        let tag = r.read_u8().await?;
        let mut payload = vec![0; len - 1];
        r.read_exact(&mut payload).await?;
        let msg = match tag {
            0 => Self::Choke,
            1 => Self::Unchoke,
            2 => Self::Interested,
            3 => Self::NotInterested,
            4 => Self::Have {},
            5 => Self::Bitfield(payload),
            6 => Self::Request {
                index: u32::from_be_bytes(payload[0..4].try_into()?),
                begin: u32::from_be_bytes(payload[4..8].try_into()?),
                length: u32::from_be_bytes(payload[8..12].try_into()?),
            },
            7 => Self::Piece {
                index: u32::from_be_bytes(payload[0..4].try_into()?),
                begin: u32::from_be_bytes(payload[4..8].try_into()?),
                block: payload[8..].to_vec(),
            },
            9 => Self::Port {},
            t => panic!("Unexpected tag {}", t),
        };
        Ok(msg)
    }

    pub async fn write_to<W>(&self, w: &mut W) -> anyhow::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let mut buf = Vec::new();
        let tag = match self {
            Message::Choke => 0,
            Message::Unchoke => 1,
            Message::Interested => 2,
            Message::NotInterested => 3,
            Message::Have {} => 4,
            Message::Bitfield(v) => {
                buf.write_all(&v).await?;
                5
            }
            &Message::Request {
                index,
                begin,
                length,
            } => {
                buf.write_u32(index).await?;
                buf.write_u32(begin).await?;
                buf.write_u32(length).await?;
                6
            }
            &Message::Piece {
                index,
                begin,
                ref block,
            } => {
                buf.write_u32(index).await?;
                buf.write_u32(begin).await?;
                buf.write(&block).await?;
                7
            }
            Message::Cancel {} => 8,
            Message::Port {} => 9,
        };

        w.write_u32(buf.len() as u32 + 1).await?;
        w.write_u8(tag).await?;
        w.write_all(&buf).await?;

        Ok(())
    }
}

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub struct Piece {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataPiece {
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    data: Torrent,
    info_hash: [u8; 20],
    bitfield: Option<Vec<u8>>,
    choked: bool,
}

impl Client {
    pub async fn connect(
        s: SocketAddr,
        data: Torrent,
        info_hash: [u8; 20],
    ) -> anyhow::Result<Self> {
        let mut ret = Self {
            stream: TcpStream::connect(s).await?,
            data,
            info_hash,
            bitfield: None,
            choked: false, // can be default since we assert that we get `Unchoke`.
        };
        ret.handshake().await?;

        let Message::Bitfield(bitfield) = Message::read_from(&mut ret.stream)
            .await
            .context("reading bitfield message")?
        else {
            bail!("First message received should be Bitfield.");
        };
        ret.bitfield = Some(bitfield);

        dbg!(Message::Interested)
            .write_to(&mut ret.stream)
            .await
            .context("sending interest message")?;

        let Message::Unchoke = Message::read_from(&mut ret.stream)
            .await
            .context("reading unchoke message")?
        else {
            bail!("expected unchoke message");
        };

        // dbg!(Message::Request {
        //     index: 0,
        //     begin: 0,
        //     length: 32,
        // })
        // .write_to(&mut ret.stream)
        // .await?;
        // let msg = Message::read_from(&mut ret.stream).await?;
        // dbg!(msg);
        // let msg = Message::read_from(&mut ret.stream).await?;
        // dbg!(msg);
        // let msg = Message::read_from(&mut ret.stream).await?;
        // dbg!(msg);
        // let msg = Message::read_from(&mut ret.stream).await?;
        // dbg!(msg);
        Ok(ret)
    }

    async fn add_pieces<W>(w: &mut W, pieces: &[Piece]) -> anyhow::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        for &piece in pieces {
            dbg!(Message::Request {
                index: piece.index as u32,
                begin: piece.begin as u32,
                length: piece.length as u32,
            })
            .write_to(w)
            .await?;
        }
        Ok(())
    }

    // Ok(None) if choked while requesting pieces
    // Ok(Some(vec![Some(a), None(b)])) if piece a succeeded but choked before piece b
    // Ok(true) if choked
    // Ok(false) if not choked
    pub async fn request_pieces(
        &mut self,
        pieces: Vec<(Piece, oneshot::Sender<Option<DataPiece>>)>,
    ) -> anyhow::Result<bool> {
        let (mut read, mut write) = self.stream.split();

        let just_pieces: Vec<_> = pieces.iter().map(|(k, _)| k).copied().collect();

        let mut pieces: HashMap<_, _> = pieces
            .into_iter()
            .map(|(k, v)| ((k.index, k.begin), v))
            .collect();

        let add = Self::add_pieces(&mut write, &just_pieces[..]);
        tokio::pin!(add);

        let choked = loop {
            tokio::select! {
                Ok(message) = Message::read_from(&mut read) => {
                    match message {
                        Message::Choke => {
                    eprintln!("choked 1");
                            break true;
                        },
                        Message::Piece { index, begin, block } => {
                            if let Some(piece) = pieces.remove(&(index, begin)) {
                                let data = DataPiece { index, begin, block };
                                if let Err(e) = piece.send(Some(data)) {
                                    eprintln!("ignoring dropped piece: {:?}", e);
                                }
                            }
                        }
                        _ => bail!("Unexpected message while requesting pieces: {:?}", message),
                    }
                },
                res = &mut add => {
                    res?;
                    break false;
                },
            };
        };
        if choked {
            self.choked = choked;
            return Ok(true);
        }
        let choked = loop {
            let message = Message::read_from(&mut read)
                .await
                .context("reading message")?;
            match message {
                Message::Choke => {
                    self.choked = true;
                    eprintln!("choked 2");
                }
                Message::Piece {
                    index,
                    begin,
                    block,
                } => {
                    eprintln!("recieved piece {}", index);
                    if let Some(piece) = pieces.remove(&(index, begin)) {
                        let data = DataPiece {
                            index,
                            begin,
                            block,
                        };
                        match piece.send(Some(data)) {
                            Ok(()) => {
                                if pieces.is_empty() {
                                    break false;
                                }
                            }
                            Err(e) => {
                                eprintln!("ignoring dropped piece: {:?}", e);
                            }
                        }
                    }
                }
                _ => bail!("Unexpected message while requesting pieces: {:?}", message),
            }
        };
        if choked {
            self.choked = choked;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn handshake(&mut self) -> anyhow::Result<[u8; 20]> {
        let prot_str = b"BitTorrent protocol";

        self.stream.write_all(&[prot_str.len() as u8]).await?;
        self.stream.write_all(prot_str).await?;
        self.stream.write_all(&[0; 8]).await?;
        self.stream.write_all(&self.info_hash).await?;
        let mut my_id = [0; 20];
        rand::thread_rng().fill_bytes(&mut my_id);
        self.stream.write_all(&my_id).await?;
        eprintln!("my_id = {:02x?}", my_id);

        let protocol_len = self.stream.read_u8().await? as usize;
        assert_eq!(
            protocol_len,
            prot_str.len(),
            "protocol name lengths not equal"
        );
        let mut buf = vec![0u8; protocol_len];
        self.stream.read_exact(&mut buf).await?;
        assert_eq!(buf, *prot_str, "protocol names not equal");

        let _reserved = self.stream.read_bytes::<8>().await?;
        // Don't want to check this since they can be set for extensions.
        // assert_eq!([0; 8], reserved, "Reserved bytes should be set to 0.");

        let buf = self.stream.read_bytes::<20>().await?;
        assert_eq!(&buf[..], &self.info_hash[..], "Info has not equal");

        let peer_id = self.stream.read_bytes::<20>().await?;
        eprintln!("Peer ID: {}", hex::encode(peer_id));

        Ok(peer_id)
    }
}
