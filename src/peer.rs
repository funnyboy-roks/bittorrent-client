use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use rand::RngCore;

use crate::Torrent;

#[derive(Debug, Clone)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have {},
    Bitfield {},
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
    pub fn read_from<R>(r: &mut R) -> anyhow::Result<Self>
    where
        R: Read,
    {
        let len = r.read_u32::<BigEndian>()? as usize;
        let tag = r.read_u8()?;
        let mut payload = vec![0; len - 1];
        r.read_exact(&mut payload)?;
        let msg = match tag {
            0 => Self::Choke,
            1 => Self::Unchoke,
            2 => Self::Interested,
            3 => Self::NotInterested,
            4 => Self::Have {},
            5 => Self::Bitfield {},
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

    pub fn write_to<W>(&self, w: &mut W) -> anyhow::Result<()>
    where
        W: Write,
    {
        let mut buf = Vec::new();
        let tag = match self {
            Message::Choke => 0,
            Message::Unchoke => 1,
            Message::Interested => 2,
            Message::NotInterested => 3,
            Message::Have {} => 4,
            Message::Bitfield {} => 5,
            Message::Request {
                index,
                begin,
                length,
            } => {
                buf.write_u32::<BigEndian>(*index)?;
                buf.write_u32::<BigEndian>(*begin)?;
                buf.write_u32::<BigEndian>(*length)?;
                6
            }
            Message::Piece {
                index,
                begin,
                block,
            } => {
                buf.write_u32::<BigEndian>(*index)?;
                buf.write_u32::<BigEndian>(*begin)?;
                buf.write(&block)?;
                7
            }
            Message::Cancel {} => 8,
            Message::Port {} => 9,
        };

        w.write_u32::<BigEndian>(buf.len() as u32 + 1)?;
        w.write_u8(tag)?;
        w.write_all(&buf)?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct PeerHandler {
    stream: TcpStream,
    data: Torrent,
    info_hash: [u8; 20],
}

impl PeerHandler {
    pub fn connect(s: SocketAddr, data: Torrent, info_hash: [u8; 20]) -> anyhow::Result<Self> {
        let mut this = Self {
            stream: TcpStream::connect(s)?,
            data,
            info_hash,
        };
        this.handshake()?;

        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        dbg!(Message::Interested).write_to(&mut this.stream)?;
        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        dbg!(Message::Request {
            index: 0,
            begin: 0,
            length: 32,
        })
        .write_to(&mut this.stream)?;
        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        let msg = Message::read_from(&mut this.stream)?;
        dbg!(msg);
        Ok(this)
    }

    fn handshake(&mut self) -> anyhow::Result<[u8; 20]> {
        let prot_str = b"BitTorrent protocol";
        self.stream.write_all(&[prot_str.len() as u8])?;
        self.stream.write_all(prot_str)?;
        self.stream.write_all(&[0; 8])?;
        self.stream.write_all(&self.info_hash)?;
        let mut my_id = [0; 20];
        rand::thread_rng().fill_bytes(&mut my_id);
        self.stream.write_all(&my_id)?;
        eprintln!("my_id = {:02x?}", my_id);

        let mut buf = [0; 20];
        self.stream.read_exact(&mut buf)?;
        assert_eq!(
            buf[0] as usize,
            prot_str.len(),
            "protocol name lengths not equal"
        );
        assert_eq!(buf[1..], *prot_str, "protocol names not equal");
        let mut buf = [0; 8];
        self.stream.read_exact(&mut buf)?;
        let mut buf = [0; 20];
        self.stream.read_exact(&mut buf)?;
        assert_eq!(&buf[..], &self.info_hash[..], "Info has not equal");
        let mut peer_id = [0; 20];
        self.stream.read_exact(&mut peer_id)?;
        eprintln!("Peer ID: {}", hex::encode(peer_id));

        Ok(peer_id)
    }
}
