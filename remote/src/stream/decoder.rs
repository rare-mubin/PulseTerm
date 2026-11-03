//! Video: moonlight-common-c's decoder callbacks, feeding FFmpeg.
//!
//! moonlight-common-c calls `submit` on its own thread with one whole frame
//! (Annex-B NAL units for H.264/HEVC, an OBU sequence for AV1). It is decoded
//! there in hardware (VA-API on Linux, D3D11VA on Windows -- `HW`), copied
//! down to an NV12 picture in system memory, and left
//! in `LATEST` for the window's thread, which is woken with an SDL event. Only
//! the newest picture is kept: a frame the screen never showed is not worth
//! showing late.
//!
//! The copy down to system memory costs ~3 ms at 1080p on this machine's AMD
//! iGPU (REMOTE_DESKTOP.md, phase 0) -- fine for the first milestone. Zero-copy
//! (dmabuf → EGL) replaces it later.

use std::ptr::{null, null_mut};
use std::sync::Mutex;

use ffmpeg_sys_next as ff;

use super::ffi;

/// The platform's hardware decoder: VA-API on Linux, Direct3D 11 on Windows.
/// Both hand back GPU surfaces that `av_hwframe_transfer_data` downloads to
/// NV12, so everything after `get_format` is the same; only zero-copy (a DRM
/// PRIME mapping, `zerocopy`) is VA-API's alone.
#[cfg(not(windows))]
const HW: (ff::AVHWDeviceType, ff::AVPixelFormat, &str) =
    (ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, ff::AVPixelFormat::AV_PIX_FMT_VAAPI, "VA-API");
#[cfg(windows)]
const HW: (ff::AVHWDeviceType, ff::AVPixelFormat, &str) =
    (ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, ff::AVPixelFormat::AV_PIX_FMT_D3D11, "D3D11VA");

/// One decoded picture, in whichever layout the decoder produced.
pub struct Picture {
    pub width: i32,
    pub height: i32,
    pub layout: Layout,
    /// Plane data and pitches: NV12 has two planes, I420 three.
    pub planes: Vec<(Vec<u8>, i32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Nv12,
    I420,
}

/// A decoded frame for the window: copied to system memory, or still on the
/// GPU as a DRM PRIME mapping (`zerocopy`).
pub enum Frame {
    Copied(Picture),
    Prime(super::zerocopy::Prime),
}

/// The newest decoded frame, waiting for the window.
pub static LATEST: Mutex<Option<Frame>> = Mutex::new(None);

/// Set by the window when it can import GPU frames directly; cleared for good
/// if a mapping or an import ever fails, so the copy path takes over.
pub static ZERO_COPY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Pictures decoded so far, for the frame-rate reading.
pub static DECODED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sums for the latency readings, in microseconds, and how many frames they
/// cover: time spent decoding here, and the host's own processing time per
/// frame (capture to encoded, as Sunshine reports it -- zero when it does not).
pub static DECODE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HOST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HOST_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Video bytes received (whole frames, before decoding): the picture's real
/// bitrate, as against the cap the setting asks for.
pub static VIDEO_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What the window wakes on when a picture lands (an SDL user event type).
pub static WAKE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct Decoder {
    ctx: *mut ff::AVCodecContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    software: *mut ff::AVFrame,
    buffer: Vec<u8>,
    hardware: bool,
}

// Used from one thread at a time (moonlight-common-c's decoder thread), under
// the mutex below.
unsafe impl Send for Decoder {}

static DECODER: Mutex<Option<Decoder>> = Mutex::new(None);

pub fn callbacks() -> ffi::DECODER_RENDERER_CALLBACKS {
    let mut cb: ffi::DECODER_RENDERER_CALLBACKS = unsafe { std::mem::zeroed() };
    unsafe { ffi::LiInitializeVideoCallbacks(&mut cb) };
    cb.setup = Some(setup);
    cb.cleanup = Some(cleanup);
    cb.submitDecodeUnit = Some(submit);
    // Decode straight from the network thread rather than through
    // moonlight-common-c's own queue: one hop less latency.
    cb.capabilities = ffi::CAPABILITY_DIRECT_SUBMIT as i32;
    cb
}

unsafe extern "C" fn get_format(
    _: *mut ff::AVCodecContext,
    mut formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    let mut fallback = ff::AVPixelFormat::AV_PIX_FMT_NONE;
    while *formats != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *formats == HW.1 {
            return *formats;
        }
        if fallback == ff::AVPixelFormat::AV_PIX_FMT_NONE {
            fallback = *formats;
        }
        formats = formats.add(1);
    }
    eprintln!("pulseterm-remote: no {} surface offered, decoding in software", HW.2);
    fallback
}

