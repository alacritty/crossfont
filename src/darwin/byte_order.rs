//! Constants for bitmap byte order.

#[cfg(target_endian = "little")]
pub fn extract_rgba(bytes: &[u8]) -> Vec<u8> {
    let pixels = bytes.len() / 4;
    let mut rgb = Vec::with_capacity(pixels * 4);

    for i in 0..pixels {
        let offset = i * 4;
        rgb.push(bytes[offset + 2]);
        rgb.push(bytes[offset + 1]);
        rgb.push(bytes[offset]);
        rgb.push(bytes[offset + 3]);
    }

    rgb
}

#[cfg(target_endian = "big")]
pub fn extract_rgba(bytes: Vec<u8>) -> Vec<u8> {
    bytes
}

#[cfg(target_endian = "little")]
pub fn extract_rgb(bytes: &[u8]) -> Vec<u8> {
    let pixels = bytes.len() / 4;
    let mut rgb = Vec::with_capacity(pixels * 3);

    for i in 0..pixels {
        let offset = i * 4;
        rgb.push(bytes[offset + 2]);
        rgb.push(bytes[offset + 1]);
        rgb.push(bytes[offset]);
    }

    rgb
}

#[cfg(target_endian = "big")]
pub fn extract_rgb(bytes: Vec<u8>) -> Vec<u8> {
    bytes
        .into_iter()
        .enumerate()
        .filter(|&(index, _)| ((index) % 4) != 0)
        .map(|(_, val)| val)
        .collect::<Vec<_>>()
}
