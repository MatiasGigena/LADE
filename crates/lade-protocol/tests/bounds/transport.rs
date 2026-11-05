use super::typed_id;
use lade_protocol::{
    FrameIoError, FrameReader, FrameWriter, Framing, MAX_FRAME, Message, WireError, WorkerBody,
    WorkerEnvelope, encode, types::DeliveryMode,
};

struct Trickle<'a>(&'a [u8], usize);
impl std::io::Read for Trickle<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.1 += 1;
        if self.1.is_multiple_of(3) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = self.0.len().min(out.len()).min(5);
        out[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[test]
fn blocking_reader_and_writer_frame_one_message_at_a_time() {
    let bodies = [
        WorkerBody::Received {
            input_id: typed_id(7),
        },
        WorkerBody::Abort,
        WorkerBody::ActivityDelta {
            item_id: typed_id(8),
            append_text: "a\u{2028}b\nc".to_owned(),
        },
    ];
    for framing in [Framing::Client, Framing::Worker] {
        let mut writer = FrameWriter::new(Vec::new(), framing);
        for body in &bodies {
            writer.write(&WorkerEnvelope::new(body.clone())).unwrap();
        }
        let bytes = writer.into_inner();
        let mut reader = FrameReader::new(Trickle(&bytes, 0), framing);
        for body in &bodies {
            let envelope: WorkerEnvelope = reader.read().unwrap().unwrap();
            assert_eq!(&envelope.body, body);
        }
        assert!(reader.read::<WorkerEnvelope>().unwrap().is_none());

        let mut truncated = FrameReader::new(&bytes[..bytes.len() - 1], framing);
        truncated.read_frame().unwrap().unwrap();
        truncated.read_frame().unwrap().unwrap();
        assert!(matches!(
            truncated.read_frame(),
            Err(FrameIoError::Wire(WireError::TruncatedFrame))
        ));
        assert!(matches!(
            truncated.read_frame(),
            Err(FrameIoError::Wire(WireError::Closed))
        ));
    }
}
#[test]
fn reader_poisons_on_invalid_frames_and_writer_rejects_invalid_messages() {
    let mut bytes = b"{\"protocol\":1}\n".to_vec();
    bytes.extend(encode(&WorkerEnvelope::new(WorkerBody::Shutdown), Framing::Worker).unwrap());
    let mut reader = FrameReader::new(&bytes[..], Framing::Worker);
    assert!(matches!(
        reader.read::<WorkerEnvelope>(),
        Err(FrameIoError::Wire(WireError::VersionMismatch))
    ));
    assert!(matches!(
        reader.read::<WorkerEnvelope>(),
        Err(FrameIoError::Wire(WireError::Closed))
    ));

    let mut writer = FrameWriter::new(Vec::new(), Framing::Worker);
    let invalid = WorkerEnvelope::new(WorkerBody::Deliver {
        input_id: typed_id(1),
        mode: DeliveryMode::Prompt,
        text: String::new(),
    });
    assert!(matches!(
        writer.write(&invalid),
        Err(FrameIoError::Wire(WireError::InvalidSchema))
    ));
    writer
        .write(&WorkerEnvelope::new(WorkerBody::Shutdown))
        .unwrap();
    assert_eq!(
        WorkerEnvelope::decode(writer.get_ref().strip_suffix(b"\n").unwrap())
            .unwrap()
            .body,
        WorkerBody::Shutdown
    );
    let mut stale = WorkerEnvelope::new(WorkerBody::Shutdown);
    stale.bundle = "0.1.0-imp001".to_owned();
    assert!(matches!(
        writer.write(&stale),
        Err(FrameIoError::Wire(WireError::VersionMismatch))
    ));
}
#[test]
fn oversized_declared_client_length_fails_before_allocation() {
    let header = u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes();
    let mut reader = FrameReader::new(&header[..], Framing::Client);
    assert!(matches!(
        reader.read_frame(),
        Err(FrameIoError::Wire(WireError::FrameSize))
    ));
}
