//! Decodes a device's H.264 stream with VideoToolbox into IOSurface-backed NV12 buffers GPUI's
//! `surface` element draws directly, turned on the GPU when the device is held sideways.

use std::ffi::c_void;
use std::ptr;

use core_foundation::base::{kCFAllocatorDefault, CFAllocatorRef, CFType, CFTypeRef, OSStatus, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef};

type CMFormatDescriptionRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type VTSessionRef = *mut c_void;
type CVPixelBufferPoolRef = *mut c_void;

/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`, the only layout GPUI's surface pipeline takes.
const PIXEL_FORMAT_NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const BLOCK_BUFFER_ASSURE_MEMORY_NOW: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type OutputCallback = extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CVPixelBufferRef, CMTime, CMTime);

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: OutputCallback,
    refcon: *mut c_void,
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_unit_header_length: i32,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory: *mut c_void,
        length: usize,
        block_allocator: CFAllocatorRef,
        custom: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(source: *const c_void, destination: CMBlockBufferRef, offset: usize, length: usize) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        samples: isize,
        timings: isize,
        timing: *const c_void,
        sizes_count: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        format: CMFormatDescriptionRef,
        decoder: CFDictionaryRef,
        destination: CFDictionaryRef,
        callback: *const VTDecompressionOutputCallbackRecord,
        out: *mut VTSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(session: VTSessionRef, sample: CMSampleBufferRef, flags: u32, refcon: *mut c_void, info: *mut u32) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: VTSessionRef);
    fn VTPixelRotationSessionCreate(allocator: CFAllocatorRef, out: *mut VTSessionRef) -> OSStatus;
    fn VTPixelRotationSessionRotateImage(session: VTSessionRef, source: CVPixelBufferRef, destination: CVPixelBufferRef) -> OSStatus;
    fn VTPixelRotationSessionInvalidate(session: VTSessionRef);
    fn VTSessionSetProperty(session: VTSessionRef, key: CFStringRef, value: CFTypeRef) -> OSStatus;
    static kVTPixelRotationPropertyKey_Rotation: CFStringRef;
    static kVTRotation_CW90: CFStringRef;
    static kVTRotation_180: CFStringRef;
    static kVTRotation_CCW90: CFStringRef;
    static kVTDecompressionPropertyKey_RealTime: CFStringRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    static kCVPixelBufferMetalCompatibilityKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;
    fn CVPixelBufferPoolCreate(allocator: CFAllocatorRef, pool: CFDictionaryRef, buffers: CFDictionaryRef, out: *mut CVPixelBufferPoolRef) -> i32;
    fn CVPixelBufferPoolCreatePixelBuffer(allocator: CFAllocatorRef, pool: CVPixelBufferPoolRef, out: *mut CVPixelBufferRef) -> i32;
    fn CVPixelBufferPoolRelease(pool: CVPixelBufferPoolRef);
}

extern "C" {
    fn CFRelease(value: *const c_void);
}

/// A decoded picture, safe to hand to the window's thread.
#[derive(Clone)]
pub struct Picture(pub CVPixelBuffer);

// SAFETY: a CVPixelBuffer is a reference-counted CoreFoundation object; CoreVideo allows it to be
// retained, released and read from any thread.
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

impl Picture {
    /// The buffer as the window paints it, holding its own reference.
    pub fn surface(&self) -> objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> {
        let buffer = std::ptr::NonNull::new(self.0.as_concrete_TypeRef().cast::<objc2_core_video::CVPixelBuffer>()).expect("a picture holds a buffer");
        // SAFETY: the pointer is a live CVPixelBuffer this picture holds a reference to.
        unsafe { objc2_core_foundation::CFRetained::retain(buffer) }
    }

    pub fn width(&self) -> usize {
        self.0.get_width()
    }

    pub fn height(&self) -> usize {
        self.0.get_height()
    }
}

/// The SPS and PPS an avcC record carries, and the size of its NAL length prefix.
pub fn parameter_sets(avcc: &[u8]) -> Option<(Vec<&[u8]>, i32)> {
    if avcc.len() < 7 || avcc[0] != 1 {
        return None;
    }
    let length_size = (avcc[4] & 0x03) as i32 + 1;
    let mut sets = Vec::new();
    let mut at = 5;
    let mut read_sets = |count: usize, at: &mut usize| -> Option<()> {
        for _ in 0..count {
            let size = u16::from_be_bytes([*avcc.get(*at)?, *avcc.get(*at + 1)?]) as usize;
            sets.push(avcc.get(*at + 2..*at + 2 + size)?);
            *at += 2 + size;
        }
        Some(())
    };
    let sps = (avcc[at] & 0x1f) as usize;
    at += 1;
    read_sets(sps, &mut at)?;
    let pps = *avcc.get(at)? as usize;
    at += 1;
    read_sets(pps, &mut at)?;
    (sets.len() >= 2).then_some((sets, length_size))
}

fn pixel_buffer_attributes(width: Option<usize>, height: Option<usize>) -> CFDictionary<CFString, CFType> {
    unsafe {
        let mut pairs: Vec<(CFString, CFType)> = vec![
            (CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey), CFNumber::from(PIXEL_FORMAT_NV12_FULL as i64).as_CFType()),
            (CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey), CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]).as_CFType()),
            (CFString::wrap_under_get_rule(kCVPixelBufferMetalCompatibilityKey), CFBoolean::true_value().as_CFType()),
        ];
        if let (Some(width), Some(height)) = (width, height) {
            pairs.push((CFString::wrap_under_get_rule(kCVPixelBufferWidthKey), CFNumber::from(width as i64).as_CFType()));
            pairs.push((CFString::wrap_under_get_rule(kCVPixelBufferHeightKey), CFNumber::from(height as i64).as_CFType()));
        }
        CFDictionary::from_CFType_pairs(&pairs)
    }
}

extern "C" fn decoded(_session: *mut c_void, frame: *mut c_void, status: OSStatus, _flags: u32, image: CVPixelBufferRef, _pts: CMTime, _duration: CMTime) {
    if status != 0 || image.is_null() || frame.is_null() {
        return;
    }
    // SAFETY: `frame` is the `Option<Picture>` slot the decode call below lends for the duration of a
    // synchronous decode, and `image` is a live pixel buffer the session hands this callback.
    unsafe {
        let slot = &mut *(frame as *mut Option<Picture>);
        *slot = Some(Picture(CVPixelBuffer::wrap_under_get_rule(image)));
    }
}

/// Turns the device's portrait picture a number of quarter turns clockwise.
pub struct Rotator {
    session: VTSessionRef,
    turns: u8,
    pool: CVPixelBufferPoolRef,
    pool_size: (usize, usize),
}

impl Rotator {
    pub fn new(turns: u8) -> Option<Self> {
        let mut session = ptr::null_mut();
        // SAFETY: plain VideoToolbox calls with a valid out pointer and constant keys.
        unsafe {
            if VTPixelRotationSessionCreate(kCFAllocatorDefault, &mut session) != 0 || session.is_null() {
                return None;
            }
            let angle = match turns % 4 {
                1 => kVTRotation_CW90,
                2 => kVTRotation_180,
                _ => kVTRotation_CCW90,
            };
            VTSessionSetProperty(session, kVTPixelRotationPropertyKey_Rotation, angle as CFTypeRef);
        }
        Some(Rotator { session, turns: turns % 4, pool: ptr::null_mut(), pool_size: (0, 0) })
    }

    pub fn turns(&self) -> u8 {
        self.turns
    }

    pub fn rotate(&mut self, picture: &Picture) -> Option<Picture> {
        let (width, height) = (picture.width(), picture.height());
        let size = if self.turns % 2 == 1 { (height, width) } else { (width, height) };
        // SAFETY: the pool is created for `size` and released before replacement; buffers come back
        // retained under the create rule.
        unsafe {
            if self.pool.is_null() || self.pool_size != size {
                if !self.pool.is_null() {
                    CVPixelBufferPoolRelease(self.pool);
                    self.pool = ptr::null_mut();
                }
                let attributes = pixel_buffer_attributes(Some(size.0), Some(size.1));
                if CVPixelBufferPoolCreate(kCFAllocatorDefault, ptr::null(), attributes.as_concrete_TypeRef(), &mut self.pool) != 0 {
                    self.pool = ptr::null_mut();
                    return None;
                }
                self.pool_size = size;
            }
            let mut buffer: CVPixelBufferRef = ptr::null_mut();
            if CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, self.pool, &mut buffer) != 0 || buffer.is_null() {
                return None;
            }
            let output = CVPixelBuffer::wrap_under_create_rule(buffer);
            if VTPixelRotationSessionRotateImage(self.session, picture.0.as_concrete_TypeRef(), output.as_concrete_TypeRef()) != 0 {
                return None;
            }
            Some(Picture(output))
        }
    }
}

impl Drop for Rotator {
    fn drop(&mut self) {
        // SAFETY: both were created by this rotator and are released once.
        unsafe {
            VTPixelRotationSessionInvalidate(self.session);
            CFRelease(self.session);
            if !self.pool.is_null() {
                CVPixelBufferPoolRelease(self.pool);
            }
        }
    }
}

/// One device's decoder. After a configure or an error it can only start again from a keyframe.
pub struct Decoder {
    session: VTSessionRef,
    format: CMFormatDescriptionRef,
    config: Vec<u8>,
    size: (u32, u32),
    needs_key: bool,
}

// SAFETY: the decoder is used from one thread at a time; VideoToolbox sessions are not tied to a thread.
unsafe impl Send for Decoder {}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Decoder { session: ptr::null_mut(), format: ptr::null_mut(), config: Vec::new(), size: (0, 0), needs_key: true }
    }

    /// The picture size the stream last said it has.
    pub fn size(&self) -> Option<(u32, u32)> {
        (self.size.0 > 0).then_some(self.size)
    }

    fn reset(&mut self) {
        // SAFETY: both were created by `configure` and are released once.
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.format.is_null() {
                CFRelease(self.format);
            }
        }
        self.session = ptr::null_mut();
        self.format = ptr::null_mut();
        self.config.clear();
        self.needs_key = true;
    }

    /// Takes a stream's codec setup. The same setup again keeps the session it already has.
    pub fn configure(&mut self, avcc: &[u8], width: u32, height: u32) -> bool {
        self.size = (width, height);
        if !self.session.is_null() && self.config == avcc {
            return true;
        }
        self.reset();
        let Some((sets, length_size)) = parameter_sets(avcc) else { return false };
        let pointers: Vec<*const u8> = sets.iter().map(|set| set.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(|set| set.len()).collect();
        // SAFETY: the parameter sets outlive the call; out pointers are valid; the callback record is
        // copied by VideoToolbox.
        unsafe {
            if CMVideoFormatDescriptionCreateFromH264ParameterSets(kCFAllocatorDefault, sets.len(), pointers.as_ptr(), sizes.as_ptr(), length_size, &mut self.format) != 0 {
                self.format = ptr::null_mut();
                return false;
            }
            let destination = pixel_buffer_attributes(None, None);
            let record = VTDecompressionOutputCallbackRecord { callback: decoded, refcon: ptr::null_mut() };
            if VTDecompressionSessionCreate(kCFAllocatorDefault, self.format, ptr::null(), destination.as_concrete_TypeRef(), &record, &mut self.session) != 0 {
                self.session = ptr::null_mut();
                self.reset();
                return false;
            }
            VTSessionSetProperty(self.session, kVTDecompressionPropertyKey_RealTime, CFBoolean::true_value().as_CFTypeRef());
        }
        self.config = avcc.to_vec();
        self.needs_key = true;
        true
    }

    /// Whether a chunk can be decoded now: a delta frame waits for the next keyframe after a reset.
    pub fn wants(&self, key: bool) -> bool {
        !self.session.is_null() && (key || !self.needs_key)
    }

    /// Notes that frames were skipped, so only a keyframe can follow.
    pub fn skip_to_key(&mut self) {
        self.needs_key = true;
    }

    /// Decodes one access unit.
    pub fn decode(&mut self, data: &[u8], key: bool) -> Option<Picture> {
        if !self.wants(key) || data.is_empty() {
            return None;
        }
        self.needs_key = false;
        let mut picture: Option<Picture> = None;
        // SAFETY: the block buffer allocates its own memory and copies `data`; `picture` outlives the
        // synchronous decode whose callback writes it; every created object is released here.
        let status = unsafe {
            let mut block: CMBlockBufferRef = ptr::null_mut();
            if CMBlockBufferCreateWithMemoryBlock(kCFAllocatorDefault, ptr::null_mut(), data.len(), kCFAllocatorDefault, ptr::null(), 0, data.len(), BLOCK_BUFFER_ASSURE_MEMORY_NOW, &mut block) != 0 {
                return None;
            }
            if CMBlockBufferReplaceDataBytes(data.as_ptr() as *const c_void, block, 0, data.len()) != 0 {
                CFRelease(block);
                return None;
            }
            let mut sample: CMSampleBufferRef = ptr::null_mut();
            let size = data.len();
            let created = CMSampleBufferCreateReady(kCFAllocatorDefault, block, self.format, 1, 0, ptr::null(), 1, &size, &mut sample);
            CFRelease(block);
            if created != 0 {
                return None;
            }
            let mut info = 0u32;
            let status = VTDecompressionSessionDecodeFrame(self.session, sample, 0, &mut picture as *mut Option<Picture> as *mut c_void, &mut info);
            CFRelease(sample);
            status
        };
        if status != 0 {
            let config = std::mem::take(&mut self.config);
            let size = self.size;
            self.reset();
            self.configure(&config, size.0, size.1);
            return None;
        }
        picture
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_yields_its_parameter_sets() {
        let avcc = [1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 3, 0x67, 1, 2, 1, 0, 2, 0x68, 9];
        let (sets, length) = parameter_sets(&avcc).unwrap();
        assert_eq!(length, 4);
        assert_eq!(sets, [&[0x67, 1, 2][..], &[0x68, 9][..]]);
        assert!(parameter_sets(&avcc[..10]).is_none());
    }
}
