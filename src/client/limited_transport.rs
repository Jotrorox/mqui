//! Reject oversized packets as soon as their Remaining Length header arrives,
//! before the MQTT decoder allocates/accumulates the packet body.
use crate::models::limits::MAX_PACKET_BYTES;
use mqtt_endpoint_tokio::mqtt_ep::transport::{TransportError, TransportOps};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{
    future::Future,
    io::{self, IoSlice},
    pin::Pin,
    time::Duration,
};

#[derive(Default)]
struct PacketFraming {
    header_bytes: usize,
    remaining: usize,
    body_left: usize,
}

impl PacketFraming {
    fn check(&mut self, mut bytes: &[u8]) -> Result<(), TransportError> {
        while !bytes.is_empty() {
            if self.body_left > 0 {
                let count = self.body_left.min(bytes.len());
                self.body_left -= count;
                bytes = &bytes[count..];
            } else {
                let byte = bytes[0];
                bytes = &bytes[1..];
                if self.header_bytes == 0 {
                    self.header_bytes = 1;
                    self.remaining = 0;
                    continue;
                }
                self.remaining += usize::from(byte & 127) << (7 * (self.header_bytes - 1));
                self.header_bytes += 1;
                if self.remaining + self.header_bytes > MAX_PACKET_BYTES
                    || (self.header_bytes == 5 && byte & 128 != 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "MQTT packet exceeds size limit or has invalid length",
                    )
                    .into());
                }
                if byte & 128 == 0 {
                    self.body_left = self.remaining;
                    self.header_bytes = 0;
                }
            }
        }
        Ok(())
    }
}

pub(super) struct LimitedTransport {
    inner: Box<dyn TransportOps + Send>,
    framing: PacketFraming,
    limit_exceeded: Arc<AtomicBool>,
    write_error: Arc<Mutex<Option<String>>>,
}

impl LimitedTransport {
    pub(super) fn new(
        inner: Box<dyn TransportOps + Send>,
        limit_exceeded: Arc<AtomicBool>,
        write_error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            inner,
            framing: PacketFraming::default(),
            limit_exceeded,
            write_error,
        }
    }
}

impl TransportOps for LimitedTransport {
    fn recv<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let size = self.inner.recv(buffer).await?;
            self.framing.check(&buffer[..size]).inspect_err(|_| {
                self.limit_exceeded.store(true, Ordering::Release);
            })?;
            Ok(size)
        })
    }

    fn send<'a>(
        &'a mut self,
        buffers: &'a [IoSlice<'a>],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let mut framing = PacketFraming::default();
            for buffer in buffers {
                framing.check(buffer)?;
            }
            self.inner.send(buffers).await.inspect_err(|err| {
                // mqtt-endpoint-tokio 0.6.5 discards transport send errors.
                // Preserve the failure so CONNECT cannot appear successful.
                *self.write_error.lock().unwrap() = Some(err.to_string());
            })
        })
    }

    fn shutdown<'a>(
        &'a mut self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.inner.shutdown(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_large_length_without_receiving_body() {
        let mut framing = PacketFraming::default();
        framing.check(&[0x30, 0x80]).unwrap();
        framing.check(&[0x80]).unwrap();
        assert!(framing.check(&[0x10]).is_err());
    }

    #[test]
    fn handles_fragmented_headers_bodies_and_coalesced_packets() {
        let mut framing = PacketFraming::default();
        for bytes in [
            &[0x30, 0x80][..],
            &[0x01],
            &[0; 100],
            &[0; 28],
            &[0xd0, 0, 0x30, 1, 42],
        ] {
            framing.check(bytes).unwrap();
        }
        assert_eq!(framing.header_bytes, 0);
        assert_eq!(framing.body_left, 0);
        assert!(framing.check(&[0x30, 0x80, 0x80, 0x80, 0x80]).is_err());
    }
}
