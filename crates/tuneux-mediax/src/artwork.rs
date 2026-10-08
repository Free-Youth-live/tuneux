//! # 封面图解码：原始字节 → RGBA 像素（三产品共用）
//!
//! 原三份重复实现（tuneux / fx / max 各一份）收口于此。
//! 公开面不泄露 image crate 类型（返回自有 CoverImage 结构）。
//!
//! 解码回退链：MIME 提示 → 魔术字节探测 → JPEG / PNG 暴力尝试
//! （与 workspace image features = jpeg + png 对齐）。

/// 解码后的封面图（RGBA 像素，非 image crate 类型）。
#[derive(Debug, Clone)]
pub struct CoverImage {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA 像素数据（长度 = width × height × 4，行主序）。
    pub rgba: Vec<u8>,
}

/// 解码封面原始字节（回退链见模块文档）。
pub fn decode(bytes: &[u8], mime_hint: Option<&str>) -> Option<CoverImage> {
    let img = decode_dynamic(bytes, mime_hint)?;
    to_cover(img)
}

/// 解码并缩放到指定尺寸内（保持宽高比，Triangle 滤波）。
pub fn decode_thumb(
    bytes: &[u8],
    mime_hint: Option<&str>,
    max_w: u32,
    max_h: u32,
) -> Option<CoverImage> {
    let img = decode_dynamic(bytes, mime_hint)?;
    let (sw, sh) = (img.width().max(1), img.height().max(1));
    let scale = (max_w as f32 / sw as f32).min(max_h as f32 / sh as f32);
    let dw = ((sw as f32 * scale).round() as u32).max(1).min(max_w);
    let dh = ((sh as f32 * scale).round() as u32).max(1).min(max_h);
    let resized = img
        .resize_exact(dw, dh, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let (w, h) = resized.dimensions();
    Some(CoverImage {
        width: w,
        height: h,
        rgba: resized.into_raw(),
    })
}

/// DynamicImage → CoverImage（内部转换，不外泄 image 类型）。
fn to_cover(img: image::DynamicImage) -> Option<CoverImage> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(CoverImage {
        width: w,
        height: h,
        rgba: rgba.into_raw(),
    })
}

/// 三层回退解码为 DynamicImage（内部实现）。
fn decode_dynamic(bytes: &[u8], mime_hint: Option<&str>) -> Option<image::DynamicImage> {
    // 1) MIME 提示（来自容器标签的 media_type 字段）
    if let Some(mime) = mime_hint {
        if let Some(fmt) = image::ImageFormat::from_mime_type(mime) {
            if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
                return Some(img);
            }
        }
    }
    // 2) 魔术字节探测
    if let Ok(fmt) = image::guess_format(bytes) {
        if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
            return Some(img);
        }
    }
    // 3) JPEG / PNG 暴力尝试（标签声明 MIME 但实际是另一种格式的情况）
    for fmt in [image::ImageFormat::Jpeg, image::ImageFormat::Png] {
        if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
            return Some(img);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 2×2 纯红 PNG 字节（最小合法 PNG，无需外部文件）。
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .expect("应能编码 PNG");
        buf.into_inner()
    }

    #[test]
    fn decode_png_basic() {
        let png = tiny_png();
        let cover = decode(&png, None).expect("PNG 应可解码");
        assert_eq!(cover.width, 2);
        assert_eq!(cover.height, 2);
        assert_eq!(cover.rgba.len(), 2 * 2 * 4);
        // 左上角像素 = 红色
        assert_eq!(&cover.rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn decode_with_wrong_mime_still_works_via_fallback() {
        let png = tiny_png();
        // 故意给错 MIME：回退链应通过魔术字节或暴力尝试命中
        let cover = decode(&png, Some("image/jpeg")).expect("错误 MIME 应回退解码");
        assert_eq!(cover.width, 2);
    }

    #[test]
    fn decode_thumb_scales_down() {
        let png = tiny_png();
        let cover = decode_thumb(&png, None, 1, 1).expect("缩略图应可解码");
        assert_eq!(cover.width, 1);
        assert_eq!(cover.height, 1);
    }

    #[test]
    fn decode_garbage_returns_none() {
        assert!(decode(&[0xFF; 64], None).is_none());
        assert!(decode(b"", None).is_none());
    }
}
