//! Just enough PNG to store clipboard images: the 8-byte signature and the
//! IHDR chunk (size and CRC checked), for the width and height. The pixel
//! data is never decoded.

/// Every PNG file starts with these bytes.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

/// The mime type image entries are stored and copied as.
pub const MIME: &str = "image/png";

/// Largest width or height the PNG spec allows (2^31 - 1).
const MAX_DIMENSION: u32 = 0x7FFF_FFFF;

/// What the IHDR chunk says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngInfo {
    pub width: u32,
    pub height: u32,
}

/// Width and height from the IHDR chunk, or `None` unless `bytes` start with
/// the PNG signature followed by a well-formed IHDR (length 13, non-zero
/// dimensions within the spec's limit, matching CRC).
pub fn parse_header(bytes: &[u8]) -> Option<PngInfo> {
    let rest = bytes.strip_prefix(&SIGNATURE)?;
    // length (4) + type (4) + data (13) + crc (4)
    let chunk = rest.get(..25)?;
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    if be32(&chunk[0..4]) != 13 || &chunk[4..8] != b"IHDR" {
        return None;
    }
    if crc32(&chunk[4..21]) != be32(&chunk[21..25]) {
        return None;
    }
    let (width, height) = (be32(&chunk[8..12]), be32(&chunk[12..16]));
    let valid = |n: u32| (1..=MAX_DIMENSION).contains(&n);
    (valid(width) && valid(height)).then_some(PngInfo { width, height })
}

/// CRC-32 (ISO-HDLC, as PNG chunks use), bitwise: it only ever runs over
/// one 17-byte chunk header.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A small, valid-looking PNG of `width` x `height` (signature, IHDR, an
/// empty IDAT and IEND, each with a correct CRC) plus `tag` in a private
/// chunk so tests can make distinct images of the same size.
#[cfg(test)]
pub fn test_png(width: u32, height: u32, tag: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    let mut out = SIGNATURE.to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"tEXt", tag);
    chunk(&mut out, b"IDAT", &[]);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // The CRC of an IEND chunk, as in every PNG file
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn parses_width_and_height() {
        assert_eq!(parse_header(&test_png(640, 480, b"a")), Some(PngInfo { width: 640, height: 480 }));
        assert_eq!(parse_header(&test_png(1, 1, b"")), Some(PngInfo { width: 1, height: 1 }));
        let max = test_png(MAX_DIMENSION, 3, b"");
        assert_eq!(parse_header(&max).map(|i| i.width), Some(MAX_DIMENSION));
        // Only the header is read: a truncated body still parses
        assert_eq!(parse_header(&test_png(5, 7, b"")[..33]), Some(PngInfo { width: 5, height: 7 }));
    }

    #[test]
    fn rejects_what_is_not_a_png() {
        let good = test_png(10, 20, b"");
        assert_eq!(parse_header(b""), None);
        assert_eq!(parse_header(b"hello world, this is plain text!!"), None);
        assert_eq!(parse_header(&SIGNATURE), None, "signature alone");
        assert_eq!(parse_header(&good[..32]), None, "IHDR cut short");

        let mut bad_sig = good.clone();
        bad_sig[1] = b'J';
        assert_eq!(parse_header(&bad_sig), None);

        let mut bad_crc = good.clone();
        bad_crc[16] ^= 1; // a width byte, so the CRC no longer matches
        assert_eq!(parse_header(&bad_crc), None);

        let mut bad_len = good.clone();
        bad_len[11] = 12;
        assert_eq!(parse_header(&bad_len), None);

        let mut not_ihdr = good;
        not_ihdr[12..16].copy_from_slice(b"IDAT");
        assert_eq!(parse_header(&not_ihdr), None);

        assert_eq!(parse_header(&test_png(0, 5, b"")), None, "zero width");
        assert_eq!(parse_header(&test_png(5, 0, b"")), None, "zero height");
        assert_eq!(parse_header(&test_png(MAX_DIMENSION + 1, 5, b"")), None, "over the spec limit");
    }
}
