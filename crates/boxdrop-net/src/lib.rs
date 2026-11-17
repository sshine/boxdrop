//! A tiny framed transport used by the peer protocol.
//!
//! Frames are `u32` big-endian length followed by a bincode payload. The limit
//! is deliberately finite: a peer must not be able to make the receiver allocate
//! arbitrary memory.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const MAX_FRAME: usize = 256 * 1024 * 1024;

pub struct Connection {
    stream: TcpStream,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    pub async fn connect(address: &str) -> Result<Self> {
        let stream = TcpStream::connect(address)
            .await
            .with_context(|| format!("connecting to {address}"))?;
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }

    pub async fn accept_once(address: &str) -> Result<Self> {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("binding {address}"))?;
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }

    pub async fn send<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard())
            .context("encoding frame")?;
        if bytes.len() > MAX_FRAME {
            bail!("frame of {} bytes exceeds the limit", bytes.len());
        }
        self.stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await?;
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T> {
        let mut length = [0u8; 4];
        self.stream
            .read_exact(&mut length)
            .await
            .context("reading frame length")?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_FRAME {
            bail!("peer announced a {length}-byte frame");
        }
        let mut bytes = vec![0u8; length];
        self.stream
            .read_exact(&mut bytes)
            .await
            .context("reading frame body")?;
        let (value, _) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .context("decoding frame")?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn round_trips_a_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut connection = Connection::new(stream);
            let value: String = connection.recv().await.unwrap();
            connection.send(&format!("echo: {value}")).await.unwrap();
        });
        let mut client = Connection::connect(&address.to_string()).await.unwrap();
        client.send(&"hello".to_string()).await.unwrap();
        let reply: String = client.recv().await.unwrap();
        assert_eq!(reply, "echo: hello");
        server.await.unwrap();
    }
}
