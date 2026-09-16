use crate::{MAX_FRAME, WireError, decode, dto::Envelope};

#[derive(Clone, Copy)]
pub enum Framing {
    Client,
    Worker,
}

/// One body retained, regardless of input batching. Any error poisons the link.
pub struct Decoder {
    framing: Framing,
    header: Vec<u8>,
    body: Vec<u8>,
    expected: Option<usize>,
    failed: bool,
}
impl Decoder {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            header: Vec::with_capacity(4),
            body: Vec::new(),
            expected: None,
            failed: false,
        }
    }
    pub fn push(
        &mut self,
        input: &[u8],
        mut emit: impl FnMut(Envelope) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError("closed"));
        }
        let result = self.consume(input, &mut emit);
        if result.is_err() {
            self.failed = true;
            self.body = Vec::new();
            self.header.clear();
        }
        result
    }
    fn consume(
        &mut self,
        mut input: &[u8],
        emit: &mut impl FnMut(Envelope) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        while !input.is_empty() {
            match self.framing {
                Framing::Client => {
                    if self.expected.is_none() {
                        let n = (4 - self.header.len()).min(input.len());
                        self.header.extend_from_slice(&input[..n]);
                        input = &input[n..];
                        if self.header.len() != 4 {
                            continue;
                        }
                        let size =
                            u32::from_be_bytes(self.header[..].try_into().expect("four bytes"))
                                as usize;
                        self.header.clear();
                        if size == 0 || size > MAX_FRAME {
                            return Err(WireError("frame_size"));
                        }
                        self.body.reserve_exact(size);
                        self.expected = Some(size);
                    }
                    let size = self.expected.expect("validated length");
                    let n = (size - self.body.len()).min(input.len());
                    self.body.extend_from_slice(&input[..n]);
                    input = &input[n..];
                    if self.body.len() == size {
                        let frame = decode(&self.body)?;
                        self.body.clear();
                        self.expected = None;
                        emit(frame)?;
                    }
                }
                Framing::Worker => {
                    let end = input.iter().position(|b| *b == b'\n');
                    let n = end.unwrap_or(input.len());
                    if self.body.len() + n > MAX_FRAME {
                        return Err(WireError("frame_size"));
                    }
                    if self.body.capacity() < self.body.len() + n {
                        let capacity = (self.body.capacity() * 2)
                            .max(self.body.len() + n)
                            .min(MAX_FRAME);
                        self.body.reserve_exact(capacity - self.body.len());
                    }
                    self.body.extend_from_slice(&input[..n]);
                    input = &input[n..];
                    if end.is_some() {
                        let frame = decode(&self.body)?;
                        self.body.clear();
                        input = &input[1..];
                        emit(frame)?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError("closed"));
        }
        self.failed = true;
        let incomplete =
            !self.header.is_empty() || !self.body.is_empty() || self.expected.is_some();
        self.body = Vec::new();
        self.header.clear();
        if incomplete {
            Err(WireError("truncated_frame"))
        } else {
            Ok(())
        }
    }
}

pub fn encode(frame: &Envelope, framing: Framing) -> Result<Vec<u8>, WireError> {
    if frame.protocol != crate::VERSION || frame.bundle != crate::BUNDLE {
        return Err(WireError("version_mismatch"));
    }
    match &frame.body {
        crate::dto::Body::ChunkData { hex, .. } if !crate::schema::format(hex, "chunk-hex") => {
            return Err(WireError("invalid_schema"));
        }
        crate::dto::Body::ChunkStart { sha256, .. } if !crate::schema::format(sha256, "sha256") => {
            return Err(WireError("invalid_schema"));
        }
        _ => (),
    }
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
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut writer, frame).map_err(|_| WireError("frame_size"))?;
    let body = writer.0;
    decode(&body)?;
    let mut result = Vec::with_capacity(body.len() + 4);
    match framing {
        Framing::Client => result.extend_from_slice(&(body.len() as u32).to_be_bytes()),
        Framing::Worker => (),
    }
    result.extend_from_slice(&body);
    if matches!(framing, Framing::Worker) {
        result.push(b'\n');
    }
    Ok(result)
}
