use crate::encoded::{
    can_passthrough_encoded_image, passthrough_encoded_image, probe_encoded_image_bytes,
    probe_encoded_image_path, EncodedImageInfo,
};
use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use std::path::Path;
use tracing::debug;

/// Standard formats go through `image`; AVIF, JPEG 2000/XR/XL and HEIC use the shared decoders.
/// The image is fully decoded, resized and re-encoded as JPEG.
pub fn extract_image_thumbnail(file_path: &Path, hint: &ThumbnailHint) -> Option<ThumbnailResult> {
    if let Some(info) = probe_encoded_image_path(file_path) {
        if can_passthrough_encoded_image(&info, hint.max_dimension, &hint.preferred_formats) {
            let bytes = std::fs::read(file_path).ok()?;
            return Some(passthrough_encoded_image(
                bytes,
                info,
                ThumbnailSource::OriginalEncoded,
            ));
        }
    }

    match image::open(file_path) {
        Ok(img) => {
            return encode_resized_image(img, hint.max_dimension, Some(file_path));
        }
        Err(e) => {
            debug!(
                "image::open failed for {:?}: {}, trying modern image decoders",
                file_path, e
            );
        }
    }

    if let Some(img) = crate::modern_image::decode_path(file_path) {
        return encode_resized_image(img, hint.max_dimension, Some(file_path));
    }

    if let Some(thumbnail) = crate::video::extract_still_thumbnail(file_path, hint.max_dimension) {
        return Some(thumbnail);
    }

    // Variants the codecs cannot handle fall back to the OS API.
    native_decode(file_path, hint.max_dimension)
}

/// OS-native decoding: ImageIO on macOS, WIC on Windows, `None` on Linux.
fn native_decode(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    #[cfg(target_os = "macos")]
    {
        macos_imageio::decode_with_imageio(file_path, max_dimension)
    }
    #[cfg(target_os = "windows")]
    {
        windows_wic::decode_with_wic(file_path, max_dimension)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (file_path, max_dimension);
        None
    }
}

pub fn extract_image_thumbnail_from_bytes(
    data: &[u8],
    hint: &ThumbnailHint,
) -> Option<ThumbnailResult> {
    if let Some(info) = probe_passthrough_candidate(data, hint) {
        return Some(passthrough_encoded_image(
            data.to_vec(),
            info,
            ThumbnailSource::OriginalEncoded,
        ));
    }

    if let Some(img) = image::load_from_memory(data)
        .ok()
        .or_else(|| crate::modern_image::decode_bytes(data))
    {
        return encode_resized_image(img, hint.max_dimension, None);
    }

    let extension = crate::modern_image::ffmpeg_extension(data)?;
    if let Some(thumbnail) =
        crate::video::extract_still_thumbnail_from_bytes(data, extension, hint.max_dimension)
    {
        return Some(thumbnail);
    }

    native_decode_bytes(data, extension, hint.max_dimension)
}

/// In-memory images reach the OS API through a temporary file; used for remote files
/// in variants the shared decoders do not support.
fn native_decode_bytes(
    data: &[u8],
    extension: &str,
    max_dimension: u32,
) -> Option<ThumbnailResult> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        use std::io::Write;

        let suffix = format!(".{extension}");
        let mut file = tempfile::Builder::new().suffix(&suffix).tempfile().ok()?;
        file.write_all(data).ok()?;
        file.flush().ok()?;
        native_decode(file.path(), max_dimension)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (data, extension, max_dimension);
        None
    }
}

pub fn probe_passthrough_candidate(data: &[u8], hint: &ThumbnailHint) -> Option<EncodedImageInfo> {
    let info = probe_encoded_image_bytes(data)?;
    can_passthrough_encoded_image(&info, hint.max_dimension, &hint.preferred_formats)
        .then_some(info)
}

fn encode_resized_image(
    img: image::DynamicImage,
    max_dimension: u32,
    file_path: Option<&Path>,
) -> Option<ThumbnailResult> {
    let resized = if img.width() > max_dimension || img.height() > max_dimension {
        img.thumbnail(max_dimension, max_dimension)
    } else {
        img
    };

    let resized = resized.to_rgb8();
    let (w, h) = resized.dimensions();
    let mut buf = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70);
    encoder.encode_image(&resized).ok()?;

    if let Some(file_path) = file_path {
        debug!(
            "generated thumbnail for generic image: {:?} ({}x{})",
            file_path, w, h
        );
    } else {
        debug!(
            "generated thumbnail for in-memory generic image ({}x{})",
            w, h
        );
    }

    Some(ThumbnailResult {
        data: buf,
        format: ImageFormat::Jpeg,
        width: w,
        height: h,
        source: ThumbnailSource::FullDecode,
    })
}

