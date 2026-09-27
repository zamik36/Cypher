//! Minimal WebM muxer for a single mono Opus track.
//!
//! Frames are appended as they are encoded; `finish` lays out
//! `SeekHead, Info (with Duration), Tracks, Clusters…, Cues` in one buffer so
//! players know the length up front and can seek without scanning.

use crate::{FRAME_MS, SAMPLE_RATE};

const EBML: u32 = 0x1A45_DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PRE_ROLL: u32 = 0x56BB;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;

const TRACK: u64 = 1;
const TRACK_TYPE_AUDIO: u64 = 2;
/// One cluster per 5 s keeps block timestamps well inside `i16` and gives
/// players a cue point every few seconds.
const CLUSTER_MS: u64 = 5_000;
/// Opus decoders need 80 ms of preroll after a seek (RFC 7845 §4.6).
const SEEK_PRE_ROLL_NS: u64 = 80_000_000;
const APP: &str = "cypher";

pub struct OpusWebm {
    pre_skip: u16,
    frames: u64,
    clusters: Vec<u8>,
    /// `(timestamp_ms, offset of the cluster inside `clusters`)`.
    cues: Vec<(u64, u64)>,
    cluster: Vec<u8>,
    cluster_start_ms: u64,
}

impl OpusWebm {
    /// `pre_skip` is the encoder lookahead in 48 kHz samples.
    pub fn new(pre_skip: u16) -> Self {
        Self {
            pre_skip,
            frames: 0,
            clusters: Vec::new(),
            cues: Vec::new(),
            cluster: Vec::with_capacity(64 * 1024),
            cluster_start_ms: 0,
        }
    }

    /// Appends one 20 ms Opus packet.
    pub fn push(&mut self, packet: &[u8]) {
        let ts = self.frames * u64::from(FRAME_MS);
        if ts - self.cluster_start_ms >= CLUSTER_MS {
            self.close_cluster();
            self.cluster_start_ms = ts;
        }
        // Bounded by CLUSTER_MS, far below i16::MAX.
        let relative = (ts - self.cluster_start_ms) as i16;
        let mut block = Vec::with_capacity(4 + packet.len());
        block.push(0x80 | TRACK as u8);
        block.extend_from_slice(&relative.to_be_bytes());
        block.push(0x80);
        block.extend_from_slice(packet);
        element(&mut self.cluster, SIMPLE_BLOCK, &block);
        self.frames += 1;
    }

    pub fn duration_ms(&self) -> u64 {
        self.frames * u64::from(FRAME_MS)
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.close_cluster();

        let mut info = Vec::new();
        uint(&mut info, TIMESTAMP_SCALE, 1_000_000);
        float(&mut info, DURATION, self.duration_ms() as f64);
        string(&mut info, MUXING_APP, APP);
        string(&mut info, WRITING_APP, APP);

        let mut tracks = Vec::new();
        element(&mut tracks, TRACK_ENTRY, &self.track_entry());

        // Seek positions are written as fixed 8-byte integers so the SeekHead
        // size does not depend on the offsets it contains.
        let seek_head_len = self.seek_head(0, 0, 0).len() as u64;
        let info_pos = seek_head_len;
        let tracks_pos = info_pos + wrapped_len(INFO, info.len());
        let clusters_pos = tracks_pos + wrapped_len(TRACKS, tracks.len());
        let cues_pos = clusters_pos + self.clusters.len() as u64;

        let mut cues = Vec::new();
        for &(time, offset) in &self.cues {
            let mut position = Vec::new();
            uint(&mut position, CUE_TRACK, TRACK);
            uint(&mut position, CUE_CLUSTER_POSITION, clusters_pos + offset);
            let mut point = Vec::new();
            uint(&mut point, CUE_TIME, time);
            element(&mut point, CUE_TRACK_POSITIONS, &position);
            element(&mut cues, CUE_POINT, &point);
        }

        let mut segment = self.seek_head(info_pos, tracks_pos, cues_pos);
        element(&mut segment, INFO, &info);
        element(&mut segment, TRACKS, &tracks);
        segment.extend_from_slice(&self.clusters);
        element(&mut segment, CUES, &cues);

        let mut out = Vec::with_capacity(segment.len() + 64);
        element(&mut out, EBML, &ebml_header());
        element(&mut out, SEGMENT, &segment);
        out
    }

    fn close_cluster(&mut self) {
        if self.cluster.is_empty() {
            return;
        }
        self.cues
            .push((self.cluster_start_ms, self.clusters.len() as u64));
        let mut body = Vec::with_capacity(self.cluster.len() + 8);
        uint(&mut body, TIMESTAMP, self.cluster_start_ms);
        body.append(&mut self.cluster);
        element(&mut self.clusters, CLUSTER, &body);
    }

