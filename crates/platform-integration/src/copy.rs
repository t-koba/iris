use crate::file_times::{preserve_modified_time, preserve_modified_time_of};
use std::path::Path;

/// macOS copyfile and Windows CopyFileEx run without a progress callback here, so larger files
/// take the progressed streaming path instead.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const UNPROGRESSED_COPY_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PlatformCopyError {
    #[error("platform copy intentionally skipped: {0}")]
    Unsupported(&'static str),

    #[error("platform copy I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl PlatformCopyError {
    pub fn should_fallback_to_streaming(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

/// Copy a file through the operating system's native path.
///
/// This intentionally operates on filesystem paths, including mounted SMB/NFS
/// paths. On supported platforms `std::fs::copy` delegates to OS facilities
/// such as copyfile/copy_file_range/CopyFileEx, which lets the kernel or
/// filesystem attempt clone/offloaded copies before user-space streaming.
pub async fn platform_copy(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<u64, PlatformCopyError> {
    let src = src.as_ref().to_path_buf();
    let dst = dst.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || platform_copy_blocking(&src, &dst))
        .await
        .map_err(|error| std::io::Error::other(format!("platform copy task failed: {error}")))?
}

/// Copy a regular file through `std::io::copy(File, File)` on the blocking pool.
///
/// This is the progressed fallback for local and mounted shares when native
/// platform copy is intentionally skipped. Keeping the operands as plain files
/// lets std use platform-specialized file copy paths where available.
pub async fn stream_file_copy(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<u64, PlatformCopyError> {
    let src = src.as_ref().to_path_buf();
    let dst = dst.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || stream_file_copy_blocking(&src, &dst))
        .await
        .map_err(|error| std::io::Error::other(format!("stream copy task failed: {error}")))?
}

fn platform_copy_blocking(src: &Path, dst: &Path) -> Result<u64, PlatformCopyError> {
    let metadata = std::fs::metadata(src)?;
    if !metadata.is_file() {
        return Err(PlatformCopyError::Unsupported(
            "source is not a regular file",
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if metadata.len() > UNPROGRESSED_COPY_LIMIT {
        return Err(PlatformCopyError::Unsupported(
            "large platform copy has no progress callback",
        ));
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // macOS copyfile/clonefile keep the source mtime; Linux and Windows stamp the copy time.
    let copied = std::fs::copy(src, dst)?;
    if let Ok(mtime) = metadata.modified() {
        preserve_modified_time(dst, mtime)?;
    }
    Ok(copied)
}

fn stream_file_copy_blocking(src: &Path, dst: &Path) -> Result<u64, PlatformCopyError> {
    let metadata = std::fs::metadata(src)?;
    if !metadata.is_file() {
        return Err(PlatformCopyError::Unsupported(
            "source is not a regular file",
        ));
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut src_file = std::fs::File::open(src)?;
    let mut dst_file = std::fs::File::create(dst)?;
    let copied = std::io::copy(&mut src_file, &mut dst_file)?;
    // The new file got the current time; set the source's on the same handle before closing.
    if let Ok(mtime) = metadata.modified() {
        preserve_modified_time_of(&dst_file, mtime)?;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn platform_copy_preserves_bytes_and_mtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("nested").join("dst.bin");
        std::fs::write(&src, b"hello").expect("write src");

        let old_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(12345);
        preserve_modified_time(&src, old_mtime).expect("set mtime");

        let copied = platform_copy(&src, &dst).await.expect("platform copy");
        assert_eq!(copied, 5);
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"hello");
        assert_eq!(
            std::fs::metadata(&dst)
                .expect("dst metadata")
                .modified()
                .expect("dst mtime"),
            old_mtime
        );
    }

    #[tokio::test]
    async fn platform_copy_rejects_directories_for_streaming_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dst = temp.path().join("dst");
        let error = platform_copy(temp.path(), &dst)
            .await
            .expect_err("directory should not be copied by platform_copy");
        assert!(error.should_fallback_to_streaming());
    }

    #[tokio::test]
    async fn stream_file_copy_preserves_bytes_and_mtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("nested").join("dst.bin");
        let bytes = (0..8192u32)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&src, &bytes).expect("write src");

        let old_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(54321);
        preserve_modified_time(&src, old_mtime).expect("set mtime");

        let copied = stream_file_copy(&src, &dst).await.expect("stream copy");
        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(std::fs::read(&dst).expect("read dst"), bytes);
        assert_eq!(
            std::fs::metadata(&dst)
                .expect("dst metadata")
                .modified()
                .expect("dst mtime"),
            old_mtime
        );
    }

    #[tokio::test]
    async fn stream_file_copy_rejects_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dst = temp.path().join("dst");
        let error = stream_file_copy(temp.path(), &dst)
            .await
            .expect_err("directory should not be copied by stream_file_copy");
        assert!(error.should_fallback_to_streaming());
    }

    #[tokio::test]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn large_unprogressed_platform_copy_uses_streaming_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("large.bin");
        let dst = temp.path().join("dst.bin");
        let file = std::fs::File::create(&src).expect("create src");
        file.set_len(UNPROGRESSED_COPY_LIMIT + 1)
            .expect("make sparse src");

        let error = platform_copy(&src, &dst)
            .await
            .expect_err("large copy should use streaming fallback");

        assert!(error.should_fallback_to_streaming());
        assert!(!dst.exists());
    }
}
