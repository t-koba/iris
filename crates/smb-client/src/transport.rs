use crate::netbios::{
    decode_direct_tcp_header, encode_direct_tcp_header, DIRECT_TCP_MAX_PAYLOAD_BYTES,
};
use crate::SmbClientError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};

#[derive(Debug, thiserror::Error)]
pub enum SmbTransportError {
    #[error(transparent)]
    Protocol(#[from] SmbClientError),

    #[error("SMB transport I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("payload length {length} exceeds transport limit {limit}")]
    PayloadTooLargeForLimit { length: usize, limit: usize },
}

pub struct DirectTcpTransport<T> {
    stream: T,
    max_payload_len: usize,
}

impl<T> DirectTcpTransport<T> {
    pub fn new(stream: T) -> Self {
        Self {
            stream,
            max_payload_len: DIRECT_TCP_MAX_PAYLOAD_BYTES,
        }
    }

    pub fn with_max_payload_len(mut self, max_payload_len: usize) -> Self {
        self.max_payload_len = max_payload_len.min(DIRECT_TCP_MAX_PAYLOAD_BYTES);
        self
    }

    pub fn into_inner(self) -> T {
        self.stream
    }
}

impl DirectTcpTransport<TcpStream> {
    pub async fn connect(addr: impl ToSocketAddrs) -> Result<Self, SmbTransportError> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(Self::new(stream))
    }
}

impl<T> DirectTcpTransport<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn send_packet(&mut self, payload: &[u8]) -> Result<(), SmbTransportError> {
        if payload.len() > self.max_payload_len {
            return Err(SmbTransportError::PayloadTooLargeForLimit {
                length: payload.len(),
                limit: self.max_payload_len,
            });
        }
        let header = encode_direct_tcp_header(payload.len())?;
        self.stream.write_all(&header).await?;
        self.stream.write_all(payload).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn recv_packet(&mut self) -> Result<Vec<u8>, SmbTransportError> {
        let mut header = [0; 4];
        self.stream.read_exact(&mut header).await?;
        let payload_len = decode_direct_tcp_header(&header)?;
        if payload_len > self.max_payload_len {
            return Err(SmbTransportError::PayloadTooLargeForLimit {
                length: payload_len,
                limit: self.max_payload_len,
            });
        }
        let mut payload = vec![0; payload_len];
        self.stream.read_exact(&mut payload).await?;
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn direct_tcp_transport_round_trips_packet() {
        let (client, server) = duplex(128);
        let mut sender = DirectTcpTransport::new(client);
        let mut receiver = DirectTcpTransport::new(server);

        sender.send_packet(b"smb2-packet").await.expect("send");
        let received = receiver.recv_packet().await.expect("receive");

        assert_eq!(received, b"smb2-packet");
    }

    #[tokio::test]
    async fn direct_tcp_transport_rejects_send_above_configured_limit() {
        let (client, _server) = duplex(128);
        let mut sender = DirectTcpTransport::new(client).with_max_payload_len(3);

        let error = sender.send_packet(b"abcd").await.expect_err("reject");

        assert!(matches!(
            error,
            SmbTransportError::PayloadTooLargeForLimit {
                length: 4,
                limit: 3
            }
        ));
    }

    #[tokio::test]
    async fn direct_tcp_transport_rejects_receive_above_configured_limit() {
        let (client, server) = duplex(128);
        let mut sender = DirectTcpTransport::new(client);
        let mut receiver = DirectTcpTransport::new(server).with_max_payload_len(3);

        sender.send_packet(b"abcd").await.expect("send");
        let error = receiver.recv_packet().await.expect_err("reject");

        assert!(matches!(
            error,
            SmbTransportError::PayloadTooLargeForLimit {
                length: 4,
                limit: 3
            }
        ));
    }
}
