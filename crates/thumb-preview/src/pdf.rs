//! PDF thumbnails: Core Graphics on macOS (no external library), pdfium-render elsewhere.

#[cfg(not(feature = "pdf"))]
use iris_core_domain::types::thumbnail::ThumbnailResult;
#[cfg(feature = "pdf")]
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailResult, ThumbnailSource};
use std::path::Path;
#[cfg(feature = "pdf")]
use tracing::debug;

/// Renders the first page; best effort, `None` on failure.
#[cfg(feature = "pdf")]
pub fn extract_pdf_thumbnail(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    platform::render_first_page(file_path, max_dimension)
}

#[cfg(feature = "pdf")]
pub fn pdf_page_count(file_path: &Path) -> Option<u32> {
    platform::page_count(file_path)
}

#[cfg(not(feature = "pdf"))]
pub fn extract_pdf_thumbnail(_file_path: &Path, _max_dimension: u32) -> Option<ThumbnailResult> {
    None
}

#[cfg(not(feature = "pdf"))]
pub fn pdf_page_count(_file_path: &Path) -> Option<u32> {
    None
}

#[cfg(feature = "pdf")]
pub const fn supports_pdf_thumbnails() -> bool {
    true
}

#[cfg(not(feature = "pdf"))]
pub const fn supports_pdf_thumbnails() -> bool {
    false
}

#[cfg(all(feature = "pdf", target_os = "macos"))]
mod platform {
    use super::*;
    use std::os::raw::c_void;

    type CFURLRef = *const c_void;
    type CFAllocatorRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: CFAllocatorRef,
            buffer: *const u8,
            buf_len: isize,
            is_directory: bool,
        ) -> CFURLRef;
        fn CFRelease(cf: *const c_void);
    }

    type CGPDFDocumentRef = *const c_void;
    type CGPDFPageRef = *const c_void;
    type CGContextRef = *mut c_void;
    type CGColorSpaceRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    const K_CG_PDF_MEDIA_BOX: i32 = 0;
    const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPDFDocumentCreateWithURL(url: CFURLRef) -> CGPDFDocumentRef;
        fn CGPDFDocumentGetNumberOfPages(document: CGPDFDocumentRef) -> usize;
        fn CGPDFDocumentGetPage(document: CGPDFDocumentRef, page_number: usize) -> CGPDFPageRef;
        fn CGPDFDocumentRelease(document: CGPDFDocumentRef);
        fn CGPDFPageGetBoxRect(page: CGPDFPageRef, box_type: i32) -> CGRect;
        fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        fn CGColorSpaceRelease(color_space: CGColorSpaceRef);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            color_space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> CGContextRef;
        fn CGContextRelease(context: CGContextRef);
        fn CGContextSetRGBFillColor(context: CGContextRef, r: f64, g: f64, b: f64, a: f64);
        fn CGContextFillRect(context: CGContextRef, rect: CGRect);
        fn CGContextScaleCTM(context: CGContextRef, sx: f64, sy: f64);
        fn CGContextDrawPDFPage(context: CGContextRef, page: CGPDFPageRef);
        fn CGBitmapContextGetData(context: CGContextRef) -> *const u8;
    }

    /// RAII wrapper owning a CGPDFDocument.
    struct PdfDoc(CGPDFDocumentRef);

    impl PdfDoc {
        fn open(file_path: &Path) -> Option<Self> {
            let path_str = file_path.to_str()?;
            let path_bytes = path_str.as_bytes();
            unsafe {
                let url = CFURLCreateFromFileSystemRepresentation(
                    std::ptr::null(),
                    path_bytes.as_ptr(),
                    path_bytes.len() as isize,
                    false,
                );
                if url.is_null() {
                    return None;
                }
                let doc = CGPDFDocumentCreateWithURL(url);
                CFRelease(url);
                if doc.is_null() {
                    None
                } else {
                    Some(Self(doc))
                }
            }
        }

        fn num_pages(&self) -> usize {
            unsafe { CGPDFDocumentGetNumberOfPages(self.0) }
        }

        fn page(&self, number: usize) -> CGPDFPageRef {
            unsafe { CGPDFDocumentGetPage(self.0, number) }
        }
    }

    impl Drop for PdfDoc {
        fn drop(&mut self) {
            unsafe { CGPDFDocumentRelease(self.0) }
        }
    }

    pub fn render_first_page(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
        let doc = PdfDoc::open(file_path)?;
        if doc.num_pages() == 0 {
            return None;
        }

        let page = doc.page(1); // 1-based
        if page.is_null() {
            return None;
        }

        let media_box = unsafe { CGPDFPageGetBoxRect(page, K_CG_PDF_MEDIA_BOX) };
        let page_w = media_box.size.width;
        let page_h = media_box.size.height;
        if page_w <= 0.0 || page_h <= 0.0 {
            return None;
        }

        let scale = if page_w > page_h {
            max_dimension as f64 / page_w
        } else {
            max_dimension as f64 / page_h
        };
        let render_w = (page_w * scale).round() as usize;
        let render_h = (page_h * scale).round() as usize;
        if render_w == 0 || render_h == 0 {
            return None;
        }

        unsafe {
            let color_space = CGColorSpaceCreateDeviceRGB();
            let bytes_per_row = render_w * 4;
            let ctx = CGBitmapContextCreate(
                std::ptr::null_mut(),
                render_w,
                render_h,
                8,
                bytes_per_row,
                color_space,
                K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
            );
            CGColorSpaceRelease(color_space);
            if ctx.is_null() {
                return None;
            }

            // Fill with a white background.
            let full_rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: render_w as f64,
                    height: render_h as f64,
                },
            };
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(ctx, full_rect);

            CGContextScaleCTM(ctx, scale, scale);
            CGContextDrawPDFPage(ctx, page);

            let data_ptr = CGBitmapContextGetData(ctx);
            if data_ptr.is_null() {
                CGContextRelease(ctx);
                return None;
            }
            let pixel_data =
                std::slice::from_raw_parts(data_ptr, render_h * bytes_per_row).to_vec();
            CGContextRelease(ctx);

            let img = image::RgbaImage::from_raw(render_w as u32, render_h as u32, pixel_data)?;
            let mut jpeg_buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                .ok()?;

            let num_pages = doc.num_pages();
            debug!(
                "rendered PDF first page {}x{} ({} pages) from {:?} via CoreGraphics",
                render_w, render_h, num_pages, file_path
            );

            Some(ThumbnailResult {
                data: jpeg_buf.into_inner(),
                format: ImageFormat::Jpeg,
                width: render_w as u32,
                height: render_h as u32,
                source: ThumbnailSource::FullDecode,
            })
        }
    }

    pub fn page_count(file_path: &Path) -> Option<u32> {
        let doc = PdfDoc::open(file_path)?;
        Some(doc.num_pages() as u32)
    }
}

