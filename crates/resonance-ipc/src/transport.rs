use crate::{Command, Response};
use postcard::{from_bytes, to_stdvec};
use std::io::{self, Read, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("encode: {0}")]
    Encode(#[from] postcard::Error),
}

const MAX_MSG_LEN: u32 = 4 * 1024 * 1024;

/// Write a length-prefixed postcard message to a stream.
pub fn write_msg<W: Write, T: serde::Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<(), TransportError> {
    let bytes = to_stdvec(msg)?;
    let len = bytes.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}

/// Read a length-prefixed postcard message from a stream.
pub fn read_msg<R: Read, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T, TransportError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_MSG_LEN {
        return Err(TransportError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        )));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf)?;
    Ok(from_bytes(&buf)?)
}

pub fn write_command<W: Write>(writer: &mut W, cmd: &Command) -> Result<(), TransportError> {
    write_msg(writer, cmd)
}

pub fn read_command<R: Read>(reader: &mut R) -> Result<Command, TransportError> {
    read_msg(reader)
}

pub fn write_response<W: Write>(writer: &mut W, resp: &Response) -> Result<(), TransportError> {
    write_msg(writer, resp)
}

pub fn read_response<R: Read>(reader: &mut R) -> Result<Response, TransportError> {
    read_msg(reader)
}
