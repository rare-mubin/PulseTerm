//! Audio: moonlight-common-c's Opus packets, decoded with libopus and played
//! through an SDL3 audio stream.
//!
//! moonlight-common-c calls `play` on its audio thread with one Opus
//! multistream packet (5 ms of sound by default). It is decoded there and
//! queued on the SDL stream, which feeds the default output device -- behind
//! a cushion that adapts to how unevenly the packets arrive (`Audio::target`):
//! it grows at once to cover the worst wait seen and eases back while they
//! come steadily. A queue above the cushion is drained by playing 2-4%
//! faster (below it, 2% slower), so nothing is cut; only far past it is a
//! packet dropped. When the queue runs dry, playback pauses until the cushion
//! is rebuilt.

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;
use std::sync::Mutex;

use sdl3_sys::everything::*;

use super::ffi;

/// The smallest cushion: 20 ms normally, 10 in low-latency mode. The cushion
/// itself adapts (`Audio::target`): packets should arrive every 5 ms, but in
/// practice they come in clumps -- measured on a real host, up to ~47 ms
/// apart even with the network at 2 ms (the host's audio capture hands
/// Sunshine sound in blocks, less evenly when it is busy) -- and a cushion
/// smaller than the clumps runs dry between them, which is the stutter.
pub static BASE_MS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(20);
/// The most the cushion may grow to.
const MOST_MS: f32 = 150.0;

/// Readings for the sidebar: times the queue ran dry, packets dropped for
/// being too far ahead, the longest wait between two packets arriving (µs),
/// and the queue's length at the last packet (ms).
pub static GAPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LATE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static QUEUED_MS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// The cushion as it stands, for the sidebar.
pub static TARGET_MS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(20);

