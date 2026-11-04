use crate::{MAX_FRAME, Message, WireError};
use std::marker::PhantomData;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Framing {
    Client,
    Worker,
}

pub struct Deframer {
    framing: Framing,
    header: [u8; 4],
    header_len: usize,
    body: Vec<u8>,
    expected: Option<usize>,
    failed: bool,
}
impl Deframer {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            header: [0; 4],
            header_len: 0,
            body: Vec::new(),
            expected: None,
            failed: false,
        }
    }

    pub fn next_frame(&mut self, input: &mut &[u8]) -> Result<Option<Vec<u8>>, WireError> {
        if self.failed {
            return Err(WireError::Closed);
        }
        let result = self.consume(input);
        if result.is_err() {
            self.poison();
        }
        result
    }

    fn poison(&mut self) {
        self.failed = true;
        self.body = Vec::new();
        self.header_len = 0;
        self.expected = None;
    }

    fn consume(&mut self, input: &mut &[u8]) -> Result<Option<Vec<u8>>, WireError> {
        while !input.is_empty() {
            match self.framing {
                Framing::Client => {
                    let size = match self.expected {
                        Some(size) => size,
                        None => {
                            let n = (self.header.len() - self.header_len).min(input.len());
                            self.header[self.header_len..self.header_len + n]
                                .copy_from_slice(&input[..n]);
                            self.header_len += n;
                            *input = &input[n..];
                            if self.header_len != self.header.len() {
                                continue;
                            }
                            self.header_len = 0;
                            let size = usize::try_from(u32::from_be_bytes(self.header))
                                .map_err(|_| WireError::FrameSize)?;
                            if size == 0 || size > MAX_FRAME {
                                return Err(WireError::FrameSize);
                            }
                            self.body.reserve_exact(size);
                            self.expected = Some(size);
                            size
                        }
                    };
                    let n = (size - self.body.len()).min(input.len());
                    self.body.extend_from_slice(&input[..n]);
                    *input = &input[n..];
                    if self.body.len() == size {
                        self.expected = None;
                        return Ok(Some(std::mem::take(&mut self.body)));
                    }
                }
                Framing::Worker => {
                    let end = input.iter().position(|b| *b == b'\n');
                    let n = end.unwrap_or(input.len());
                    if self.body.len() + n > MAX_FRAME {
                        return Err(WireError::FrameSize);
                    }
                    if self.body.capacity() < self.body.len() + n {
                        let capacity = (self.body.capacity() * 2)
                            .max(self.body.len() + n)
                            .min(MAX_FRAME);
                        self.body.reserve_exact(capacity - self.body.len());
                    }
                    self.body.extend_from_slice(&input[..n]);
                    *input = &input[n..];
                    if end.is_some() {
                        *input = &input[1..];
                        if self.body.is_empty() {
                            return Err(WireError::FrameSize);
                        }
                        return Ok(Some(std::mem::take(&mut self.body)));
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn finish(&mut self) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError::Closed);
        }
        let incomplete = self.header_len != 0 || !self.body.is_empty() || self.expected.is_some();
        self.poison();
        if incomplete {
            Err(WireError::TruncatedFrame)
        } else {
            Ok(())
        }
    }
}

pub struct Decoder<M> {
    deframer: Deframer,
    failed: bool,
    message: PhantomData<fn() -> M>,
}
impl<M: Message> Decoder<M> {
    pub fn new(framing: Framing) -> Self {
        Self {
            deframer: Deframer::new(framing),
            failed: false,
            message: PhantomData,
        }
    }
    pub fn push(
        &mut self,
        mut input: &[u8],
        mut emit: impl FnMut(M) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError::Closed);
        }
        let result = (|| {
            while !input.is_empty() {
                if let Some(frame) = self.deframer.next_frame(&mut input)? {
                    emit(M::decode(&frame)?)?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
            let _ = self.deframer.finish();
        }
        result
    }
    pub fn finish(&mut self) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError::Closed);
        }
        self.failed = true;
        self.deframer.finish()
    }
}

/// A frame body buffer that refuses to grow past [`MAX_FRAME`].
struct BoundedWriter(Vec<u8>);

impl std::io::Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME - self.0.len() {
            return Err(std::io::Error::other("frame_size"));
        }
        if self.0.capacity() < self.0.len() + bytes.len() {
            let capacity = (self.0.capacity() * 2)
                .max(self.0.len() + bytes.len())
                .min(MAX_FRAME);
            self.0.reserve_exact(capacity - self.0.len());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn encode<M: Message>(frame: &M, framing: Framing) -> Result<Vec<u8>, WireError> {
    if frame.identity() != (crate::VERSION, crate::BUNDLE) {
        return Err(WireError::VersionMismatch);
    }
    frame.precheck()?;
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut writer, frame).map_err(|_| WireError::FrameSize)?;
    let body = writer.0;
    M::decode(&body)?;
    let mut result = Vec::with_capacity(body.len() + 4);
    match framing {
        Framing::Client => {
            let size = u32::try_from(body.len()).map_err(|_| WireError::FrameSize)?;
            result.extend_from_slice(&size.to_be_bytes());
        }
        Framing::Worker => (),
    }
    result.extend_from_slice(&body);
    if framing == Framing::Worker {
        result.push(b'\n');
    }
    Ok(result)
}
