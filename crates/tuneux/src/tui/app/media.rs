//! 媒体信息：元数据缓存、歌词加载、封面解码。
//!
//! 本模块是 App 的媒体信息域（叶子模块，不依赖其他子模块）。

use crate::lyrics;
use crate::metadata;
use image;

use super::App;

impl App {
    /// 取元数据：优先从缓存读，miss 时调 from_file 并写入缓存。
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

    pub fn get_or_extract_metadata(&mut self, path: &std::path::Path) -> metadata::TrackMetadata {
        if let Some(cached) = self.metadata_cache.get(path) {
            return cached.clone();
        }
        // 容量上限：元数据缓存（含封面原始字节）无限增长会持续积累内存。
        // 超过上限时只清掉**非当前曲目**的条目——保留正在播放的元数据
        //（封面/歌词频繁读取），避免整体清空后下一帧又得重提当前曲目
        //（原实现 O(500) 整体清空 + 热数据全丢）。
        const METADATA_CACHE_MAX: usize = 500;
        if self.metadata_cache.len() >= METADATA_CACHE_MAX {
            self.metadata_cache.retain(|k, _| {
                // 保留当前播放曲目（若有）
                self.current_path.as_ref() != Some(k)
            });
            // 极端情况：全是当前曲目（不太可能），仍强制留一个空位
            if self.metadata_cache.len() >= METADATA_CACHE_MAX {
                self.metadata_cache.clear();
            }
        }
        let md = metadata::TrackMetadata::from_file(path);
        self.metadata_cache.insert(path.to_path_buf(), md.clone());
        md
    }

    /// 取当前曲目的封面（已解码），触发缓存填充。
    pub fn current_decoded_cover(&mut self) -> Option<&image::DynamicImage> {
        let (path, cover) = match (&self.current_path, self.current_metadata.as_ref()) {
            (Some(p), Some(md)) => (p.clone(), md.cover.as_ref()?),
            _ => return None,
        };
        if let Some((cached_path, _)) = &self.cover_cache {
            if cached_path == &path {
                return self.cover_cache.as_ref().map(|(_, img)| img);
            }
        }
        // 缓存未命中——解码（慢路径，仅第一次）。
        // 按优先级尝试多种格式检测：MIME → 魔术字节 → JPEG/PNG 遍历。
        let img = Self::decode_cover_bytes(&cover.bytes, &cover.mime)?;
        self.cover_cache = Some((path, img));
        self.cover_cache.as_ref().map(|(_, img)| img)
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
        eprintln!("[封面] 解码失败（MIME={mime}，{} 字节）", bytes.len());
        None
    }
}
