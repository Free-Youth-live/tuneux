//! 媒体信息：元数据缓存、歌词加载、封面解码。
//!
//! 本模块是 App 的媒体信息域（叶子模块，不依赖其他子模块）。

use image;
use tuneux_mediax::lyrics;
use tuneux_mediax::metadata;

use super::App;

impl App {
    /// 取元数据：优先从缓存读，miss 时调 from_file 并写入缓存。
    ///
    /// 容量上限：元数据缓存无限增长会持续积累内存。
    /// 超过上限时只清掉**非当前曲目**的条目——保留正在播放的元数据
    ///（封面/歌词频繁读取），避免整体清空后下一帧又得重提当前曲目。
    pub fn get_or_extract_metadata(&mut self, path: &std::path::Path) -> metadata::TrackMetadata {
        if let Some(cached) = self.metadata_cache.get(path) {
            return cached.clone();
        }
        const METADATA_CACHE_MAX: usize = 500;
        if self.metadata_cache.len() >= METADATA_CACHE_MAX {
            // 只保留当前播放曲目，逐出其余（retain 保留谓词为真的条目）。
            self.metadata_cache
                .retain(|k, _| self.current_path.as_ref() == Some(k));
            // 极端情况：当前曲目不在缓存，仍强制留一个空位。
            if self.metadata_cache.len() >= METADATA_CACHE_MAX {
                self.metadata_cache.clear();
            }
        }
        let md = metadata::TrackMetadata::from_file(path);
        // 缓存不保留封面原始字节（封面可达数百 KB，500 条缓存会积累大量内存）：
        // 封面只随当前曲目保留在 current_metadata；命中缓存切回时按需补提取。
        let mut cached = md.clone();
        cached.cover = None;
        self.metadata_cache.insert(path.to_path_buf(), cached);
        md
    }

    /// 按路径 + 目标尺寸取封面网格缩略图（带缓存）。
    ///
    /// 封面浏览网格专用：命中缓存直接返回 RGBA；未命中则提取封面字节→解码→缩放，
    /// 结果按 (路径, 目标宽, 目标高) 缓存（None=无封面/解码失败，也缓存避免重复探测）。
    /// 与 `get_or_extract_metadata`（缓存剥离封面字节）不同，这里专门保封面，供网格持久显示。
    pub fn cover_grid_thumb(
        &mut self,
        path: &std::path::Path,
        max_w: u32,
        max_h: u32,
    ) -> Option<&image::RgbaImage> {
        let key = (path.to_path_buf(), max_w, max_h);
        if !self.cover_thumb_cache.contains_key(&key) {
            let thumb = Self::build_cover_thumb(path, max_w, max_h);
            self.cover_thumb_cache.insert(key.clone(), thumb);
        }
        self.cover_thumb_cache.get(&key).and_then(|o| o.as_ref())
    }

    /// 提取封面字节并解码缩放为 RGBA 缩略图（无封面/解码失败返回 None）。
    fn build_cover_thumb(
        path: &std::path::Path,
        max_w: u32,
        max_h: u32,
    ) -> Option<image::RgbaImage> {
        let cover = metadata::TrackMetadata::from_file(path).cover?;
        let img = image::load_from_memory(&cover.bytes).ok()?;
        let (sw, sh) = (img.width().max(1), img.height().max(1));
        let scale = (max_w as f32 / sw as f32).min(max_h as f32 / sh as f32);
        let dw = ((sw as f32 * scale).round() as u32).max(1).min(max_w);
        let dh = ((sh as f32 * scale).round() as u32).max(1).min(max_h);
        Some(
            img.resize_exact(dw, dh, image::imageops::FilterType::Triangle)
                .to_rgba8(),
        )
    }

    /// 加载当前曲目的歌词（统一入口，消除两处重复逻辑）。
    ///
    /// 优先级：
    /// 1. 同目录同名 `.lrc`（如 稻香.mp3 → 稻香.lrc）——外部文件优先，方便用户自行替换；
    /// 2. 内嵌歌词标签（ID3v2 USLT / Vorbis LYRICS / MP4 ©lyr）——metadata
    ///    提取后复用 LRC 解析；纯文本（无时间戳）内嵌歌词视为无歌词。
    ///
    /// 注意：`.lrc` 存在但解析出 0 行（空文件/纯元数据）时同样落到内嵌兜底。
    pub(crate) fn load_lyrics_for(&mut self, path: &std::path::Path) -> Option<lyrics::Lyrics> {
        let lrc_path = path.with_extension("lrc");
        let lrc = lyrics::Lyrics::load_from_file(&lrc_path).filter(|l| !l.is_empty());
        lrc.or_else(|| {
            self.current_metadata
                .as_ref()
                .and_then(|m| m.lyrics.as_deref())
                .and_then(lyrics::Lyrics::from_embedded)
        })
    }

