//! The server's compact shared-memory message-table encoding.

use crate::codec::Error;
use serde::de::Error as _;

pub fn deserialize_message_table(input: &[u8]) -> Result<Vec<(String, u16)>, Error> {
    let mut remaining = input;
    let mut count = [0; 4];
    count.copy_from_slice(take(&mut remaining, 4)?);
    let count = u32::from_be_bytes(count);
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = u16::try_from(read_integer(&mut remaining)?).map_err(Error::custom)?;
        let length = usize::try_from(read_integer(&mut remaining)?).map_err(Error::custom)?;
        let name = std::str::from_utf8(take(&mut remaining, length)?)
            .map_err(Error::custom)?
            .to_owned();
        entries.push((name, id));
    }
    Ok(entries)
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
    if input.len() < length {
        return Err(Error::custom("truncated API message table"));
    }
    let (prefix, remaining) = input.split_at(length);
    *input = remaining;
    Ok(prefix)
}

fn read_integer(input: &mut &[u8]) -> Result<u64, Error> {
    let byte = take(input, 1)?[0];
    if byte & 1 != 0 {
        return Ok(u64::from(byte / 2));
    }
    if byte & 2 != 0 {
        return Ok(128 + u64::from(u16::from_le_bytes([byte, take(input, 1)?[0]]) / 4));
    }
    if byte & 4 != 0 {
        let rest = take(input, 3)?;
        return Ok(128
            + 16384
            + u64::from(u32::from_le_bytes([byte, rest[0], rest[1], rest[2]]) / 8));
    }
    let mut bytes = [0; 8];
    bytes.copy_from_slice(take(input, 8)?);
    Ok(u64::from_le_bytes(bytes))
}