#[cfg(all(feature = "pdf", not(target_os = "macos")))]
mod platform {
    use super::*;
    use pdfium_render::prelude::*;
    use std::io::Cursor;

    pub fn render_first_page(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
        let pdfium = Pdfium::default();
        let document = pdfium.load_pdf_from_file(file_path, None).ok()?;
        let page = document.pages().get(0).ok()?;

        let width = page.width().value;
        let height = page.height().value;

        let scale = if width > height {
            max_dimension as f32 / width
        } else {
            max_dimension as f32 / height
        };
        let render_width = (width * scale) as i32;
        let render_height = (height * scale) as i32;

        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_width(render_width)
                    .set_target_height(render_height),
            )
            .ok()?;

        let dynamic = bitmap.as_image();
        let mut buf = Cursor::new(Vec::new());
        dynamic.write_to(&mut buf, image::ImageFormat::Jpeg).ok()?;

        debug!(
            "rendered PDF first page {}x{} from {:?} via pdfium",
            render_width, render_height, file_path
        );

        Some(ThumbnailResult {
            data: buf.into_inner(),
            format: ImageFormat::Jpeg,
            width: render_width as u32,
            height: render_height as u32,
            source: ThumbnailSource::FullDecode,
        })
    }

    pub fn page_count(file_path: &Path) -> Option<u32> {
        let pdfium = Pdfium::default();
        let document = pdfium.load_pdf_from_file(file_path, None).ok()?;
        Some(document.pages().len() as u32)
    }
}
