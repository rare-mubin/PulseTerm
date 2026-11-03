//! Zero-copy display: a decoded frame never leaves the GPU.
//!
//! The copy path downloads every VA-API surface to system memory, copies it
//! again into a buffer, and uploads it to an SDL texture -- measured at ~10 ms
//! a frame for 1080p on cachyos's client, the largest delay on this side.
//! Here instead:
//!
//! 1. the decoder thread maps the surface to a DRM PRIME descriptor
//!    (`av_hwframe_map` → `AV_PIX_FMT_DRM_PRIME`): file descriptors of the
//!    GPU buffers, one layer per plane (Y as `R8`, CbCr as `GR88`);
//! 2. this thread imports each plane as an EGL image
//!    (`EGL_EXT_image_dma_buf_import`, with the buffer's format modifier) and
//!    binds it to a GL texture (`glEGLImageTargetTexture2DOES`);
//! 3. SDL draws the two textures as one NV12 texture, created around them
//!    (`SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_NUMBER` / `_UV_NUMBER`), with
//!    its own YUV shader.
//!
//! The mapped frame is kept until the next one replaces it, so the decoder
//! cannot reuse a surface still on screen. Any step that is not there --
//! another renderer than OpenGL, no EGL, no dma-buf import, an import that
//! fails -- switches back to the copy path, which stays correct everywhere.

use std::ffi::{c_void, CStr};
use std::ptr::null_mut;

use ffmpeg_sys_next as ff;
use sdl3_sys::everything::*;

/// A mapped frame on its way to the screen. Owns its `AVFrame` (and through
/// it a reference to the decoder's surface).
pub struct Prime {
    pub frame: *mut ff::AVFrame,
    pub width: i32,
    pub height: i32,
}

// Handed from the decoder thread to the window's; never shared.
unsafe impl Send for Prime {}

impl Drop for Prime {
    fn drop(&mut self) {
        unsafe { ff::av_frame_free(&mut self.frame) };
    }
}

type EglCreateImage = unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImage = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type EglQueryString = unsafe extern "C" fn(*mut c_void, i32) -> *const std::ffi::c_char;
type GlImageTarget = unsafe extern "C" fn(u32, *mut c_void);
type GlGenTextures = unsafe extern "C" fn(i32, *mut u32);
type GlDeleteTextures = unsafe extern "C" fn(i32, *const u32);
type GlBindTexture = unsafe extern "C" fn(u32, u32);
type GlTexParameteri = unsafe extern "C" fn(u32, u32, i32);
type GlGetIntegerv = unsafe extern "C" fn(u32, *mut i32);

const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;
const EGL_NONE: i32 = 0x3038;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_TEXTURE_SWIZZLE_A: u32 = 0x8E45;
const GL_GREEN: i32 = 0x1904;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
const DRM_FORMAT_R8: u32 = 0x2020_3852;
const DRM_FORMAT_GR88: u32 = 0x3838_5247;

pub struct ZeroCopy {
    display: *mut c_void,
    create_image: EglCreateImage,
    destroy_image: EglDestroyImage,
    image_target: GlImageTarget,
    bind: GlBindTexture,
    get_integer: GlGetIntegerv,
    delete_textures: GlDeleteTextures,
    /// GL textures for Y and CbCr, and the SDL texture made around them.
    gl: [u32; 2],
    /// The renderer is OpenGL ES 2, whose texture properties are its own.
    gles: bool,
    texture: *mut SDL_Texture,
    size: (i32, i32),
    images: [*mut c_void; 2],
    /// The frame on screen, kept so its surface is not reused under it.
    current: Option<Prime>,
}

