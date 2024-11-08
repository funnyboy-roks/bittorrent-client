use std::{collections::HashMap, fmt::Display, io::Write, ops::Index};

use anyhow::{bail, Context};
use nom::{
    branch::alt,
    bytes::complete::take,
    character::complete::{char, i64, u64},
    multi::many0,
    sequence::{delimited, terminated, tuple},
    IResult,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::serde;

#[derive(Debug, Clone)]
pub struct Decoded<'a> {
    pub source: Option<&'a [u8]>,
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

impl<'de> Deserialize<'de> for Decoded<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Decoded {
            source: None,
            kind: DecodedKind::deserialize(deserializer)?,
        })
    }
}

impl Decoded<'_> {
    pub fn encode<W>(&self, writer: &mut W) -> anyhow::Result<()>
    where
        W: Write,
    {
        match &self.kind {
            DecodedKind::Bytes(b) => {
                write!(writer, "{}:", b.len())?;
                writer.write_all(b)?;
            }
            DecodedKind::String(s) => {
                write!(writer, "{}:{}", s.len(), s)?;
            }
            DecodedKind::Int(n) => write!(writer, "i{}e", n)?,
            DecodedKind::List(l) => {
                write!(writer, "l")?;
                for i in l {
                    i.encode(writer)?;
                }
                write!(writer, "e")?;
            }
            DecodedKind::Dict(d) => {
                write!(writer, "d")?;
                for (k, v) in d {
                    write!(writer, "{}:{}", k.len(), k)?;
                    v.encode(writer)?;
                }
                write!(writer, "e")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        Decoded {
            source: Some(source),
            kind: self,
        }
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
    let (rest, vec) = delimited(char('l'), many0(decode), char('e'))(encoded)?;
    let slice = encoded
        .strip_suffix(rest)
        .expect("rest is the end of `encoded`");
    Ok((rest, DecodedKind::List(vec).into_decoded(slice)))
}

fn dict_entry(encoded: &[u8]) -> IResult<&[u8], (&str, Decoded<'_>)> {
    let (rest, (key, value)) = tuple((string, decode))(encoded)?;
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

// TODO: make this support AsyncRead
pub fn decode(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    alt((string, int, list, dict))(encoded)
}

pub fn decode_into<D>(encoded: &[u8]) -> anyhow::Result<D>
where
    D: DeserializeOwned,
{
    let v = match alt((string, int, list, dict))(encoded) {
        Ok((_, v)) => v,
        Err(e) => bail!("Error decoding {:?}", e),
    };
    Ok(serde(&v)?)
}

pub fn encode<W, S>(writer: &mut W, value: S) -> anyhow::Result<()>
where
    W: Write,
    S: Serialize,
{
    let string = serde_json::to_string(&value).context("serializing value")?;
    let decoded: Decoded<'_> = serde_json::from_str(&string).context("deserializing value")?;
    decoded.encode(writer)
}
