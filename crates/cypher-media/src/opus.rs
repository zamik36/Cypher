//! Safe wrapper over the libopus encoder (pure-Rust translation, no C
//! toolchain). This is the crate's only `unsafe`: the encoder state is a raw
//! pointer that this type owns exclusively from `create` to `destroy`.
#![allow(unsafe_code)]

use std::ptr::NonNull;

use unsafe_libopus::{
    OPUS_APPLICATION_VOIP, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_SET_BITRATE_REQUEST,
    OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE, opus_encode, opus_encoder_create, opus_encoder_ctl,
    opus_encoder_destroy,
};

use crate::{FRAME_SAMPLES, MediaError, SAMPLE_RATE};

/// Largest packet libopus produces for one frame (RFC 6716 §3.2.1).
pub const MAX_PACKET: usize = 1275;

pub struct OpusEncoder {
    st: NonNull<unsafe_libopus::OpusEncoder>,
}

// SAFETY: the encoder state has no thread affinity and is only reachable
// through `&mut self`, so moving it to another thread is sound.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    /// 48 kHz mono encoder tuned for speech at `bitrate` bits per second.
    pub fn voice(bitrate: i32) -> Result<Self, MediaError> {
        let mut err = 0;
        // SAFETY: arguments are valid constants; `err` outlives the call.
        let raw =
            unsafe { opus_encoder_create(SAMPLE_RATE as i32, 1, OPUS_APPLICATION_VOIP, &mut err) };
        let st = NonNull::new(raw).ok_or(MediaError::Encoder(err))?;
        let enc = Self { st };
        if err != OPUS_OK {
            return Err(MediaError::Encoder(err));
        }
        enc.ctl_set(OPUS_SET_BITRATE_REQUEST, bitrate)?;
        enc.ctl_set(OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE)?;
        Ok(enc)
    }

    /// Encoder delay in 48 kHz samples: the Ogg/WebM `pre_skip`.
    pub fn lookahead(&self) -> Result<u16, MediaError> {
        let mut value = 0i32;
        // SAFETY: `st` is live; the request writes one i32 through `value`.
        let rc =
            unsafe { opus_encoder_ctl!(self.st.as_ptr(), OPUS_GET_LOOKAHEAD_REQUEST, &mut value) };
        if rc != OPUS_OK {
            return Err(MediaError::Encoder(rc));
        }
        u16::try_from(value).map_err(|_| MediaError::Encoder(value))
    }

    /// Encodes exactly one 20 ms frame into `out`, returning the packet size.
    pub fn encode(
        &mut self,
        pcm: &[i16; FRAME_SAMPLES],
        out: &mut [u8; MAX_PACKET],
    ) -> Result<usize, MediaError> {
        // SAFETY: `pcm` holds FRAME_SAMPLES mono samples and `out` MAX_PACKET
        // writable bytes, matching the sizes passed alongside them.
        let n = unsafe {
            opus_encode(
                self.st.as_ptr(),
                pcm.as_ptr(),
                FRAME_SAMPLES as i32,
                out.as_mut_ptr(),
                MAX_PACKET as i32,
            )
        };
        usize::try_from(n).map_err(|_| MediaError::Encoder(n))
    }

    fn ctl_set(&self, request: i32, value: i32) -> Result<(), MediaError> {
        // SAFETY: `st` is live and `request` takes one i32 argument.
        let rc = unsafe { opus_encoder_ctl!(self.st.as_ptr(), request, value) };
        if rc == OPUS_OK {
            Ok(())
        } else {
            Err(MediaError::Encoder(rc))
        }
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: `st` came from `opus_encoder_create` and is freed once.
        unsafe { opus_encoder_destroy(self.st.as_ptr()) }
    }
}