#[link(name = "opus")]
extern "C" {
    fn opus_multistream_decoder_create(
        fs: i32,
        channels: c_int,
        streams: c_int,
        coupled_streams: c_int,
        mapping: *const u8,
        error: *mut c_int,
    ) -> *mut c_void;
    fn opus_multistream_decode(
        st: *mut c_void,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_multistream_decoder_destroy(st: *mut c_void);
}

struct Audio {
    decoder: *mut c_void,
    stream: *mut SDL_AudioStream,
    channels: i32,
    rate: i32,
    pcm: Vec<i16>,
    /// Paused, building the cushion back up after running dry.
    priming: bool,
    last: Option<std::time::Instant>,
    /// The cushion, in ms: grows at once to cover the worst wait between
    /// packets seen, shrinks slowly back while they come steadily.
    target: f32,
    last_shrink: std::time::Instant,
    /// The playback rate last set: a little over 1 while draining an
    /// over-full queue, a little under while it runs low.
    ratio: f32,
}

// Touched by moonlight-common-c's audio thread only, under the mutex.
unsafe impl Send for Audio {}

static AUDIO: Mutex<Option<Audio>> = Mutex::new(None);

/// Audio off in the settings mutes here, at once and mid-stream. Packets are
/// still decoded, so the decoder's state stays whole for unmuting.
pub static MUTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The callbacks. Always installed, so audio can be switched on mid-stream;
/// whether the host sends any is its Sunshine's `stream_audio`.
pub fn callbacks() -> ffi::AUDIO_RENDERER_CALLBACKS {
    let mut cb: ffi::AUDIO_RENDERER_CALLBACKS = unsafe { std::mem::zeroed() };
    unsafe { ffi::LiInitializeAudioCallbacks(&mut cb) };
    cb.init = Some(init);
    cb.cleanup = Some(cleanup);
    cb.decodeAndPlaySample = Some(play);
    // Decoding 5 ms packets with libopus is cheap; leave the host its short
    // packets, which keep latency low.
    cb.capabilities = 0;
    cb
}

unsafe extern "C" fn init(
    _configuration: c_int,
    opus: ffi::POPUS_MULTISTREAM_CONFIGURATION,
    _context: *mut c_void,
    _flags: c_int,
) -> c_int {
    let o = &*opus;
    let mut error = 0;
    let decoder =
        opus_multistream_decoder_create(o.sampleRate, o.channelCount, o.streams, o.coupledStreams, o.mapping.as_ptr(), &mut error);
    if decoder.is_null() || error != 0 {
        eprintln!("pulseterm-remote: Opus decoder failed ({error})");
        return -1;
    }
    if !SDL_InitSubSystem(SDL_INIT_AUDIO) {
        opus_multistream_decoder_destroy(decoder);
        eprintln!("pulseterm-remote: no audio device");
        return -1;
    }
    let spec = SDL_AudioSpec { format: SDL_AUDIO_S16, channels: o.channelCount, freq: o.sampleRate };
    let stream = SDL_OpenAudioDeviceStream(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, None, null_mut());
    if stream.is_null() {
        opus_multistream_decoder_destroy(decoder);
        eprintln!("pulseterm-remote: could not open the audio output");
        return -1;
    }
    // Starts paused: playback begins once the first cushion is queued.
    SDL_PauseAudioStreamDevice(stream);
    *AUDIO.lock().unwrap() = Some(Audio {
        decoder,
        stream,
        channels: o.channelCount,
        rate: o.sampleRate,
        pcm: vec![0; (o.samplesPerFrame.max(240) * o.channelCount) as usize * 2],
        priming: true,
        last: None,
        target: BASE_MS.load(std::sync::atomic::Ordering::Relaxed) as f32,
        last_shrink: std::time::Instant::now(),
        ratio: 1.0,
    });
    0
}

unsafe extern "C" fn cleanup() {
    if let Some(a) = AUDIO.lock().unwrap().take() {
        SDL_DestroyAudioStream(a.stream);
        opus_multistream_decoder_destroy(a.decoder);
    }
}

unsafe extern "C" fn play(data: *mut std::ffi::c_char, length: c_int) {
    let mut guard = AUDIO.lock().unwrap();
    let Some(a) = guard.as_mut() else { return };
    let now = std::time::Instant::now();
    let base = BASE_MS.load(std::sync::atomic::Ordering::Relaxed) as f32;
    if let Some(last) = a.last {
        let gap = now.duration_since(last);
        LATE_US.fetch_max(gap.as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        // Cover the worst clump seen, with a margin; ease back down while
        // the packets come steadily, so the delay does not stay high for
        // nothing.
        let need = (gap.as_secs_f32() * 1000.0 * 1.25 + 5.0).min(MOST_MS);
        if need > a.target {
            a.target = need;
            a.last_shrink = now;
        } else if now.duration_since(a.last_shrink).as_secs_f32() >= 2.0 {
            a.target = (a.target * 0.9).max(base);
            a.last_shrink = now;
        }
    }
    a.target = a.target.max(base);
    TARGET_MS.store(a.target as i32, std::sync::atomic::Ordering::Relaxed);
    a.last = Some(now);
    let frame = (a.pcm.len() as i32 / a.channels) as c_int;
    // A null packet (length 0) asks libopus to conceal a lost one.
    let samples = opus_multistream_decode(a.decoder, data as *const u8, length, a.pcm.as_mut_ptr(), frame, 0);
    if samples <= 0 {
        return;
    }
    if MUTED.load(std::sync::atomic::Ordering::Relaxed) {
        SDL_ClearAudioStream(a.stream);
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    let bytes_per_ms = a.rate / 1000 * a.channels * 2;
    let queued = SDL_GetAudioStreamQueued(a.stream);
    // Too far ahead (a burst arrived at once): drop this one packet -- a few
    // milliseconds -- rather than the whole queue, which was an audible hole.
    QUEUED_MS.store(queued / bytes_per_ms.max(1), Relaxed);
    // Steer the queue back to the cushion by playing a touch faster or
    // slower -- 2-4%, which the ear does not catch -- instead of dropping
    // packets. Measured on a real host: clumps left the queue at ~120 ms over
    // a 63 ms cushion, nothing ever brought it back down, and the next clump
    // tipped it over the old limit: 33 dropped packets in ten seconds of
    // moving the mouse, each one an audible click.
    let queued_ms = queued as f32 / bytes_per_ms.max(1) as f32;
    let ratio = if queued_ms > a.target * 1.6 {
        1.04
    } else if queued_ms > a.target * 1.15 {
        1.02
    } else if queued_ms < a.target * 0.5 && !a.priming {
        0.98
    } else {
        1.0
    };
    if ratio != a.ratio {
        a.ratio = ratio;
        SDL_SetAudioStreamFrequencyRatio(a.stream, ratio);
    }
    // Dropping is now only for the extreme: far past what steering drains.
    let ceiling = (a.target * 3.0 + 60.0).min(400.0) as i32;
    if queued > ceiling * bytes_per_ms {
        DROPS.fetch_add(1, Relaxed);
        return;
    }
    // Ran dry since the last packet: pause and rebuild a small cushion,
    // instead of playing each late packet into silence on either side.
    if queued == 0 && !a.priming {
        GAPS.fetch_add(1, Relaxed);
        a.priming = true;
        SDL_PauseAudioStreamDevice(a.stream);
    }
    let bytes = samples * a.channels * 2;
    SDL_PutAudioStreamData(a.stream, a.pcm.as_ptr() as *const c_void, bytes);
    if a.priming && queued + bytes >= a.target as i32 * bytes_per_ms {
        a.priming = false;
        SDL_ResumeAudioStreamDevice(a.stream);
    }
}
