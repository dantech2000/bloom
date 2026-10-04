// SPDX-License-Identifier: AGPL-3.0-or-later
//! GPU video path for macOS. mpv renders with OpenGL straight into a BGRA
//! IOSurface, and gpui composites that surface with Metal, so a frame never
//! passes through the CPU. The BGRA surface path is in the patched renderer
//! (`vendor/gpui-pre-apple`); stock gpui takes only NV12 surfaces.

use std::{
    ffi::{c_char, c_void},
    ptr,
};

use anyhow::{Result, anyhow};
use core_foundation::{
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    string::CFString,
};
use core_video::pixel_buffer::{
    CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferOpenGLCompatibilityKey, kCVPixelFormatType_32BGRA,
};

/// Buffers in rotation. gpui holds the newest one while the worker writes the
/// oldest, so the two never touch the same surface.
const POOL_SIZE: usize = 4;

/// One video frame on the GPU. Cheap to clone (a retain).
#[derive(Clone)]
pub struct VideoFrame(CVPixelBuffer);

// CVPixelBuffer is a thread-safe reference-counted object.
unsafe impl Send for VideoFrame {}
unsafe impl Sync for VideoFrame {}

impl VideoFrame {
    pub fn buffer(&self) -> CVPixelBuffer {
        self.0.clone()
    }
    #[cfg(test)]
    pub fn width(&self) -> usize {
        self.0.get_width()
    }
    #[cfg(test)]
    pub fn height(&self) -> usize {
        self.0.get_height()
    }
}

struct Target {
    buffer: CVPixelBuffer,
    texture: u32,
    fbo: u32,
}

/// An offscreen OpenGL context plus the surfaces mpv renders into.
/// It must be created, used and dropped on one thread.
pub struct GlRenderer {
    ctx: *mut c_void,
    size: (u32, u32),
    pool: Vec<Target>,
    next: usize,
}

impl GlRenderer {
    pub fn new() -> Result<Self> {
        let attribs = [
            gl::kCGLPFAOpenGLProfile,
            gl::kCGLOGLPVersion_3_2_Core,
            gl::kCGLPFAAccelerated,
            gl::kCGLPFAAllowOfflineRenderers,
            gl::kCGLPFAColorSize,
            24,
            0,
        ];
        let mut pix: *mut c_void = ptr::null_mut();
        let mut npix = 0;
        let mut ctx: *mut c_void = ptr::null_mut();
        unsafe {
            let code = gl::CGLChoosePixelFormat(attribs.as_ptr(), &mut pix, &mut npix);
            if code != 0 || pix.is_null() {
                return Err(anyhow!("CGLChoosePixelFormat failed ({code})"));
            }
            let code = gl::CGLCreateContext(pix, ptr::null_mut(), &mut ctx);
            gl::CGLReleasePixelFormat(pix);
            if code != 0 || ctx.is_null() {
                return Err(anyhow!("CGLCreateContext failed ({code})"));
            }
            let code = gl::CGLSetCurrentContext(ctx);
            if code != 0 {
                gl::CGLReleaseContext(ctx);
                return Err(anyhow!("CGLSetCurrentContext failed ({code})"));
            }
        }
        Ok(Self {
            ctx,
            size: (0, 0),
            pool: Vec::new(),
            next: 0,
        })
    }

    /// Returns the framebuffer mpv must render the next frame into.
    pub fn begin(&mut self, width: u32, height: u32) -> Result<i32> {
        if self.size != (width, height) {
            self.resize(width, height)?;
        }
        Ok(self.pool[self.next].fbo as i32)
    }

    /// Counts the black rows at the two edges of the frame mpv rendered
    /// since `begin`, in a few columns of the picture. Returns the smaller
    /// count when the two edges agree (a letterbox is the same at both), and
    /// `None` for a frame that is black as a whole or dark at one edge only.
    pub fn black_bars(&mut self) -> Option<u32> {
        // A bar is black; a compressed one has some noise.
        const DARK: u8 = 24;
        let (width, height) = self.size;
        let mut bars = [height; 2];
        let mut column = vec![0u8; height as usize * 4];
        unsafe { gl::glBindFramebuffer(gl::FRAMEBUFFER, self.pool[self.next].fbo) };
        for part in [0.2, 0.35, 0.5, 0.65, 0.8] {
            let x = (width as f32 * part) as i32;
            unsafe {
                gl::glReadPixels(
                    x,
                    0,
                    1,
                    height as i32,
                    gl::BGRA,
                    gl::UNSIGNED_BYTE,
                    column.as_mut_ptr().cast(),
                );
            }
            let bright = |pixel: &[u8]| pixel[..3].iter().any(|c| *c > DARK);
            let pixels = column.chunks_exact(4);
            let first = pixels.clone().position(bright).unwrap_or(height as usize);
            let last = pixels.rev().position(bright).unwrap_or(height as usize);
            bars[0] = bars[0].min(first as u32);
            bars[1] = bars[1].min(last as u32);
        }
        let agree = bars[0].abs_diff(bars[1]) <= (height / 100).max(2);
        (bars[0] < height / 2 && agree).then(|| bars[0].min(bars[1]))
    }

