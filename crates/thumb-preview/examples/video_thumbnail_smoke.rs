//! Exercises Iris's decoder against the validation workflow's generated MPEG-4 clip.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("expected test clip path")?;
    let still = std::env::args_os()
        .nth(2)
        .is_some_and(|arg| arg == "--still");
    let thumbnail = if still {
        let bytes = std::fs::read(&path)?;
        let from_bytes =
            iris_thumb_preview::video::extract_still_thumbnail_from_bytes(&bytes, "png", 32)
                .ok_or("Iris FFmpeg still-byte fallback failed")?;
        let from_file = iris_thumb_preview::video::extract_still_thumbnail(path.as_ref(), 32)
            .ok_or("Iris FFmpeg still-file fallback failed")?;
        if from_bytes.data != from_file.data {
            return Err("still-file and still-byte fallback outputs differ".into());
        }
        from_file
    } else {
        iris_thumb_preview::video::extract_video_thumbnail(path.as_ref(), 32)
            .ok_or("Iris video thumbnail extraction failed")?
    };
    if thumbnail.width != 32 || thumbnail.height != 24 || !thumbnail.data.starts_with(&[0xff, 0xd8])
    {
        return Err("video thumbnail dimensions or JPEG encoding are incorrect".into());
    }
    println!(
        "Video smoke passed: {}x{}, {} bytes",
        thumbnail.width,
        thumbnail.height,
        thumbnail.data.len()
    );
    Ok(())
}
