//! Blocking, bounded frame I/O over `std::io::Read`/`Write` for both framings.
use crate::{Deframer, Framing, Message, WireError, encode};
use std::io::{ErrorKind, Read, Write};

const READ_BUFFER: usize = 64 * 1024;

#[derive(Debug)]
pub enum TransportError {
    /// Protocol rejection with a stable code. The link must close unless the caller has a
    /// documented recovery (see `decode_client_frame`).
    Wire(WireError),
    Io(std::io::Error),
}
impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(e) => write!(f, "wire: {e}"),
            Self::Io(e) => write!(f, "io: {}", e.kind()),
        }
    }
}
impl std::error::Error for TransportError {}
impl From<WireError> for TransportError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}
impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reads one frame at a time. Holds at most one partial body plus a fixed 64 KiB read buffer;
/// bytes after the current frame stay buffered for the next call. Any error closes the reader.
pub struct FrameReader<R> {
    inner: R,
    deframer: Deframer,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
    failed: bool,
}
impl<R: Read> FrameReader<R> {
    pub fn new(inner: R, framing: Framing) -> Self {
        Self {
            inner,
            deframer: Deframer::new(framing),
            buffer: vec![0; READ_BUFFER].into_boxed_slice(),
            start: 0,
            end: 0,
            failed: false,
        }
    }

    /// Next raw frame body; `Ok(None)` on clean end of stream between frames.
    pub fn read_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.failed {
            return Err(WireError("closed").into());
        }
        let result = self.next();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn next(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if self.start < self.end {
                let mut pending = &self.buffer[self.start..self.end];
                let before = pending.len();
                let frame = self.deframer.next_frame(&mut pending)?;
                self.start += before - pending.len();
                if frame.is_some() {
                    return Ok(frame);
                }
            }
            self.start = 0;
            self.end = match self.inner.read(&mut self.buffer) {
                Ok(0) => {
                    self.deframer.finish()?;
                    return Ok(None);
                }
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => 0,
                Err(e) => return Err(e.into()),
            };
        }
    }

    /// Next frame decoded strictly; any decode failure closes the reader.
    pub fn read<M: Message>(&mut self) -> Result<Option<M>, TransportError> {
        let Some(frame) = self.read_frame()? else {
            return Ok(None);
        };
        M::decode(&frame).map(Some).map_err(|e| {
            self.failed = true;
            e.into()
        })
    }

    pub fn get_ref(&self) -> &R {
        &self.inner
    }
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// Writes whole validated frames and flushes each. A failed write closes the writer, since a
/// partial frame may have reached the peer.
pub struct FrameWriter<W> {
    inner: W,
    framing: Framing,
    failed: bool,
}
impl<W: Write> FrameWriter<W> {
    pub fn new(inner: W, framing: Framing) -> Self {
        Self {
            inner,
            framing,
            failed: false,
        }
    }
    /// Encoding failures (schema, size) write nothing and leave the writer usable.
    pub fn write<M: Message>(&mut self, message: &M) -> Result<(), TransportError> {
        if self.failed {
            return Err(WireError("closed").into());
        }
        let bytes = encode(message, self.framing)?;
        self.inner
            .write_all(&bytes)
            .and_then(|()| self.inner.flush())
            .map_err(|e| {
                self.failed = true;
                e.into()
            })
    }
    pub fn get_ref(&self) -> &W {
        &self.inner
    }
    pub fn into_inner(self) -> W {
        self.inner
    }
}