    /// The same count for the left and right edges, in a few rows of the
    /// picture: the bars of a narrow film in a 16:9 video.
    pub fn black_sides(&mut self) -> Option<u32> {
        const DARK: u8 = 24;
        let (width, height) = self.size;
        let mut bars = [width; 2];
        let mut row = vec![0u8; width as usize * 4];
        unsafe { gl::glBindFramebuffer(gl::FRAMEBUFFER, self.pool[self.next].fbo) };
        for part in [0.2, 0.35, 0.5, 0.65, 0.8] {
            let y = (height as f32 * part) as i32;
            unsafe {
                gl::glReadPixels(
                    0,
                    y,
                    width as i32,
                    1,
                    gl::BGRA,
                    gl::UNSIGNED_BYTE,
                    row.as_mut_ptr().cast(),
                );
            }
            let bright = |pixel: &[u8]| pixel[..3].iter().any(|c| *c > DARK);
            let pixels = row.chunks_exact(4);
            let first = pixels.clone().position(bright).unwrap_or(width as usize);
            let last = pixels.rev().position(bright).unwrap_or(width as usize);
            bars[0] = bars[0].min(first as u32);
            bars[1] = bars[1].min(last as u32);
        }
        let agree = bars[0].abs_diff(bars[1]) <= (width / 100).max(2);
        (bars[0] < width / 2 && agree).then(|| bars[0].min(bars[1]))
    }

    /// Returns the surface of the frame mpv rendered since `begin`.
    pub fn finish(&mut self) -> Result<VideoFrame> {
        let target = &self.pool[self.next];
        self.next = (self.next + 1) % self.pool.len();
        unsafe {
            gl::glBindFramebuffer(gl::FRAMEBUFFER, 0);
            // Metal reads the surface from another queue; submit the work now.
            gl::glFlush();
        }
        Ok(VideoFrame(target.buffer.clone()))
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.release_targets();
        for _ in 0..POOL_SIZE {
            let target = self.target(width, height)?;
            self.pool.push(target);
        }
        self.size = (width, height);
        self.next = 0;
        Ok(())
    }

    fn target(&self, width: u32, height: u32) -> Result<Target> {
        let key = |raw| unsafe { CFString::wrap_under_get_rule(raw) };
        let io_surface: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[]);
        let options = CFDictionary::from_CFType_pairs(&[
            (
                key(unsafe { kCVPixelBufferIOSurfacePropertiesKey }),
                io_surface.as_CFType(),
            ),
            (
                key(unsafe { kCVPixelBufferMetalCompatibilityKey }),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                key(unsafe { kCVPixelBufferOpenGLCompatibilityKey }),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);
        let buffer = CVPixelBuffer::new(
            kCVPixelFormatType_32BGRA,
            width as usize,
            height as usize,
            Some(&options),
        )
        .map_err(|code| anyhow!("CVPixelBufferCreate failed ({code})"))?;
        let surface = unsafe { gl::CVPixelBufferGetIOSurface(buffer.as_concrete_TypeRef().cast()) };
        if surface.is_null() {
            return Err(anyhow!("pixel buffer has no IOSurface"));
        }
        let mut texture = 0;
        let fbo = unsafe {
            gl::glGenTextures(1, &mut texture);
            gl::glBindTexture(gl::TEXTURE_RECTANGLE, texture);
            let code = gl::CGLTexImageIOSurface2D(
                self.ctx,
                gl::TEXTURE_RECTANGLE,
                gl::RGBA,
                width as i32,
                height as i32,
                gl::BGRA,
                gl::UNSIGNED_INT_8_8_8_8_REV,
                surface,
                0,
            );
            gl::glBindTexture(gl::TEXTURE_RECTANGLE, 0);
            if code != 0 {
                return Err(anyhow!("CGLTexImageIOSurface2D failed ({code})"));
            }
            framebuffer(gl::TEXTURE_RECTANGLE, texture)?
        };
        Ok(Target {
            buffer,
            texture,
            fbo,
        })
    }

    fn release_targets(&mut self) {
        unsafe {
            for target in self.pool.drain(..) {
                gl::glDeleteFramebuffers(1, &target.fbo);
                gl::glDeleteTextures(1, &target.texture);
            }
        }
        self.size = (0, 0);
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        self.release_targets();
        unsafe {
            gl::CGLSetCurrentContext(ptr::null_mut());
            gl::CGLReleaseContext(self.ctx);
        }
    }
}

/// Symbol lookup mpv uses to load OpenGL functions.
pub unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { gl::dlsym(gl::RTLD_DEFAULT, name) }
}

