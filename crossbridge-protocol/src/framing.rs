//! Length-prefixed framing helpers.
//!
//! Wire format: 4-byte big-endian `u32` length followed by a postcard payload.
//! Frames larger than [`MAX_FRAME_SIZE`] are rejected on both encode and
//! decode to bound memory use against malformed input.

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Error, Result};

/// Maximum allowed framed payload size: 16 MiB.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let payload = postcard::to_stdvec(msg)?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(Error::FrameTooLarge {
            size: payload.len(),
            max: MAX_FRAME_SIZE,
        });
    }
    Ok(payload)
}

fn check_incoming_len(len: usize) -> Result<()> {
    if len > MAX_FRAME_SIZE {
        return Err(Error::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }
    Ok(())
}

/// Synchronously write a length-prefixed postcard frame.
pub fn write_message_sync<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: std::io::Write,
    T: Serialize,
{
    let payload = encode(msg)?;
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(&payload)?;
    Ok(())
}

/// Synchronously read a length-prefixed postcard frame.
pub fn read_message_sync<R, T>(r: &mut R) -> Result<T>
where
    R: std::io::Read,
    T: DeserializeOwned,
{
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    check_incoming_len(len)?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(postcard::from_bytes(&buf)?)
}

/// Asynchronously write a length-prefixed postcard frame.
pub async fn write_message<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = encode(msg)?;
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(&payload).await?;
    Ok(())
}

/// Asynchronously read a length-prefixed postcard frame.
pub async fn read_message<R, T>(r: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    check_incoming_len(len)?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}