unsafe extern "C" fn setup(
    format: i32,
    width: i32,
    height: i32,
    _fps: i32,
    _context: *mut std::ffi::c_void,
    _flags: i32,
) -> i32 {
    let codec_id = if format & ffi::VIDEO_FORMAT_MASK_H264 as i32 != 0 {
        ff::AVCodecID::AV_CODEC_ID_H264
    } else if format & ffi::VIDEO_FORMAT_MASK_H265 as i32 != 0 {
        ff::AVCodecID::AV_CODEC_ID_HEVC
    } else if format & ffi::VIDEO_FORMAT_MASK_AV1 as i32 != 0 {
        ff::AVCodecID::AV_CODEC_ID_AV1
    } else {
        eprintln!("pulseterm-remote: unknown video format {format:#x}");
        return -1;
    };
    let codec = ff::avcodec_find_decoder(codec_id);
    if codec.is_null() {
        eprintln!("pulseterm-remote: FFmpeg has no decoder for {codec_id:?} (Fedora: RPM Fusion's ffmpeg)");
        return -1;
    }
    let ctx = ff::avcodec_alloc_context3(codec);
    (*ctx).width = width;
    (*ctx).height = height;
    (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
    let mut device: *mut ff::AVBufferRef = null_mut();
    let hardware = ff::av_hwdevice_ctx_create(&mut device, HW.0, null(), null_mut(), 0) == 0;
    if hardware {
        (*ctx).hw_device_ctx = ff::av_buffer_ref(device);
        // Spare surfaces: the frame on screen stays referenced until the next
        // replaces it (zero-copy), and the decoder must not run short.
        (*ctx).extra_hw_frames = 4;
        (*ctx).get_format = Some(get_format);
        ff::av_buffer_unref(&mut device);
    } else {
        // Software decoding wants threads; hardware does not.
        (*ctx).thread_count = 0;
    }
    if ff::avcodec_open2(ctx, codec, null_mut()) < 0 {
        eprintln!("pulseterm-remote: could not open the {codec_id:?} decoder");
        return -1;
    }
    eprintln!("pulseterm-remote: decoding {codec_id:?} {width}x{height}, {}: {hardware}", HW.2);
    *DECODER.lock().unwrap() = Some(Decoder {
        ctx,
        packet: ff::av_packet_alloc(),
        frame: ff::av_frame_alloc(),
        software: ff::av_frame_alloc(),
        buffer: Vec::with_capacity(1 << 20),
        hardware,
    });
    0
}

unsafe extern "C" fn cleanup() {
    if let Some(mut d) = DECODER.lock().unwrap().take() {
        ff::av_packet_free(&mut d.packet);
        ff::av_frame_free(&mut d.frame);
        ff::av_frame_free(&mut d.software);
        ff::avcodec_free_context(&mut d.ctx);
    }
}

unsafe extern "C" fn submit(unit: ffi::PDECODE_UNIT) -> i32 {
    use std::sync::atomic::Ordering::Relaxed;
    let started = std::time::Instant::now();
    VIDEO_BYTES.fetch_add((*unit).fullLength.max(0) as u64, Relaxed);
    let host = (*unit).frameHostProcessingLatency as u64; // tenths of a millisecond
    if host > 0 {
        HOST_US.fetch_add(host * 100, Relaxed);
        HOST_FRAMES.fetch_add(1, Relaxed);
    }
    let result = decode(unit);
    DECODE_US.fetch_add(started.elapsed().as_micros() as u64, Relaxed);
    result
}

unsafe fn decode(unit: ffi::PDECODE_UNIT) -> i32 {
    let mut guard = DECODER.lock().unwrap();
    let Some(d) = guard.as_mut() else { return ffi::DR_NEED_IDR };

    // One contiguous buffer, with the zeroed tail FFmpeg's parsers read past
    // the end into.
    d.buffer.clear();
    let mut entry = (*unit).bufferList;
    while !entry.is_null() {
        let e = &*entry;
        d.buffer.extend_from_slice(std::slice::from_raw_parts(e.data as *const u8, e.length as usize));
        entry = e.next;
    }
    let len = d.buffer.len();
    d.buffer.resize(len + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize, 0);

    (*d.packet).data = d.buffer.as_mut_ptr();
    (*d.packet).size = len as i32;
    (*d.packet).flags = if (*unit).frameType == ffi::FRAME_TYPE_IDR as i32 { ff::AV_PKT_FLAG_KEY as i32 } else { 0 };
    if ff::avcodec_send_packet(d.ctx, d.packet) < 0 {
        // A broken reference chain: ask the host for a key frame.
        return ffi::DR_NEED_IDR;
    }
    while ff::avcodec_receive_frame(d.ctx, d.frame) == 0 {
        use std::sync::atomic::Ordering::Relaxed;
        // Zero-copy: hand the window the surface itself, mapped to dma-buf
        // descriptors -- no download, no copy (`zerocopy`).
        if d.hardware && ZERO_COPY.load(Relaxed) && (*d.frame).format == ff::AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
            let mut mapped = ff::av_frame_alloc();
            (*mapped).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            if ff::av_hwframe_map(mapped, d.frame, ff::AV_HWFRAME_MAP_READ as i32) == 0 {
                let prime = super::zerocopy::Prime { frame: mapped, width: (*d.frame).width, height: (*d.frame).height };
                DECODED.fetch_add(1, Relaxed);
                *LATEST.lock().unwrap() = Some(Frame::Prime(prime));
                wake();
                ff::av_frame_unref(d.frame);
                continue;
            }
            ff::av_frame_free(&mut mapped);
            ZERO_COPY.store(false, Relaxed);
            eprintln!("pulseterm-remote: could not map frames for zero-copy; copying instead");
        }
        let source = if d.hardware && (*d.frame).format == HW.1 as i32 {
            ff::av_frame_unref(d.software);
            (*d.software).format = ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            if ff::av_hwframe_transfer_data(d.software, d.frame, 0) < 0 {
                ff::av_frame_unref(d.frame);
                return ffi::DR_NEED_IDR;
            }
            d.software
        } else {
            d.frame
        };
        if let Some(picture) = picture(source) {
            DECODED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *LATEST.lock().unwrap() = Some(Frame::Copied(picture));
            wake();
        }
        ff::av_frame_unref(d.frame);
    }
    ffi::DR_OK as i32
}

