//! H.264 RTP depacketization (RFC 6184) for the receive path.
//!
//! Sending is handled by webrtc-rs's `H264Payloader` (STAP-A for SPS/PPS +
//! IDR, FU-A fragmentation for large NALs). On the receive side
//! `TrackRemote::read_rtp` yields raw RTP payloads, so we reassemble them
//! into an Annex-B bitstream for the H.264 decoder here.

use bytes::Bytes;
use rtp::codecs::h264::H264Packet;
use rtp::packetizer::Depacketizer;

/// Wraps webrtc-rs's `H264Packet` with loss resilience: when a new FU-A
/// fragment sequence starts while a previous one is still open (its tail was
/// lost), the stale partial buffer is discarded instead of being corrupted.
#[derive(Default)]
pub struct H264Depacketizer {
    inner: H264Packet,
    fua_open: bool,
}

impl H264Depacketizer {
    /// Feeds one RTP payload. Returns the Annex-B chunk (start code + NAL
    /// unit, start codes included) for single-NAL and STAP-A packets and for
    /// the final FU-A fragment of a sequence; returns `None` for intermediate
    /// FU-A fragments and for packets that fail to parse.
    pub fn depacketize(&mut self, payload: &[u8]) -> Option<Bytes> {
        let &first = payload.first()?;
        let is_fua = first & 0x1f == 28;

        let packet = Bytes::copy_from_slice(payload);
        if is_fua && self.inner.is_partition_head(&packet) && self.fua_open {
            // New FU-A start while a previous sequence is open: the old tail
            // was lost. Drop the stale buffer so this NAL starts clean.
            self.inner = H264Packet::default();
        }

        let bytes = self.inner.depacketize(&packet).ok()?;
        if is_fua {
            // Intermediate fragments yield empty bytes; the sequence stays
            // open until a non-empty (final) NAL is produced.
            self.fua_open = bytes.is_empty();
        } else {
            self.fua_open = false;
        }

        if bytes.is_empty() {
            None
        } else {
            Some(bytes)
        }
    }
}

/// Scans an Annex-B chunk for a keyframe: any IDR (type 5) or SPS (type 7)
/// NAL unit. Handles both 3- and 4-byte start codes.
pub fn annexb_contains_keyframe(data: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let mut nal_start = i + 3;
            // 4-byte start code: 00 00 00 01.
            if data.get(nal_start) == Some(&0) {
                nal_start += 1;
            }
            if let Some(&nal_type) = data.get(nal_start) {
                if matches!(nal_type & 0x1f, 5 | 7) {
                    return true;
                }
            }
            // Skip past this NAL unit (start code + at least one byte) and
            // continue scanning for further NALs in the chunk.
            i = nal_start.saturating_add(1);
            continue;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annexb(nal: &[u8]) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x00, 0x01];
        out.extend_from_slice(nal);
        out
    }

    #[test]
    fn single_nal_packet_becomes_annexb() {
        let mut dep = H264Depacketizer::default();
        let nal = [0x65, 0x88, 0x84]; // IDR slice
        let out = dep.depacketize(&nal).expect("single NAL parsed");
        assert_eq!(out.to_vec(), annexb(&nal));
    }

    #[test]
    fn stap_a_splits_into_annexb_nals() {
        let mut dep = H264Depacketizer::default();
        let sps = [0x67, 0x42, 0xe0];
        let pps = [0x68, 0xce, 0x3c];
        let mut stap = vec![0x78]; // STAP-A
        stap.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        stap.extend_from_slice(&sps);
        stap.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        stap.extend_from_slice(&pps);

        let out = dep.depacketize(&stap).expect("STAP-A parsed");
        let mut expected = annexb(&sps);
        expected.extend_from_slice(&annexb(&pps));
        assert_eq!(out.to_vec(), expected);
    }

    #[test]
    fn fua_fragments_reassemble_in_order() {
        let mut dep = H264Depacketizer::default();
        let body = [0x11, 0x22, 0x33, 0x44, 0x55];
        // FU indicator: type 28, NRI 1. FU header: S=1 then E=1.
        let start = [0x7c, 0x85]; // 0x80 | 5 (IDR)
        let mid = [0x7c, 0x05];
        let end = [0x7c, 0x45];

        let mut fu_start = start.to_vec();
        fu_start.extend_from_slice(&body[..2]);
        let mut fu_mid = mid.to_vec();
        fu_mid.extend_from_slice(&body[2..4]);
        let mut fu_end = end.to_vec();
        fu_end.extend_from_slice(&body[4..]);

        assert!(dep.depacketize(&fu_start).is_none(), "start fragment");
        assert!(dep.depacketize(&fu_mid).is_none(), "middle fragment");
        let out = dep
            .depacketize(&fu_end)
            .expect("end fragment completes NAL");
        let mut expected = annexb(&[0x65]);
        expected.extend_from_slice(&body);
        assert_eq!(out.to_vec(), expected);
    }

    #[test]
    fn lost_fua_tail_discards_stale_buffer() {
        let mut dep = H264Depacketizer::default();
        // First sequence loses its tail (only the S=1 fragment arrives)…
        let start = [0x7c, 0x85];
        let mut fu_start = start.to_vec();
        fu_start.extend_from_slice(&[0xaa, 0xbb]);
        assert!(dep.depacketize(&fu_start).is_none());

        // …then a complete second sequence starts with S=1. It must not be
        // appended to the stale buffer.
        let body = [0x11, 0x22, 0x33];
        let mut second = [0x7c, 0x85].to_vec(); // S=1
        second.extend_from_slice(&body[..2]);
        assert!(dep.depacketize(&second).is_none());
        let mut end = [0x7c, 0x45].to_vec(); // E=1
        end.push(body[2]);
        let out = dep.depacketize(&end).expect("second sequence is clean");
        let mut expected = annexb(&[0x65]);
        expected.extend_from_slice(&body);
        assert_eq!(out.to_vec(), expected);
    }

    #[test]
    fn empty_and_short_packets_are_rejected() {
        let mut dep = H264Depacketizer::default();
        assert!(dep.depacketize(&[]).is_none());
        assert!(dep.depacketize(&[0x7c]).is_none(), "FU-A needs FU header");
    }

    #[test]
    fn keyframe_detection_across_start_codes() {
        // 4-byte start code + IDR.
        assert!(annexb_contains_keyframe(&annexb(&[0x65, 0x88, 0x84])));
        // 3-byte start code + SPS.
        let mut three = vec![0x00, 0x00, 0x01, 0x67, 0x42];
        assert!(annexb_contains_keyframe(&three));
        // Non-IDR slice: not a keyframe.
        three = vec![0x00, 0x00, 0x01, 0x41, 0x9a];
        assert!(!annexb_contains_keyframe(&three));
        // Keyframe hidden behind an earlier NAL in a multi-NAL chunk.
        let mut chunk = annexb(&[0x41, 0x9a]);
        chunk.extend_from_slice(&annexb(&[0x65, 0x88]));
        assert!(annexb_contains_keyframe(&chunk));
        assert!(!annexb_contains_keyframe(&[]));
        assert!(!annexb_contains_keyframe(&[0x00, 0x01, 0x67])); // no valid start code
    }
}
