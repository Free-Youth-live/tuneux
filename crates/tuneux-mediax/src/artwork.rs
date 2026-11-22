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
    // 目标尺寸为 0 会算出 0 尺寸 resize 输入（debug 构建 panic）：按无效参数处理。
    if max_w == 0 || max_h == 0 {
        return None;
    }
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

/// 单边像素上限（输出上限）：4096×4096 RGBA 最多约 64MB 分配，
/// 对封面展示与缩略场景绰绰有余；单边超过 4096 的合法封面会被拒绝，
/// 这是防解压炸弹的有意取舍（封面用途不需要超过 4K 的分辨率）。
const DECODE_MAX_DIM: u32 = 4096;

/// 解码资源上限：单边像素 ≤ DECODE_MAX_DIM，其余沿用 image 默认。
///
/// max_image_width/height 是 image 的严格上限（头部解析后即校验，
/// 巨像素缓冲不会分配）；max_alloc 沿用默认 512MiB 兜底。
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(DECODE_MAX_DIM);
    limits.max_image_height = Some(DECODE_MAX_DIM);
    limits
}

/// 按指定格式解码并套用输出上限（超限声明在头部校验即被拒绝，不做巨分配）。
fn load_limited(bytes: &[u8], fmt: image::ImageFormat) -> Option<image::DynamicImage> {
    let mut reader =
        image::ImageReader::with_format(std::io::BufReader::new(std::io::Cursor::new(bytes)), fmt);
    reader.limits(decode_limits());
    reader.decode().ok()
}

/// 三层回退解码为 DynamicImage（内部实现）。
fn decode_dynamic(bytes: &[u8], mime_hint: Option<&str>) -> Option<image::DynamicImage> {
    // 解码上限分两层、各自独立：
    // · 输入上限（DECODE_INPUT_LIMIT）：按压缩文件字节数拦截超大文件；
    // · 输出上限（decode_limits）：按声明像素拦截「小文件大声明」——纯色
    //   PNG 几 KB 就能声明 10000×10000，输入上限对它无能为力；不设输出
    //   上限时 image 默认 512MiB 分配额度会放行约 400MB 的巨分配。
    // 两层任一超限都直接放弃（调用方走无封面路径），绝不冒险分配。
    const DECODE_INPUT_LIMIT: usize = 1_200_000;
    if bytes.len() > DECODE_INPUT_LIMIT {
        return None;
    }

    // 1) MIME 提示（来自容器标签的 media_type 字段）
    if let Some(mime) = mime_hint {
        if let Some(fmt) = image::ImageFormat::from_mime_type(mime) {
            if let Some(img) = load_limited(bytes, fmt) {
                return Some(img);
            }
        }
    }
    // 2) 魔术字节探测
    if let Ok(fmt) = image::guess_format(bytes) {
        if let Some(img) = load_limited(bytes, fmt) {
            return Some(img);
        }
    }
    // 3) JPEG / PNG 暴力尝试（标签声明 MIME 但实际是另一种格式的情况）
    for fmt in [image::ImageFormat::Jpeg, image::ImageFormat::Png] {
        if let Some(img) = load_limited(bytes, fmt) {
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

    /// 构造声明 (w, h) 尺寸的全零 RGBA PNG（纯色像素压缩后极小，
    /// 模拟「几 KB 声明巨尺寸」的解压炸弹形态）。
    fn zeros_png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let raw = vec![0u8; w as usize * h as usize * 4];
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut buf,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(&raw, w, h, image::ExtendedColorType::Rgba8)
        .expect("应能编码 PNG");
        buf
    }

    /// 输出上限回归：压缩后远小于输入上限的 PNG 声明超过 4096 的宽/高
    /// 时必须拒绝——修复前无输出上限，此类声明会按默认 512MiB 分配额度
    /// 放行巨像素分配（解压炸弹）；修复后头部校验即拒，且不误伤限内图。
    #[test]
    fn decode_rejects_dimensions_over_output_limit() {
        let w_over = zeros_png(DECODE_MAX_DIM + 1, 4);
        assert!(
            w_over.len() < 1_000_000,
            "炸弹前提：纯色 PNG 压缩后应远小于输入上限"
        );
        assert!(decode(&w_over, None).is_none(), "超宽声明应拒绝");
        assert!(
            decode_thumb(&w_over, None, 32, 32).is_none(),
            "缩略路径同样拒绝"
        );

        let h_over = zeros_png(4, DECODE_MAX_DIM + 1);
        assert!(decode(&h_over, None).is_none(), "超高声明应拒绝");

        // 输出上限内的正常图不受影响。
        assert!(
            decode(&zeros_png(64, 64), None).is_some(),
            "限内图应正常解码"
        );
    }
}