    /// 取当前曲目的封面（已解码），触发缓存填充。
    pub fn current_decoded_cover(&mut self) -> Option<&image::DynamicImage> {
        let (path, cover) = match (&self.current_path, self.current_metadata.as_ref()) {
            (Some(p), Some(md)) => (p.clone(), md.cover.as_ref()?),
            _ => return None,
        };
        // 负缓存：该曲目封面解码已失败过，直接跳过——避免每帧重试解码 + 刷日志。
        if self.cover_failed_path.as_ref() == Some(&path) {
            return None;
        }
        if let Some((cached_path, _)) = &self.cover_cache {
            if cached_path == &path {
                return self.cover_cache.as_ref().map(|(_, img)| img);
            }
        }
        // 缓存未命中——解码（慢路径，仅第一次）。
        // 按优先级尝试多种格式检测：MIME → 魔术字节 → JPEG/PNG 遍历。
        match Self::decode_cover_bytes(&cover.bytes, &cover.mime) {
            Some(img) => {
                self.cover_cache = Some((path, img));
                self.cover_cache.as_ref().map(|(_, img)| img)
            }
            None => {
                // 解码失败：记入负缓存（本曲不再重试），并经 UI 提示一次（自动过期）。
                self.cover_failed_path = Some(path);
                self.flash_message("封面解码失败（已跳过）");
                None
            }
        }
    }

    /// 确保当前曲目的封面缩略图已就绪（按路径 + 目标像素区缓存，命中则跳过 resize）。
    pub fn ensure_cover_thumb(&mut self, pixel_w: u32, pixel_h: u32) {
        let Some(path) = self.current_path.clone() else {
            return;
        };
        // 命中缓存（路径 + 目标像素区一致）：无需重算。
        if let Some((cp, pw, ph, _, _, _)) = &self.cover_thumb {
            if cp == &path && *pw == pixel_w && *ph == pixel_h {
                return;
            }
        }
        // 未命中：清旧条目，解码原图 + 按比例缩放，写入缓存。
        self.cover_thumb = None;
        let Some(img) = self.current_decoded_cover() else {
            return;
        };
        let (sw, sh) = (img.width().max(1), img.height().max(1));
        let scale = (pixel_w as f32 / sw as f32).min(pixel_h as f32 / sh as f32);
        let w = ((sw as f32 * scale).round() as u32).max(1).min(pixel_w);
        let h = ((sh as f32 * scale).round() as u32).max(1).min(pixel_h);
        let rgba = img
            .resize_exact(w, h, image::imageops::FilterType::Triangle)
            .to_rgba8();
        self.cover_thumb = Some((path, pixel_w, pixel_h, w, h, rgba));
    }

    /// 取当前缩略图（只读借用）。调用前应先 `ensure_cover_thumb` 命中。
    /// 返回 (实际缩放宽, 实际缩放高, RGBA)；无封面时 None。
    pub fn cover_thumb(&self) -> Option<(u32, u32, &image::RgbaImage)> {
        self.cover_thumb
            .as_ref()
            .map(|(_, _, _, dw, dh, rgba)| (*dw, *dh, rgba))
    }

    /// 解码封面原始字节为 DynamicImage。
    fn decode_cover_bytes(bytes: &[u8], mime: &str) -> Option<image::DynamicImage> {
        // 1) MIME → ImageFormat
        if let Some(fmt) = image::ImageFormat::from_mime_type(mime) {
            if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
                return Some(img);
            }
        }
        // 2) 魔术字节探测
        if let Ok(fmt) = image::guess_format(bytes) {
            if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
                return Some(img);
            }
        }
        // 3) 遍历 JPEG / PNG
        for fmt in [image::ImageFormat::Jpeg, image::ImageFormat::Png] {
            if let Ok(img) = image::load_from_memory_with_format(bytes, fmt) {
                return Some(img);
            }
        }
        // 解码失败由调用方记负缓存并经 UI 提示，不在此打日志（避免每帧刷屏）。
        None
    }
}
