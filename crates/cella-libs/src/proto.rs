//! The wire vocabulary, generated from proto/cella.proto (see
//! docs/NETWORK-MODEL.md, "The control plane"). This module is the
//! only place the generated code enters; every user speaks these
//! types and the length-delimited framing below.

include!(concat!(env!("OUT_DIR"), "/cella.rs"));

use prost::Message as _;

/// The Accord version this binary speaks. Version 3: the
/// Operation carries its direction (proto/cella.proto, the Accord
/// comment).
pub const ACCORD_VERSION: u32 = 3;

/// Frame one Message for the wire: a varint length, then the bytes
/// (the standard length-delimited protobuf form).
pub fn frame(msg: &Message) -> Vec<u8> {
    let mut buf = Vec::with_capacity(msg.encoded_len() + 4);
    msg.encode_length_delimited(&mut buf)
        .expect("a Vec never refuses bytes");
    buf
}

/// Read one framed Message from the front of a buffer. Returns the
/// message and the bytes consumed; None while the buffer holds an
/// incomplete frame (the caller keeps accumulating).
pub fn unframe(buf: &[u8]) -> Option<(Message, usize)> {
    let mut slice = buf;
    let len = prost::decode_length_delimiter(&mut slice).ok()?;
    let header = buf.len() - slice.len();
    if slice.len() < len {
        return None;
    }
    let msg = Message::decode(&slice[..len]).ok()?;
    Some((msg, header + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_operation() -> Operation {
        Operation {
            id: vec![7u8; 16],
            destination: Some(Destination {
                host: "example.com".into(),
                ip: vec![1, 2, 3, 4],
                port: 443,
                proto: 6,
                ethertype: 0x0800,
                mac: Vec::new(),
            }),
            guest_ns: 1_000_000_007,
            host_ns: 9_000_000_001,
            direction: 0,
        }
    }

    /// Every Message body survives the wire: encode, frame,
    /// unframe, decode, equal. The vocabulary is the contract.
    #[test]
    fn every_body_round_trips() {
        let bodies = vec![
            message::Body::Accord(Accord {
                version: ACCORD_VERSION,
            }),
            message::Body::Event(Event {
                event: Some(event::Event::Parked(sample_operation())),
            }),
            message::Body::Event(Event {
                event: Some(event::Event::Released(Released {
                    id: vec![7u8; 16],
                    first_response_ns: 42,
                    bytes_in: 4096,
                    bytes_out: 512,
                })),
            }),
            message::Body::Event(Event {
                event: Some(event::Event::Lapsed(Lapsed {
                    id: vec![7u8; 16],
                    why: "the sender gave up".into(),
                })),
            }),
            message::Body::Decision(Decision {
                id: vec![7u8; 16],
                decision: Some(decision::Decision::Release(Release {})),
            }),
            message::Body::Decision(Decision {
                id: vec![7u8; 16],
                decision: Some(decision::Decision::Refusal(Refusal {
                    why: "not part of this world".into(),
                })),
            }),
            message::Body::Valve(Valve {
                v: valve::V::Closed as i32,
            }),
        ];
        for body in bodies {
            let msg = Message { body: Some(body) };
            let wire = frame(&msg);
            let (back, used) = unframe(&wire).expect("a complete frame decodes");
            assert_eq!(used, wire.len());
            assert_eq!(back, msg);
        }
    }

    /// A split frame waits: unframe on a partial buffer is None,
    /// and the same bytes complete once the rest arrives -- the
    /// serial line delivers in pieces.
    #[test]
    fn a_partial_frame_waits_for_the_rest() {
        let msg = Message {
            body: Some(message::Body::Event(Event {
                event: Some(event::Event::Parked(sample_operation())),
            })),
        };
        let wire = frame(&msg);
        for cut in 1..wire.len() {
            assert!(
                unframe(&wire[..cut]).is_none(),
                "cut at {cut} decoded early"
            );
        }
        let (back, used) = unframe(&wire).unwrap();
        assert_eq!((back, used), (msg, wire.len()));
    }

    /// Two frames back to back: the first decodes, the consumed
    /// count hands the caller the exact start of the second.
    #[test]
    fn frames_stream_back_to_back() {
        let a = Message {
            body: Some(message::Body::Accord(Accord {
                version: ACCORD_VERSION,
            })),
        };
        let b = Message {
            body: Some(message::Body::Valve(Valve {
                v: valve::V::Open as i32,
            })),
        };
        let mut wire = frame(&a);
        wire.extend(frame(&b));
        let (first, used) = unframe(&wire).unwrap();
        assert_eq!(first, a);
        let (second, used2) = unframe(&wire[used..]).unwrap();
        assert_eq!(second, b);
        assert_eq!(used + used2, wire.len());
    }
}