/// Copies a frame's planes out of FFmpeg's buffers.
unsafe fn picture(f: *const ff::AVFrame) -> Option<Picture> {
    let f = &*f;
    let (layout, heights): (Layout, Vec<i32>) = if f.format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
        (Layout::Nv12, vec![f.height, (f.height + 1) / 2])
    } else if f.format == ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32
        || f.format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32
    {
        (Layout::I420, vec![f.height, (f.height + 1) / 2, (f.height + 1) / 2])
    } else {
        eprintln!("pulseterm-remote: unsupported picture format {}", f.format);
        return None;
    };
    let planes = heights
        .iter()
        .enumerate()
        .map(|(i, &rows)| {
            let pitch = f.linesize[i];
            let bytes = std::slice::from_raw_parts(f.data[i], (pitch * rows) as usize).to_vec();
            (bytes, pitch)
        })
        .collect();
    Some(Picture { width: f.width, height: f.height, layout, planes })
}

fn wake() {
    let kind = WAKE.load(std::sync::atomic::Ordering::Relaxed);
    if kind == 0 {
        return;
    }
    unsafe {
        let mut event: sdl3_sys::everything::SDL_Event = std::mem::zeroed();
        event.r#type = kind;
        sdl3_sys::everything::SDL_PushEvent(&mut event);
    }
}