    fn track_entry(&self) -> Vec<u8> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(1);
        head.extend_from_slice(&self.pre_skip.to_le_bytes());
        head.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);

        let mut audio = Vec::new();
        float(&mut audio, SAMPLING_FREQUENCY, f64::from(SAMPLE_RATE));
        uint(&mut audio, CHANNELS, 1);

        let mut entry = Vec::new();
        uint(&mut entry, TRACK_NUMBER, TRACK);
        uint(&mut entry, TRACK_UID, TRACK);
        uint(&mut entry, TRACK_TYPE, TRACK_TYPE_AUDIO);
        string(&mut entry, CODEC_ID, "A_OPUS");
        element(&mut entry, CODEC_PRIVATE, &head);
        uint(
            &mut entry,
            CODEC_DELAY,
            u64::from(self.pre_skip) * 1_000_000_000 / u64::from(SAMPLE_RATE),
        );
        uint(&mut entry, SEEK_PRE_ROLL, SEEK_PRE_ROLL_NS);
        element(&mut entry, AUDIO, &audio);
        entry
    }

    fn seek_head(&self, info: u64, tracks: u64, cues: u64) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, pos) in [(INFO, info), (TRACKS, tracks), (CUES, cues)] {
            let mut seek = Vec::new();
            element(&mut seek, SEEK_ID, &id_bytes(id));
            element(&mut seek, SEEK_POSITION, &pos.to_be_bytes());
            element(&mut body, SEEK, &seek);
        }
        let mut out = Vec::new();
        element(&mut out, SEEK_HEAD, &body);
        out
    }
}

fn ebml_header() -> Vec<u8> {
    let mut h = Vec::new();
    uint(&mut h, EBML_VERSION, 1);
    uint(&mut h, EBML_READ_VERSION, 1);
    uint(&mut h, EBML_MAX_ID_LENGTH, 4);
    uint(&mut h, EBML_MAX_SIZE_LENGTH, 8);
    string(&mut h, DOC_TYPE, "webm");
    uint(&mut h, DOC_TYPE_VERSION, 4);
    uint(&mut h, DOC_TYPE_READ_VERSION, 2);
    h
}

/// Element IDs already carry their length marker; drop leading zero bytes.
fn id_bytes(id: u32) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count();
    bytes[skip..].to_vec()
}

fn size_len(size: u64) -> usize {
    (1..=8).find(|&n| size < (1u64 << (7 * n)) - 1).unwrap_or(8)
}

fn put_size(out: &mut Vec<u8>, size: u64) {
    let n = size_len(size);
    let marked = size | (1u64 << (7 * n));
    out.extend_from_slice(&marked.to_be_bytes()[8 - n..]);
}

fn wrapped_len(id: u32, body: usize) -> u64 {
    (id_bytes(id).len() + size_len(body as u64) + body) as u64
}

fn element(out: &mut Vec<u8>, id: u32, body: &[u8]) {
    out.extend_from_slice(&id_bytes(id));
    put_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
    element(out, id, &bytes[skip..]);
}

fn float(out: &mut Vec<u8>, id: u32, v: f64) {
    element(out, id, &v.to_be_bytes());
}

fn string(out: &mut Vec<u8>, id: u32, s: &str) {
    element(out, id, s.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use matroska_demuxer::{Frame, MatroskaFile, TrackType};

    use super::*;

    fn mux(frames: u64) -> Vec<u8> {
        let mut w = OpusWebm::new(312);
        for i in 0..frames {
            w.push(&[0xFC, i as u8, 0xAA]);
        }
        assert_eq!(w.duration_ms(), frames * 20);
        w.finish()
    }

    #[test]
    fn element_sizes_use_shortest_vint() {
        let mut out = Vec::new();
        put_size(&mut out, 0);
        put_size(&mut out, 126);
        put_size(&mut out, 127);
        put_size(&mut out, 16_382);
        assert_eq!(out, [0x80, 0xFE, 0x40, 0x7F, 0x7F, 0xFE]);
    }

    #[test]
    fn demuxes_with_duration_codec_and_every_frame() {
        let frames = 777;
        let file = mux(frames);
        let mut mkv = MatroskaFile::open(Cursor::new(file)).expect("valid webm");
        assert_eq!(mkv.info().timestamp_scale().get(), 1_000_000);
        assert_eq!(mkv.info().duration(), Some((frames * 20) as f64));

        let track = &mkv.tracks()[0];
        assert_eq!(track.track_type(), TrackType::Audio);
        assert_eq!(track.codec_id(), "A_OPUS");
        assert_eq!(track.codec_delay(), Some(6_500_000));
        let head = track.codec_private().expect("OpusHead");
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312);
        let audio = track.audio().expect("audio settings");
        assert_eq!(audio.channels().get(), 1);
        assert_eq!(audio.sampling_frequency(), 48_000.0);

        let mut frame = Frame::default();
        let mut seen = 0u64;
        while mkv.next_frame(&mut frame).expect("frame") {
            assert_eq!(frame.timestamp, seen * 20);
            assert_eq!(frame.data, [0xFC, seen as u8, 0xAA]);
            seen += 1;
        }
        assert_eq!(seen, frames);
    }

    #[test]
    fn seeks_through_cues() {
        let mut mkv = MatroskaFile::open(Cursor::new(mux(1000))).expect("valid webm");
        mkv.seek(12_345).expect("seek");
        let mut frame = Frame::default();
        assert!(mkv.next_frame(&mut frame).expect("frame"));
        assert_eq!(
            frame.timestamp, 12_360,
            "first frame at or after the target"
        );
    }

    #[test]
    fn single_frame_file_is_valid() {
        let mkv = MatroskaFile::open(Cursor::new(mux(1))).expect("valid webm");
        assert_eq!(mkv.info().duration(), Some(20.0));
    }
}