unsafe fn framebuffer(texture_target: u32, texture: u32) -> Result<u32> {
    let mut fbo = 0;
    unsafe {
        gl::glGenFramebuffers(1, &mut fbo);
        gl::glBindFramebuffer(gl::FRAMEBUFFER, fbo);
        gl::glFramebufferTexture2D(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            texture_target,
            texture,
            0,
        );
        let status = gl::glCheckFramebufferStatus(gl::FRAMEBUFFER);
        gl::glBindFramebuffer(gl::FRAMEBUFFER, 0);
        if status != gl::FRAMEBUFFER_COMPLETE {
            return Err(anyhow!("framebuffer incomplete ({status:#x})"));
        }
    }
    Ok(fbo)
}

#[allow(non_snake_case, non_upper_case_globals, clippy::too_many_arguments)]
mod gl {
    use std::ffi::{c_char, c_void};

    pub const kCGLPFAColorSize: i32 = 8;
    pub const kCGLPFAAccelerated: i32 = 73;
    pub const kCGLPFAAllowOfflineRenderers: i32 = 96;
    pub const kCGLPFAOpenGLProfile: i32 = 99;
    pub const kCGLOGLPVersion_3_2_Core: i32 = 0x3200;

    pub const RGBA: u32 = 0x1908;
    pub const BGRA: u32 = 0x80E1;
    pub const UNSIGNED_BYTE: u32 = 0x1401;
    pub const UNSIGNED_INT_8_8_8_8_REV: u32 = 0x8367;
    pub const TEXTURE_RECTANGLE: u32 = 0x84F5;
    pub const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
    pub const COLOR_ATTACHMENT0: u32 = 0x8CE0;
    pub const FRAMEBUFFER: u32 = 0x8D40;

    pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    #[link(name = "OpenGL", kind = "framework")]
    unsafe extern "C" {
        pub fn CGLChoosePixelFormat(
            attribs: *const i32,
            pix: *mut *mut c_void,
            npix: *mut i32,
        ) -> i32;
        pub fn CGLReleasePixelFormat(pix: *mut c_void);
        pub fn CGLCreateContext(
            pix: *mut c_void,
            share: *mut c_void,
            ctx: *mut *mut c_void,
        ) -> i32;
        pub fn CGLReleaseContext(ctx: *mut c_void);
        pub fn CGLSetCurrentContext(ctx: *mut c_void) -> i32;
        pub fn CGLTexImageIOSurface2D(
            ctx: *mut c_void,
            target: u32,
            internal_format: u32,
            width: i32,
            height: i32,
            format: u32,
            kind: u32,
            surface: *mut c_void,
            plane: u32,
        ) -> i32;

        pub fn glGenTextures(n: i32, textures: *mut u32);
        pub fn glDeleteTextures(n: i32, textures: *const u32);
        pub fn glBindTexture(target: u32, texture: u32);
        pub fn glGenFramebuffers(n: i32, fbos: *mut u32);
        pub fn glDeleteFramebuffers(n: i32, fbos: *const u32);
        pub fn glBindFramebuffer(target: u32, fbo: u32);
        pub fn glFramebufferTexture2D(
            target: u32,
            attachment: u32,
            texture_target: u32,
            texture: u32,
            level: i32,
        );
        pub fn glCheckFramebufferStatus(target: u32) -> u32;
        pub fn glFlush();
        pub fn glReadPixels(
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            format: u32,
            kind: u32,
            pixels: *mut c_void,
        );
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        pub fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;
    }

    unsafe extern "C" {
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
}
