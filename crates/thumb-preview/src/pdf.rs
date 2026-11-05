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
    use crate::macos_ffi::*;
    use std::os::raw::c_void;

    type CGPDFDocumentRef = *const c_void;
    type CGPDFPageRef = *const c_void;

    const K_CG_PDF_MEDIA_BOX: i32 = 0;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPDFDocumentCreateWithURL(url: CFURLRef) -> CGPDFDocumentRef;
        fn CGPDFDocumentGetNumberOfPages(document: CGPDFDocumentRef) -> usize;
        fn CGPDFDocumentGetPage(document: CGPDFDocumentRef, page_number: usize) -> CGPDFPageRef;
        fn CGPDFDocumentRelease(document: CGPDFDocumentRef);
        fn CGPDFPageGetBoxRect(page: CGPDFPageRef, box_type: i32) -> CGRect;
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
    use std::path::PathBuf;
    use std::sync::OnceLock;

    /// Directories that may hold a packaged PDFium library, in precedence order: beside the
    /// executable, in the installation prefix's `lib/iris`, then the working directory. The
    /// system search path is consulted after these.
    fn pdfium_dirs(exe: Option<&Path>, cwd: Option<PathBuf>) -> Vec<PathBuf> {
        let mut dirs = Vec::with_capacity(3);
        if let Some(exe_dir) = exe.and_then(Path::parent) {
            dirs.push(exe_dir.to_path_buf());
            if let Some(prefix) = exe_dir.parent() {
                dirs.push(prefix.join("lib").join("iris"));
            }
        }
        if let Some(cwd) = cwd {
            dirs.push(cwd);
        }
        dirs
    }

    /// Binds pdfium once per process.
    ///
    /// `Pdfium::default()` panics when the shared library is missing, and the release profile
    /// aborts on panic, so binding goes through the fallible API. pdfium is optional at run time:
    /// without it PDFs simply get no thumbnail. The library next to the executable wins, then the
    /// installation prefix's `lib/iris` directory used by Linux packages, then the working
    /// directory, then the system search path.
    fn pdfium() -> Option<&'static Pdfium> {
        static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();
        PDFIUM
            .get_or_init(|| {
                let exe = std::env::current_exe().ok();
                let dirs = pdfium_dirs(exe.as_deref(), std::env::current_dir().ok());
                let local_candidates = dirs
                    .into_iter()
                    .map(|dir| Pdfium::pdfium_platform_library_name_at_path(&dir));
                let bindings = local_candidates
                    .filter(|candidate| candidate.is_file())
                    .find_map(|candidate| Pdfium::bind_to_library(candidate).ok())
                    .map(Ok)
                    .unwrap_or_else(Pdfium::bind_to_system_library);
                match bindings {
                    Ok(bindings) => Some(Pdfium::new(bindings)),
                    Err(error) => {
                        tracing::info!(
                            "pdfium is unavailable; PDF thumbnails are disabled: {error}"
                        );
                        None
                    }
                }
            })
            .as_ref()
    }

    pub fn render_first_page(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
        let document = pdfium()?.load_pdf_from_file(file_path, None).ok()?;
        let page = document.pages().get(0).ok()?;

        let width = page.width().value;
        let height = page.height().value;
        if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
            return None;
        }

        let scale = max_dimension.max(1) as f32 / width.max(height);
        let render_width = ((width * scale).round() as i32).max(1);
        let render_height = ((height * scale).round() as i32).max(1);

        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_width(render_width)
                    .set_target_height(render_height),
            )
            .ok()?;

        // JPEG has no alpha channel; the image crate refuses to encode RGBA as JPEG.
        let rgb = image::DynamicImage::ImageRgb8(bitmap.as_image().ok()?.into_rgb8());
        let mut buf = Cursor::new(Vec::new());
        rgb.write_to(&mut buf, image::ImageFormat::Jpeg).ok()?;

        debug!(
            "rendered PDF first page {}x{} from {:?} via pdfium",
            render_width, render_height, file_path
        );

        Some(ThumbnailResult {
            data: buf.into_inner(),
            format: ImageFormat::Jpeg,
            width: rgb.width(),
            height: rgb.height(),
            source: ThumbnailSource::FullDecode,
        })
    }

    pub fn page_count(file_path: &Path) -> Option<u32> {
        let document = pdfium()?.load_pdf_from_file(file_path, None).ok()?;
        u32::try_from(document.pages().len()).ok()
    }

    #[cfg(test)]
    mod tests {
        use super::pdfium_dirs;
        use std::path::{Path, PathBuf};

        #[test]
        fn search_order_prefers_executable_then_install_prefix_then_working_directory() {
            let dirs = pdfium_dirs(
                Some(Path::new("/usr/bin/Iris")),
                Some(PathBuf::from("/work")),
            );
            assert_eq!(
                dirs,
                vec![
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/usr/lib/iris"),
                    PathBuf::from("/work"),
                ]
            );
        }

        #[test]
        fn search_order_without_executable_or_working_directory_is_empty() {
            assert!(pdfium_dirs(None, None).is_empty());
        }
    }
}