/// ImageIO decoding for formats the `image` crate lacks, such as HEIC/HEIF.
#[cfg(target_os = "macos")]
mod macos_imageio {
    use super::*;
    use crate::macos_ffi::*;
    use std::ffi::CString;
    use std::os::raw::c_void;

    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFNumberRef = *const c_void;
    type CGImageSourceRef = *const c_void;
    type CGImageRef = *const c_void;

    const K_CF_ALLOCATOR_DEFAULT: CFAllocatorRef = std::ptr::null();

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFNumberCreate(
            alloc: CFAllocatorRef,
            the_type: CFIndex,
            value_ptr: *const c_void,
        ) -> CFNumberRef;

        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFBooleanTrue: CFTypeRef;
    }

    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCreateWithURL(url: CFURLRef, options: CFDictionaryRef) -> CGImageSourceRef;
        fn CGImageSourceCreateThumbnailAtIndex(
            source: CGImageSourceRef,
            index: usize,
            options: CFDictionaryRef,
        ) -> CGImageRef;

        static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
        static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
        static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGImageGetWidth(image: CGImageRef) -> usize;
        fn CGImageGetHeight(image: CGImageRef) -> usize;
        fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
        fn CGImageRelease(image: CGImageRef);
    }

    pub fn decode_with_imageio(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
        let c_path = CString::new(file_path.to_str()?).ok()?;
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                K_CF_ALLOCATOR_DEFAULT,
                c_path.as_ptr() as *const u8,
                c_path.as_bytes().len() as CFIndex,
                false,
            );
            if url.is_null() {
                debug!("ImageIO: URL creation failed for {:?}", file_path);
                return None;
            }

            let source = CGImageSourceCreateWithURL(url, std::ptr::null());
            CFRelease(url);
            if source.is_null() {
                debug!("ImageIO: source creation failed for {:?}", file_path);
                return None;
            }

            // Options: max pixel size, apply transform, always create a thumbnail.
            let max_px = max_dimension as i32;
            // kCFNumberSInt32Type = 3
            let num = CFNumberCreate(
                K_CF_ALLOCATOR_DEFAULT,
                3,
                &max_px as *const i32 as *const c_void,
            );

            let keys: [CFTypeRef; 3] = [
                kCGImageSourceCreateThumbnailFromImageAlways as CFTypeRef,
                kCGImageSourceThumbnailMaxPixelSize as CFTypeRef,
                kCGImageSourceCreateThumbnailWithTransform as CFTypeRef,
            ];
            let values: [CFTypeRef; 3] = [kCFBooleanTrue, num as CFTypeRef, kCFBooleanTrue];
            let opts = CFDictionaryCreate(
                K_CF_ALLOCATOR_DEFAULT,
                keys.as_ptr(),
                values.as_ptr(),
                3,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            );

            let thumb = CGImageSourceCreateThumbnailAtIndex(source, 0, opts);
            CFRelease(opts as CFTypeRef);
            CFRelease(num as CFTypeRef);
            CFRelease(source);

            if thumb.is_null() {
                debug!("ImageIO: thumbnail creation failed for {:?}", file_path);
                return None;
            }

            let w = CGImageGetWidth(thumb);
            let h = CGImageGetHeight(thumb);
            if w == 0 || h == 0 {
                CGImageRelease(thumb);
                return None;
            }

            let bytes_per_row = w * 4;
            let mut pixel_data = vec![0u8; bytes_per_row * h];
            let color_space = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(
                pixel_data.as_mut_ptr() as *mut c_void,
                w,
                h,
                8,
                bytes_per_row,
                color_space,
                K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
            );
            CGColorSpaceRelease(color_space);

            if ctx.is_null() {
                CGImageRelease(thumb);
                return None;
            }

            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: w as f64,
                    height: h as f64,
                },
            };
            CGContextDrawImage(ctx, rect, thumb);
            CGContextRelease(ctx);
            CGImageRelease(thumb);

            let rgba = image::RgbaImage::from_raw(w as u32, h as u32, pixel_data)?;
            let img = image::DynamicImage::ImageRgba8(rgba);

            let mut buf = Vec::new();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70);
            encoder.encode_image(&img).ok()?;

            debug!(
                "generated thumbnail via macOS ImageIO: {:?} ({}x{})",
                file_path, w, h
            );

            Some(ThumbnailResult {
                data: buf,
                format: ImageFormat::Jpeg,
                width: w as u32,
                height: h as u32,
                source: ThumbnailSource::FullDecode,
            })
        }
    }
}

