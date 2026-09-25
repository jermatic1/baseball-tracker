use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;

pub fn encode_gray_jpeg(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize;
    if pixels.len() != expected {
        return Err(format!(
            "frame len {} != {}x{}",
            pixels.len(),
            width,
            height
        ));
    }
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, 80);
    encoder
        .write_image(pixels, width, height, image::ExtendedColorType::L8)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}
