//! A minimal BMP writer for `--screenshot`, so capturing a frame needs no
//! image crate. BMP is uncompressed: a 54-byte header, then rows of BGR
//! pixels from the bottom up, each row padded to a multiple of 4 bytes.

/// Encode tightly packed RGBA pixels as a 24-bit BMP file.
///
/// Returns `None` if the dimensions do not match the data or the file would
/// exceed BMP's 4 GB size field.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    if rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let row = (w * 3).div_ceil(4) * 4;
    let image_size = u32::try_from(row.checked_mul(h)?).ok()?;
    let file_size = image_size.checked_add(54)?;
    let width_i = i32::try_from(width).ok()?;
    let height_i = i32::try_from(height).ok()?;

    let mut out = Vec::with_capacity(file_size as usize);
    // File header.
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    // Info header (BITMAPINFOHEADER).
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width_i.to_le_bytes());
    out.extend_from_slice(&height_i.to_le_bytes()); // positive: bottom-up rows
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // no compression
    out.extend_from_slice(&image_size.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes()); // 72 dpi
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&[0; 8]); // palette counts

    let padding = row - w * 3;
    for y in (0..h).rev() {
        for px in rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        out.extend(std::iter::repeat_n(0, padding));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn two_by_two_has_padded_bottom_up_rows() {
        // Top row red, green; bottom row blue, white.
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let bmp = super::encode(2, 2, &rgba).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 2 * 8);
        // First stored row is the bottom one: blue then white, as BGR.
        assert_eq!(&bmp[54..60], &[255, 0, 0, 255, 255, 255]);
        assert_eq!(&bmp[62..68], &[0, 0, 255, 0, 255, 0]);
    }

    #[test]
    fn rejects_mismatched_sizes() {
        assert!(super::encode(2, 2, &[0; 15]).is_none());
    }
}
