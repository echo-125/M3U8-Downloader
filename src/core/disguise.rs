//! PNG 伪装分片解码。
//!
//! 部分站点把 TS / fMP4 分片伪装成 PNG 图片：响应体是一张带缩略图的合法 PNG，
//! 真实媒体数据藏在自定义 chunk `roUd` 里，首字节是标志位（bit0 置位表示
//! 后续数据经 zlib deflate 压缩），图片本体与媒体数据无关，只起迷惑作用。

use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::core::format::{detect_format_decoded, SegmentFormat};

/// PNG 文件签名。
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// MPEG-TS 包固定长度。
const TS_PACKET_BYTES: usize = 188;
/// 允许解出的媒体数据上限，防止异常压缩数据撑爆内存。
const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;

/// 尝试从 PNG 伪装分片中提取真实媒体数据。
///
/// 返回 None 表示这不是可解包的伪装分片：不是 PNG、没有 roUd chunk、
/// chunk 数据被截断，或内嵌数据校验不过。调用方应继续按原始内容报错。
pub fn extract_png_media(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < PNG_SIGNATURE.len() || data[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return None;
    }
    let mut media: Option<Vec<u8>> = None;
    let mut pos = PNG_SIGNATURE.len();
    loop {
        // 长度、类型或 CRC 被截断时一律放弃：半截数据既不能当媒体，
        // 也说明响应本身不完整。
        let header = data.get(pos..pos + 8)?;
        let length = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
        let chunk_type: [u8; 4] = header[4..].try_into().ok()?;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(length)?;
        data.get(body_end..body_end + 4)?;
        if chunk_type == *b"roUd" {
            let unpacked = unpack_chunk(&data[body_start..body_end])?;
            media
                .get_or_insert_with(Vec::new)
                .extend_from_slice(&unpacked);
        }
        pos = body_end + 4;
        if chunk_type == *b"IEND" {
            return media;
        }
    }
}

/// 解开单个 roUd chunk：首字节是标志位，bit0 置位表示数据经 zlib deflate 压缩。
fn unpack_chunk(body: &[u8]) -> Option<Vec<u8>> {
    let (flag, payload) = body.split_first()?;
    let media = if flag & 1 != 0 {
        inflate(payload)?
    } else {
        payload.to_vec()
    };
    is_valid_media(&media).then_some(media)
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    // take 多读一个字节用于区分「刚好到上限」和「被截断」，超限即拒绝。
    let mut decoded = Vec::new();
    ZlibDecoder::new(data)
        .take(MAX_MEDIA_BYTES as u64 + 1)
        .read_to_end(&mut decoded)
        .ok()?;
    (decoded.len() <= MAX_MEDIA_BYTES).then_some(decoded)
}

/// 内嵌数据必须是完整 TS（188 对齐且逐包同步字节校验）或 fMP4，
/// 防止把碰巧同头的图片杂讯当媒体拼进成品。
fn is_valid_media(media: &[u8]) -> bool {
    match detect_format_decoded(media) {
        SegmentFormat::Ts => {
            media.len() % TS_PACKET_BYTES == 0
                && media
                    .chunks_exact(TS_PACKET_BYTES)
                    .all(|packet| packet[0] == 0x47)
        }
        SegmentFormat::Fmp4 => true,
        _ => false,
    }
}

/// 按伪装方案的 chunk 布局构造单个 PNG chunk。仅测试使用。
#[cfg(test)]
fn png_chunk(chunk_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut chunk = (body.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(chunk_type);
    chunk.extend_from_slice(body);
    // 解析路径不校验 CRC，测试数据填 0 即可。
    chunk.extend_from_slice(&[0; 4]);
    chunk
}

/// 用给定媒体数据构造一张 PNG 伪装分片，供单元测试与端到端用例生成样本。
#[cfg(test)]
pub(crate) fn disguised_png(flag: u8, media: &[u8]) -> Vec<u8> {
    let mut body = vec![flag];
    body.extend_from_slice(media);
    let mut png = PNG_SIGNATURE.to_vec();
    png.extend_from_slice(&png_chunk(b"IHDR", &[0; 13]));
    png.extend_from_slice(&png_chunk(b"IDAT", b"thumbnail"));
    png.extend_from_slice(&png_chunk(b"roUd", &body));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn sample_ts() -> Vec<u8> {
        let mut data = Vec::new();
        for fill in [0x11_u8, 0x22, 0x33] {
            data.push(0x47);
            data.extend(vec![fill; TS_PACKET_BYTES - 1]);
        }
        data
    }

    #[test]
    fn extracts_raw_ts() {
        let ts = sample_ts();
        assert_eq!(
            extract_png_media(&disguised_png(0x00, &ts)).as_deref(),
            Some(&ts[..])
        );
    }

    #[test]
    fn extracts_deflated_ts() {
        let ts = sample_ts();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&ts).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(
            extract_png_media(&disguised_png(0x01, &compressed)).as_deref(),
            Some(&ts[..])
        );
    }

    #[test]
    fn rejects_png_without_media_chunk() {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&png_chunk(b"IHDR", &[0; 13]));
        png.extend_from_slice(&png_chunk(b"IDAT", b"thumbnail"));
        png.extend_from_slice(&png_chunk(b"IEND", &[]));
        assert_eq!(extract_png_media(&png), None);
    }

    #[test]
    fn rejects_truncated_chunk() {
        let png = disguised_png(0x00, &sample_ts());
        // 掐掉 roUd 数据尾部与 CRC：截断的伪装分片不能交出半截媒体。
        assert_eq!(extract_png_media(&png[..png.len() - 100]), None);
    }

    #[test]
    fn rejects_chunk_with_broken_ts_sync() {
        let mut junk = vec![0x47; TS_PACKET_BYTES * 2];
        junk[TS_PACKET_BYTES] = 0x00;
        assert_eq!(extract_png_media(&disguised_png(0x00, &junk)), None);
    }

    #[test]
    fn rejects_chunk_with_invalid_media() {
        let junk = vec![0x00; TS_PACKET_BYTES * 2];
        assert_eq!(extract_png_media(&disguised_png(0x00, &junk)), None);
    }

    #[test]
    fn rejects_non_png_data() {
        let ts = sample_ts();
        assert_eq!(extract_png_media(&ts), None);
    }
}
