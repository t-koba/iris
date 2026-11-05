//! CoreFoundation / CoreGraphics declarations shared by the ImageIO and PDF renderers.

use std::os::raw::c_void;

pub(crate) type CFTypeRef = *const c_void;
pub(crate) type CFURLRef = *const c_void;
pub(crate) type CFAllocatorRef = *const c_void;
pub(crate) type CFIndex = isize;
pub(crate) type CGColorSpaceRef = *const c_void;
pub(crate) type CGContextRef = *mut c_void;

pub(crate) const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CGPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CGSize {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CGRect {
    pub(crate) origin: CGPoint,
    pub(crate) size: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFRelease(cf: CFTypeRef);
    pub(crate) fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        path: *const u8,
        len: CFIndex,
        is_dir: bool,
    ) -> CFURLRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub(crate) fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub(crate) fn CGColorSpaceRelease(color_space: CGColorSpaceRef);
    pub(crate) fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        color_space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub(crate) fn CGContextRelease(context: CGContextRef);
}
