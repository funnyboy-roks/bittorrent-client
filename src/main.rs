use anyhow::{bail, Context};
use nom::{
    branch::alt,
    bytes::streaming::take,
    character::streaming::{char, i64, u64},
    multi::many0,
    sequence::{delimited, terminated, tuple},
    IResult,
};
use serde::Serialize;
use std::{collections::HashMap, env, fmt::Display, fs::File, ops::Index};

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Decoded<'a> {
    Bytes(&'a [u8]),
    String(&'a str),
    Int(i64),
    List(Vec<Decoded<'a>>),
    Dict(HashMap<&'a str, Decoded<'a>>),
}

impl<'a> Index<&'_ str> for Decoded<'a> {
    type Output = Decoded<'a>;

    fn index(&self, index: &'_ str) -> &Self::Output {
        match self {
            Decoded::Dict(d) => &d[index],
            _ => panic!("Cannot index with string into type other than dictionary"),
        }
    }
}

impl<'a> Index<usize> for Decoded<'a> {
    type Output = Decoded<'a>;

    fn index(&self, index: usize) -> &Self::Output {
        match self {
            Decoded::List(d) => &d[index],
            _ => panic!("Cannot index with usize into type other than list"),
        }
    }
}

impl Display for Decoded<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Decoded::Bytes(b) => {
                write!(f, "0x")?;
                for b in b.iter() {
                    write!(f, "{:02x}", b)?;
                }
                Ok(())
            }
            Decoded::String(s) => write!(f, "{}", s),
            Decoded::Int(n) => write!(f, "{}", n),
            Decoded::List(l) => {
                write!(f, "[")?;
                for (i, d) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", d)?;
                }
                write!(f, "]")
            }
            Decoded::Dict(l) => {
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

fn _decode_bencoded_value(encoded: &[u8]) -> anyhow::Result<(Decoded<'_>, &[u8])> {
    match encoded {
        [b'd', rest @ ..] => {
            let mut items = HashMap::new();
            let mut rest = rest;
            while rest[0] != b'e' {
                let (key_decoded, new_rest) = _decode_bencoded_value(rest)?;
                let (value_decoded, new_rest) = _decode_bencoded_value(new_rest)?;
                rest = new_rest;
                match key_decoded {
                    Decoded::String(key) => {
                        items.insert(key.into(), value_decoded);
                    }
                    _ => bail!("Expected key to be of type string, found {:?}", key_decoded),
                }
            }
            let rest = rest.strip_prefix(b"e").unwrap();
            Ok((Decoded::Dict(items), rest))
        }
        [b'l', rest @ ..] => {
            let mut items = Vec::new();
            let mut rest = rest;
            while rest[0] != b'e' {
                let (decoded, new_rest) = _decode_bencoded_value(rest)?;
                rest = new_rest;
                items.push(decoded);
            }
            let rest = rest.strip_prefix(b"e").unwrap();
            Ok((Decoded::List(items), rest))
        }
        [b'i', rest @ ..] => {
            let e = rest
                .iter()
                .position(|b| *b == b'e')
                .context("looking for end of integer")?;
            let (num, rest) = rest.split_at(e);
            let rest = &rest[1..];
            let num = String::from_utf8(num.to_vec())?;
            dbg!(&num);
            let num: i64 = num.parse()?;
            Ok((Decoded::Int(num), rest))
        }
        [b'0'..=b'9', ..] => {
            let split = encoded
                .iter()
                .position(|b| *b == b':')
                .context("need colon for string")?;
            let (len, rest) = encoded.split_at(split);
            let rest = &rest[1..];
            let len: usize = String::from_utf8(len.to_vec())?.parse()?;
            let (s, rest) = rest.split_at(len);
            if let Ok(s) = std::str::from_utf8(s) {
                Ok((Decoded::String(s), rest))
            } else {
                Ok((Decoded::Bytes(s), rest))
            }
        }
        _ => panic!(
            "Unhandled encoded value: {}",
            String::from_utf8(encoded.to_vec()).unwrap_or_else(|_| format!("{:?}", encoded))
        ),
    }
}

fn string(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, len) = terminated(u64, char(':'))(encoded)?;
    let (rest, s) = take(len)(rest)?;
    if let Ok(string) = std::str::from_utf8(s) {
        Ok((rest, Decoded::String(string)))
    } else {
        Ok((rest, Decoded::Bytes(s)))
    }
}

fn int(encoded: &[u8]) -> IResult<&[u8], Decoded> {
    let (rest, n) = delimited(char('i'), i64, char('e'))(encoded)?;
    Ok((rest, Decoded::Int(n)))
}

fn list(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, vec) = delimited(char('l'), many0(decode_bencoded_value), char('e'))(encoded)?;
    Ok((rest, Decoded::List(vec)))
}

fn dict_entry(encoded: &[u8]) -> IResult<&[u8], (&str, Decoded<'_>)> {
    let (rest, (key, value)) = tuple((string, decode_bencoded_value))(encoded)?;
    let Decoded::String(key) = key else {
        panic!("should always be string");
    };
    Ok((rest, (key, value)))
}

fn dict(encoded: &[u8]) -> IResult<&[u8], Decoded<'_>> {
    let (rest, vec) = delimited(char('d'), many0(dict_entry), char('e'))(encoded)?;
    Ok((rest, Decoded::Dict(vec.into_iter().collect())))
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
            // dbg!(value);
            let Decoded::Dict(dict) = value else {
                bail!("expected dict, got {:?}", value);
            };
            println!("Tracker URL: {}", dict["announce"]);
            println!("Length: {}", dict["info"]["length"]);
        }
        _ => {}
    }
    Ok(())
}