/// Windows Imaging Component decoding: HEIC (Windows 10 1809+), RAW depending on installed codecs.
/// Uses COM; mind the threading model.
#[cfg(target_os = "windows")]
mod windows_wic {
    use super::*;
    use std::os::raw::c_void;

    type HRESULT = i32;
    type UINT = u32;
    type ULONG = u32;
    type LPCWSTR = *const u16;

    #[repr(C)]
    struct GUID {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    // IUnknown vtable
    #[repr(C)]
    struct IUnknownVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut c_void) -> ULONG,
        release: unsafe extern "system" fn(*mut c_void) -> ULONG,
    }

    // IWICImagingFactory vtable (only the entries used).
    #[repr(C)]
    struct IWICImagingFactoryVtbl {
        // IUnknown
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut c_void) -> ULONG,
        release: unsafe extern "system" fn(*mut c_void) -> ULONG,
        // IWICImagingFactory
        create_decoder_from_filename: unsafe extern "system" fn(
            *mut c_void,      // this
            LPCWSTR,          // filename
            *const GUID,      // vendor (NULL)
            u32,              // desired access (GENERIC_READ)
            u32,              // metadata option (0 = default)
            *mut *mut c_void, // out: IWICBitmapDecoder
        ) -> HRESULT,
        _create_decoder_from_stream: *const c_void,
        _create_decoder_from_file_handle: *const c_void,
        _create_component_info: *const c_void,
        _create_decoder: *const c_void,
        _create_encoder: *const c_void,
        _create_palette: *const c_void,
        create_format_converter: unsafe extern "system" fn(
            *mut c_void,      // this
            *mut *mut c_void, // out: IWICFormatConverter
        ) -> HRESULT,
        create_bitmap_scaler: unsafe extern "system" fn(
            *mut c_void,      // this
            *mut *mut c_void, // out: IWICBitmapScaler
        ) -> HRESULT,
    }

    // IWICBitmapDecoder vtable (only the entries used).
    #[repr(C)]
    struct IWICBitmapDecoderVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut c_void) -> ULONG,
        release: unsafe extern "system" fn(*mut c_void) -> ULONG,
        _query_capability: *const c_void,
        _initialize: *const c_void,
        _get_container_format: *const c_void,
        _get_decoder_info: *const c_void,
        _copy_palette: *const c_void,
        _get_metadata_query_reader: *const c_void,
        _get_preview: *const c_void,
        _get_color_contexts: *const c_void,
        _get_thumbnail: *const c_void,
        _get_frame_count: *const c_void,
        get_frame: unsafe extern "system" fn(
            *mut c_void,      // this
            UINT,             // index
            *mut *mut c_void, // out: IWICBitmapFrameDecode
        ) -> HRESULT,
    }

    // IWICBitmapSource vtable, shared by IWICBitmapFrameDecode, IWICFormatConverter and IWICBitmapScaler.
    #[repr(C)]
    struct IWICBitmapSourceVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut c_void) -> ULONG,
        release: unsafe extern "system" fn(*mut c_void) -> ULONG,
        get_size: unsafe extern "system" fn(*mut c_void, *mut UINT, *mut UINT) -> HRESULT,
        _get_pixel_format: *const c_void,
        _get_resolution: *const c_void,
        _copy_palette: *const c_void,
        copy_pixels: unsafe extern "system" fn(
            *mut c_void,   // this
            *const c_void, // rect (NULL = all)
            UINT,          // stride
            UINT,          // buffer size
            *mut u8,       // buffer
        ) -> HRESULT,
    }

    // IWICFormatConverter vtable. `base` must stay the first field: COM vtables place the
    // parent interface's entries at the start.
    #[repr(C)]
    struct IWICFormatConverterVtbl {
        base: IWICBitmapSourceVtbl,
        initialize: unsafe extern "system" fn(
            *mut c_void, // this
            *mut c_void, // source (IWICBitmapSource)
            *const GUID, // dest format
            u32,         // dither (0 = none)
            *mut c_void, // palette (NULL)
            f64,         // alpha threshold
            u32,         // palette translate (0)
        ) -> HRESULT,
    }

    // IWICBitmapScaler vtable: IWICBitmapSource entries followed by its own method.
    #[repr(C)]
    struct IWICBitmapScalerVtbl {
        base: IWICBitmapSourceVtbl,
        initialize: unsafe extern "system" fn(
            *mut c_void, // this
            *mut c_void, // source (IWICBitmapSource)
            UINT,        // width
            UINT,        // height
            u32,         // interpolation mode (1 = Fant)
        ) -> HRESULT,
    }

    const CLSID_WIC_IMAGING_FACTORY: GUID = GUID {
        data1: 0xcacaf262,
        data2: 0x9370,
        data3: 0x4615,
        data4: [0xa1, 0x3b, 0x9f, 0x55, 0x39, 0xda, 0x4c, 0x0a],
    };
    const IID_IWIC_IMAGING_FACTORY: GUID = GUID {
        data1: 0xec5ec8a9,
        data2: 0xc395,
        data3: 0x4314,
        data4: [0x9c, 0x77, 0x54, 0xd7, 0xa9, 0x35, 0xff, 0x70],
    };
    const GUID_WIC_PIXEL_FORMAT_32BPP_RGBA: GUID = GUID {
        data1: 0xf5c7ad2d,
        data2: 0x6a8d,
        data3: 0x43dd,
        data4: [0xa7, 0xa8, 0xa2, 0x99, 0x35, 0x26, 0x1a, 0xe9],
    };

    const GENERIC_READ: u32 = 0x80000000;
    const COINIT_MULTITHREADED: u32 = 0;
    const CLSCTX_INPROC_SERVER: u32 = 1;
    const WIC_BITMAP_INTERPOLATION_FANT: u32 = 1;

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> HRESULT;
        fn CoCreateInstance(
            rclsid: *const GUID,
            outer: *mut c_void,
            ctx: u32,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> HRESULT;
    }

    /// RAII wrapper that releases the COM object.
    struct ComPtr(*mut c_void);
    impl ComPtr {
        fn vtbl<T>(&self) -> &T {
            unsafe { &**(self.0 as *const *const T) }
        }
        fn as_raw(&self) -> *mut c_void {
            self.0
        }
    }
    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                let vtbl = unsafe { &**(self.0 as *const *const IUnknownVtbl) };
                unsafe { (vtbl.release)(self.0) };
            }
        }
    }

    pub fn decode_with_wic(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
        unsafe {
            // Returns S_FALSE (1) when COM is already initialized, which is fine.
            let hr = CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED);
            if hr < 0 && hr != 1 {
                debug!("WIC: CoInitializeEx failed: 0x{:08x}", hr);
                return None;
            }

            let mut factory_raw: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_WIC_IMAGING_FACTORY,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IWIC_IMAGING_FACTORY,
                &mut factory_raw,
            );
            if hr < 0 {
                debug!("WIC: CoCreateInstance failed: 0x{:08x}", hr);
                return None;
            }
            let factory = ComPtr(factory_raw);

            let wide_path: Vec<u16> = file_path
                .to_str()?
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let factory_vtbl = factory.vtbl::<IWICImagingFactoryVtbl>();
            let mut decoder_raw: *mut c_void = std::ptr::null_mut();
            let hr = (factory_vtbl.create_decoder_from_filename)(
                factory.as_raw(),
                wide_path.as_ptr(),
                std::ptr::null(),
                GENERIC_READ,
                0,
                &mut decoder_raw,
            );
            if hr < 0 {
                debug!("WIC: CreateDecoderFromFilename failed: 0x{:08x}", hr);
                return None;
            }
            let decoder = ComPtr(decoder_raw);

            let decoder_vtbl = decoder.vtbl::<IWICBitmapDecoderVtbl>();
            let mut frame_raw: *mut c_void = std::ptr::null_mut();
            let hr = (decoder_vtbl.get_frame)(decoder.as_raw(), 0, &mut frame_raw);
            if hr < 0 {
                debug!("WIC: GetFrame failed: 0x{:08x}", hr);
                return None;
            }
            let frame = ComPtr(frame_raw);

            let frame_vtbl = frame.vtbl::<IWICBitmapSourceVtbl>();
            let mut orig_w: UINT = 0;
            let mut orig_h: UINT = 0;
            let hr = (frame_vtbl.get_size)(frame.as_raw(), &mut orig_w, &mut orig_h);
            if hr < 0 || orig_w == 0 || orig_h == 0 {
                return None;
            }

            let (scaled_w, scaled_h) = if orig_w > max_dimension || orig_h > max_dimension {
                let ratio = max_dimension as f64 / orig_w.max(orig_h) as f64;
                (
                    (orig_w as f64 * ratio).round() as u32,
                    (orig_h as f64 * ratio).round() as u32,
                )
            } else {
                (orig_w, orig_h)
            };

            let mut scaler_raw: *mut c_void = std::ptr::null_mut();
            let hr = (factory_vtbl.create_bitmap_scaler)(factory.as_raw(), &mut scaler_raw);
            if hr < 0 {
                return None;
            }
            let scaler = ComPtr(scaler_raw);
            let scaler_vtbl = scaler.vtbl::<IWICBitmapScalerVtbl>();
            let hr = (scaler_vtbl.initialize)(
                scaler.as_raw(),
                frame.as_raw(),
                scaled_w,
                scaled_h,
                WIC_BITMAP_INTERPOLATION_FANT,
            );
            if hr < 0 {
                return None;
            }

            let mut converter_raw: *mut c_void = std::ptr::null_mut();
            let hr = (factory_vtbl.create_format_converter)(factory.as_raw(), &mut converter_raw);
            if hr < 0 {
                return None;
            }
            let converter = ComPtr(converter_raw);
            let converter_vtbl = converter.vtbl::<IWICFormatConverterVtbl>();
            let hr = (converter_vtbl.initialize)(
                converter.as_raw(),
                scaler.as_raw(),
                &GUID_WIC_PIXEL_FORMAT_32BPP_RGBA,
                0,
                std::ptr::null_mut(),
                0.0,
                0,
            );
            if hr < 0 {
                return None;
            }

            let stride = scaled_w * 4;
            let buf_size = stride * scaled_h;
            let mut pixel_data = vec![0u8; buf_size as usize];
            let hr = (converter_vtbl.base.copy_pixels)(
                converter.as_raw(),
                std::ptr::null(),
                stride,
                buf_size,
                pixel_data.as_mut_ptr(),
            );
            if hr < 0 {
                return None;
            }

            let rgba = image::RgbaImage::from_raw(scaled_w, scaled_h, pixel_data)?;
            let img = image::DynamicImage::ImageRgba8(rgba);
            let mut buf = Vec::new();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70);
            encoder.encode_image(&img).ok()?;

            debug!(
                "generated thumbnail via Windows WIC: {:?} ({}x{})",
                file_path, scaled_w, scaled_h
            );

            Some(ThumbnailResult {
                data: buf,
                format: ImageFormat::Jpeg,
                width: scaled_w,
                height: scaled_h,
                source: ThumbnailSource::FullDecode,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_png_is_returned_without_reencoding() {
        let image = image::DynamicImage::new_rgba8(24, 16);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode png");

        let result = extract_image_thumbnail_from_bytes(
            &bytes,
            &ThumbnailHint {
                max_dimension: 64,
                preferred_formats: vec![ImageFormat::Jpeg, ImageFormat::Png],
            },
        )
        .expect("generate thumbnail");

        assert_eq!(result.format, ImageFormat::Png);
        assert_eq!(result.source, ThumbnailSource::OriginalEncoded);
        assert_eq!(result.data, bytes);
        assert_eq!((result.width, result.height), (24, 16));
    }

    #[test]
    fn decodes_avif_from_memory() {
        let image = image::DynamicImage::new_rgb8(18, 12);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Avif,
            )
            .expect("encode avif");

        let result = extract_image_thumbnail_from_bytes(
            &bytes,
            &ThumbnailHint {
                max_dimension: 64,
                preferred_formats: vec![ImageFormat::Jpeg],
            },
        )
        .expect("decode avif thumbnail");

        assert_eq!(result.format, ImageFormat::Jpeg);
        assert_eq!(result.source, ThumbnailSource::FullDecode);
        assert_eq!((result.width, result.height), (18, 12));
    }
}
