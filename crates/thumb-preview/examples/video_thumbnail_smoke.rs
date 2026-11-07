//! Exercises Iris's decoder against the validation workflow's generated MPEG-4 clip.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("expected test clip path")?;
    let thumbnail = iris_thumb_preview::video::extract_video_thumbnail(path.as_ref(), 32)
        .ok_or("Iris video thumbnail extraction failed")?;
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