impl ZeroCopy {
    /// Sets up on `renderer`'s GL context, or says why not.
    pub unsafe fn new(renderer: *mut SDL_Renderer) -> Result<Self, String> {
        let name = CStr::from_ptr(SDL_GetRendererName(renderer)).to_string_lossy().into_owned();
        let gles = match name.as_str() {
            "opengl" => false,
            "opengles2" => true,
            _ => return Err(format!("renderer is {name}, not OpenGL")),
        };
        let display = SDL_EGL_GetCurrentDisplay();
        if display.is_null() {
            return Err("no EGL display (GLX?)".into());
        }
        let egl = |n: &CStr| SDL_EGL_GetProcAddress(n.as_ptr());
        let gl = |n: &CStr| SDL_GL_GetProcAddress(n.as_ptr());
        macro_rules! load {
            ($f:expr, $n:literal, $t:ty) => {
                match $f($n) {
                    Some(p) => std::mem::transmute::<unsafe extern "C" fn(), $t>(p),
                    None => return Err(format!("{} missing", $n.to_string_lossy())),
                }
            };
        }
        let query: EglQueryString = load!(egl, c"eglQueryString", EglQueryString);
        let ext = query(display, EGL_EXTENSIONS);
        let ext = if ext.is_null() { String::new() } else { CStr::from_ptr(ext).to_string_lossy().into_owned() };
        if !ext.contains("EGL_EXT_image_dma_buf_import") {
            return Err("no EGL_EXT_image_dma_buf_import".into());
        }
        let create_image = load!(egl, c"eglCreateImageKHR", EglCreateImage);
        let destroy_image = load!(egl, c"eglDestroyImageKHR", EglDestroyImage);
        let image_target = load!(gl, c"glEGLImageTargetTexture2DOES", GlImageTarget);
        let gen: GlGenTextures = load!(gl, c"glGenTextures", GlGenTextures);
        let bind: GlBindTexture = load!(gl, c"glBindTexture", GlBindTexture);
        let param: GlTexParameteri = load!(gl, c"glTexParameteri", GlTexParameteri);
        let get_integer: GlGetIntegerv = load!(gl, c"glGetIntegerv", GlGetIntegerv);
        let delete_textures: GlDeleteTextures = load!(gl, c"glDeleteTextures", GlDeleteTextures);

        // Our own textures, on the renderer's context; SDL's idea of what is
        // bound is put back afterwards.
        SDL_FlushRenderer(renderer);
        let mut previous = 0;
        get_integer(GL_TEXTURE_BINDING_2D, &mut previous);
        let mut textures = [0u32; 2];
        gen(2, textures.as_mut_ptr());
        for &t in &textures {
            bind(GL_TEXTURE_2D, t);
            param(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            param(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            param(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            param(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        }
        // SDL's NV12 shader reads chroma as luminance-alpha: Cb from red, Cr
        // from *alpha*. The imported GR88 plane has Cr in green and alpha at
        // 1, which drew every picture red; read alpha from green instead.
        bind(GL_TEXTURE_2D, textures[1]);
        param(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_A, GL_GREEN);
        bind(GL_TEXTURE_2D, previous as u32);
        Ok(Self {
            display,
            create_image,
            destroy_image,
            image_target,
            bind,
            get_integer,
            delete_textures,
            gl: textures,
            gles,
            texture: null_mut(),
            size: (0, 0),
            images: [null_mut(); 2],
            current: None,
        })
    }

    /// Imports `frame` and returns the texture to draw, or why it failed.
    pub unsafe fn show(&mut self, renderer: *mut SDL_Renderer, frame: Prime) -> Result<*mut SDL_Texture, String> {
        let desc = &*((*frame.frame).data[0] as *const ff::AVDRMFrameDescriptor);
        // (fd, offset, pitch, fourcc, modifier) for the Y and CbCr planes:
        // two layers of one plane each (what FFmpeg's VA-API export gives),
        // or one NV12 layer of two planes.
        let mut planes = Vec::new();
        for l in 0..desc.nb_layers as usize {
            let layer = &desc.layers[l];
            for p in 0..layer.nb_planes as usize {
                let plane = &layer.planes[p];
                let object = &desc.objects[plane.object_index as usize];
                let fourcc = if desc.nb_layers == 1 {
                    if p == 0 { DRM_FORMAT_R8 } else { DRM_FORMAT_GR88 }
                } else {
                    layer.format
                };
                planes.push((object.fd, plane.offset as i32, plane.pitch as i32, fourcc, object.format_modifier));
            }
        }
        if planes.len() != 2 || planes[0].3 != DRM_FORMAT_R8 || planes[1].3 != DRM_FORMAT_GR88 {
            return Err(format!("unexpected layout: {} planes, formats {:?}", planes.len(), planes.iter().map(|p| p.3).collect::<Vec<_>>()));
        }

        let mut images: [*mut c_void; 2] = [null_mut(); 2];
        for (i, &(fd, offset, pitch, fourcc, modifier)) in planes.iter().enumerate() {
            let (w, h) = if i == 0 { (frame.width, frame.height) } else { ((frame.width + 1) / 2, (frame.height + 1) / 2) };
            let mut attrs = vec![
                EGL_WIDTH, w,
                EGL_HEIGHT, h,
                EGL_LINUX_DRM_FOURCC_EXT, fourcc as i32,
                EGL_DMA_BUF_PLANE0_FD_EXT, fd,
                EGL_DMA_BUF_PLANE0_OFFSET_EXT, offset,
                EGL_DMA_BUF_PLANE0_PITCH_EXT, pitch,
            ];
            if modifier != DRM_FORMAT_MOD_INVALID {
                attrs.extend_from_slice(&[
                    EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (modifier & 0xffff_ffff) as u32 as i32,
                    EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (modifier >> 32) as u32 as i32,
                ]);
            }
            attrs.push(EGL_NONE);
            let image = (self.create_image)(self.display, null_mut(), EGL_LINUX_DMA_BUF_EXT, null_mut(), attrs.as_ptr());
            if image.is_null() {
                for img in images.iter().filter(|i| !i.is_null()) {
                    (self.destroy_image)(self.display, *img);
                }
                return Err("eglCreateImageKHR refused the frame".into());
            }
            images[i] = image;
        }

        SDL_FlushRenderer(renderer);
        let mut previous = 0;
        (self.get_integer)(GL_TEXTURE_BINDING_2D, &mut previous);
        for (i, &image) in images.iter().enumerate() {
            (self.bind)(GL_TEXTURE_2D, self.gl[i]);
            (self.image_target)(GL_TEXTURE_2D, image);
        }
        (self.bind)(GL_TEXTURE_2D, previous as u32);

        if self.texture.is_null() || self.size != (frame.width, frame.height) {
            if !self.texture.is_null() {
                SDL_DestroyTexture(self.texture);
            }
            let props = SDL_CreateProperties();
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, SDL_PIXELFORMAT_NV12.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_STATIC.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, frame.width as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, frame.height as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, SDL_COLORSPACE_BT709_LIMITED.0 as i64);
            let (y, uv) = if self.gles {
                (SDL_PROP_TEXTURE_CREATE_OPENGLES2_TEXTURE_NUMBER, SDL_PROP_TEXTURE_CREATE_OPENGLES2_TEXTURE_UV_NUMBER)
            } else {
                (SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_NUMBER, SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_UV_NUMBER)
            };
            SDL_SetNumberProperty(props, y, self.gl[0] as i64);
            SDL_SetNumberProperty(props, uv, self.gl[1] as i64);
            self.texture = SDL_CreateTextureWithProperties(renderer, props);
            SDL_DestroyProperties(props);
            if self.texture.is_null() {
                for img in images {
                    (self.destroy_image)(self.display, img);
                }
                return Err(format!("SDL would not wrap the textures: {}", CStr::from_ptr(SDL_GetError()).to_string_lossy()));
            }
            self.size = (frame.width, frame.height);
        }

        // The previous frame's images and surface can go now.
        for img in std::mem::replace(&mut self.images, images) {
            if !img.is_null() {
                (self.destroy_image)(self.display, img);
            }
        }
        self.current = Some(frame);
        Ok(self.texture)
    }

    /// The texture last shown, for redrawing without a new frame.
    pub fn texture(&self) -> Option<(*mut SDL_Texture, (i32, i32))> {
        (!self.texture.is_null() && self.current.is_some()).then_some((self.texture, self.size))
    }
}

impl Drop for ZeroCopy {
    fn drop(&mut self) {
        unsafe {
            for img in self.images {
                if !img.is_null() {
                    (self.destroy_image)(self.display, img);
                }
            }
            if !self.texture.is_null() {
                SDL_DestroyTexture(self.texture);
            }
            (self.delete_textures)(2, self.gl.as_ptr());
        }
    }
}
