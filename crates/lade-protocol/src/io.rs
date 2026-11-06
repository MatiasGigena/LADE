use crate::{Deframer, Framing, Message, WireError, encode};
use std::io::{ErrorKind, Read, Write};

const READ_BUFFER: usize = 64 * 1024;

/// A failure reading or writing frames on a byte stream: a refused frame, or the stream's I/O error.
#[derive(Debug)]
pub enum FrameIoError {
    Wire(WireError),
    Io(std::io::Error),
}
impl std::fmt::Display for FrameIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(e) => write!(f, "wire: {e}"),
            Self::Io(e) => write!(f, "io: {}", e.kind()),
        }
    }
}
impl std::error::Error for FrameIoError {}
impl From<WireError> for FrameIoError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}
impl From<std::io::Error> for FrameIoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

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

    /// Reads the next frame body; `None` at a clean end of stream. A refusal poisons the reader.
    ///
    /// # Errors
    ///
    /// - `FrameIoError::Wire` for a refused frame, a stream that ended inside a frame, or a read
    ///   after an earlier refusal.
    /// - `FrameIoError::Io` when reading fails.
    pub fn read_frame(&mut self) -> Result<Option<Vec<u8>>, FrameIoError> {
        if self.failed {
            return Err(WireError::Closed.into());
        }
        let result = self.next();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn next(&mut self) -> Result<Option<Vec<u8>>, FrameIoError> {
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

    /// Reads and decodes the next message; `None` at a clean end of stream.
    ///
    /// # Errors
    ///
    /// Fails as `FrameReader::read_frame` does, or with `FrameIoError::Wire` when the frame does
    /// not decode as `M`.
    pub fn read<M: Message>(&mut self) -> Result<Option<M>, FrameIoError> {
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
    /// Encodes, writes and flushes one message. A write failure poisons the writer.
    ///
    /// # Errors
    ///
    /// - `FrameIoError::Wire` when the message does not encode, or after an earlier write
    ///   failure.
    /// - `FrameIoError::Io` when writing or flushing fails.
    pub fn write<M: Message>(&mut self, message: &M) -> Result<(), FrameIoError> {
        if self.failed {
            return Err(WireError::Closed.into());
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
