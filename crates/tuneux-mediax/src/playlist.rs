//! # 播放导航核心（PlaylistCore）
//!
//! 「下一首是什么」是**音频领域问题**（音乐规则：顺序 / 循环 / 随机 /
//! 去重 / 显示序），收口在 mediax。UI 呈现状态（选中哪行、滚到哪、折叠
//! 哪个专辑、面板焦点）留在产品层独立结构——那是 ratatui 行渲染的产物，
//! max 的 egui 用虚拟滚动与 Selection widget，实现完全不同。
//!
//! # 单线程所有不变式
//!
//! `display_order` 的结果缓存在 `RefCell` 中以保持 `&self` 签名，因此
//! [`PlaylistCore`] 是 `!Sync`。**现状即单线程所有**（App 主循环独占）；
//! 将来任何产品若需跨线程访问，走缓存外移或锁化，不允许就地加
//! unsafe / Arc 硬绕。
//!
//! # 数据模型
//!
//! `items` 按**插入序**存储（这是播放序），`display_order()` 返回**显示序**
//! 索引（按专辑-曲序排序）。这样避免插入时排序破坏 current/history 索引。

use std::cmp::Ordering;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::playlist_item::{NavOutcome, PlaylistItem};
use crate::repeat::RepeatMode;

/// 播放列表的显示模式（`g` 键切换，存入配置）。
///
/// serde 表示（`"flat"` / `"by_album"`）已冻结——写入用户配置文件，
/// 变更 = 破坏性变更。各产品默认值不同（基础版 ByAlbum、fx Flat），
/// 由产品配置层用 `#[serde(default = "...")]` 各自注入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistView {
    /// 按专辑分组显示。
    ByAlbum,
    /// 平铺大排行。
    Flat,
}

/// 简单伪随机数生成器（splitmix64），避免引入 `rand` 依赖。
///
/// 分布对洗牌足够用，单测用固定种子验证确定性。收口为 mediax 公共类型，
/// 产品层频谱乱码等随机需求一并复用，消除双端 `rng_next` 副本。
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl Default for SplitMix64 {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitMix64 {
    /// 默认种子：固定常量。洗牌会话间不可复现但单测可设自己的种子。
    pub fn new() -> Self {
        Self {
            state: 0x1234567890abcdef,
        }
    }

    /// 用固定种子构造（确定性复现用，供产品测试 / 对拍）。
    pub fn with_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// splitmix64 单步。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// 0..n 的随机 usize（n=0 返回 0）。
    pub fn gen_range(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() as usize) % n
        }
    }
}

/// 播放导航核心（领域层）。
///
/// 持有条目集合、播放状态（current）、已播历史（history）、随机开关与
/// LCG 状态，以及「下一首 / 上一首 / 显示序 / 去重」等导航逻辑。
/// 不含任何 UI 呈现状态。
/// 随机模式（2026-10-09 扩容；serde 表示冻结为 snake_case 四值）。
///
/// - `Off`：顺序播放；
/// - `Tracks`：曲目袋式——全列表随机、一轮内不重复（v0.5.x 的 `shuffle = true` 同义）；
/// - `TrueRandom`：真随机——均匀抽取、允许跨轮即时重复（排除当前曲防连播死循环）；
/// - `Albums`：专辑袋式——随机选专辑、专辑内按曲序播完整张，再换下一张
///   （专辑分组按 `PlaylistItem::album`，未知专辑各自成组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShuffleMode {
    /// 关闭（顺序播放）。
    #[default]
    Off,
    /// 曲目袋式（不重复袋）。
    Tracks,
    /// 真随机（均匀，允许重复）。
    TrueRandom,
    /// 专辑袋式（整张连播）。
    Albums,
}

impl ShuffleMode {
    /// 冻结的 serde 字符串表示。
    pub fn as_str(self) -> &'static str {
        match self {
            ShuffleMode::Off => "off",
            ShuffleMode::Tracks => "tracks",
            ShuffleMode::TrueRandom => "true_random",
            ShuffleMode::Albums => "albums",
        }
    }
    /// 从冻结字符串解析（配置加载用）；未知值回落 Off。
    pub fn parse(s: &str) -> Self {
        match s {
            "tracks" => ShuffleMode::Tracks,
            "true_random" => ShuffleMode::TrueRandom,
            "albums" => ShuffleMode::Albums,
            _ => ShuffleMode::Off,
        }
    }
    /// 循环到下一档（`s` 键轻量循环：关→曲目→专辑→真随机→关）。
    pub fn next(self) -> Self {
        match self {
            ShuffleMode::Off => ShuffleMode::Tracks,
            ShuffleMode::Tracks => ShuffleMode::Albums,
            ShuffleMode::Albums => ShuffleMode::TrueRandom,
            ShuffleMode::TrueRandom => ShuffleMode::Off,
        }
    }
    /// 是否处于任一随机档（状态栏 / 预载逻辑判定用）。
    pub fn is_on(self) -> bool {
        !matches!(self, ShuffleMode::Off)
    }
}

/// 播放导航核心（媒体层）——条目集合 + 播放状态 + 随机模式 + 导航逻辑。
#[derive(Debug, Clone)]
pub struct PlaylistCore {
    items: Vec<PlaylistItem>,
    /// 当前播放项索引（按插入序）。None 表示还没开始播。
    current: Option<usize>,
    /// 已播历史（最近的在末尾），用于 shuffle 模式下的 prev 撤回。
    history: Vec<usize>,
    shuffle: ShuffleMode,
    rng: SplitMix64,
    /// 显示序缓存：`display_order()` 的结果，列表变更时置 None 失效。
    display_order_cache: std::cell::RefCell<Option<Vec<usize>>>,
}

impl Default for PlaylistCore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaylistCore {
    /// 新建空的播放导航核心。
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            current: None,
            history: Vec::new(),
            shuffle: ShuffleMode::Off,
            rng: SplitMix64::new(),
            display_order_cache: std::cell::RefCell::new(None),
        }
    }

    /// 用固定种子构造（确定性复现用，供产品测试 / 对拍）。
    pub fn new_with_seed(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            current: None,
            history: Vec::new(),
            shuffle: ShuffleMode::Off,
            rng: SplitMix64::with_seed(seed),
            display_order_cache: std::cell::RefCell::new(None),
        }
    }

    /// 加一个条目（按插入序追加，不立即排序——排序在显示时算）。
    pub fn add(&mut self, item: PlaylistItem) {
        self.items.push(item);
        self.invalidate_display_order();
    }

    /// 批量加入。
    pub fn add_many(&mut self, items: impl IntoIterator<Item = PlaylistItem>) {
        self.items.extend(items);
        self.invalidate_display_order();
    }

    /// 条目的去重键：`(path, cue.index)`。
    fn dedup_key(item: &PlaylistItem) -> (PathBuf, Option<u32>) {
        (item.path.clone(), item.cue.as_ref().map(|c| c.index))
    }

    /// 判断条目是否已存在（按去重键判定）。
    fn contains_dedup_key(&self, item: &PlaylistItem) -> bool {
        let key = Self::dedup_key(item);
        self.items
            .iter()
            .any(|existing| Self::dedup_key(existing) == key)
    }

    /// 去重加入单个条目：若 `(path, cue.index)` 已存在则跳过。
    ///
    /// 返回 true 表示实际加入了新条目。
    pub fn add_dedup(&mut self, item: PlaylistItem) -> bool {
        if self.contains_dedup_key(&item) {
            false
        } else {
            self.items.push(item);
            self.invalidate_display_order();
            true
        }
    }

    /// 去重批量加入，返回实际加入的新条目数量。
    pub fn add_many_dedup(&mut self, items: impl IntoIterator<Item = PlaylistItem>) -> usize {
        let mut added = 0;
        for item in items {
            if self.add_dedup(item) {
                added += 1;
            }
        }
        added
    }

    /// 清空列表，重置导航状态（UI 状态由产品层另行清理）。
    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
        self.history.clear();
        self.invalidate_display_order();
    }

    /// 删除指定项（item index），同步调整 current/history 索引。
    ///
    /// 删除当前项时 current 置 None（调用方应停止播放）。UI 状态
    /// （selected/scroll/折叠）由产品层在调用后另行调整。
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        self.invalidate_display_order();

        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
        self.history = self
            .history
            .iter()
            .filter(|&&i| i != index)
            .map(|&i| if i > index { i - 1 } else { i })
            .collect();
        true
    }

    /// 条目数量。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 播放列表是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 可变访问全部条目（排序 / 批量修改用）。
    ///
    /// 经此入口的物理重排/批量修改会使显示序缓存失真，此处一律先失效
    /// 缓存（O(1)），下次 `display_order()` 惰性重算。调用方无需再手动
    /// 处理缓存——曾因某条排序入口漏失效，导致显示行与物理条目错位
    /// （删 A 行删到 B 曲）。
    pub fn items_mut(&mut self) -> &mut [PlaylistItem] {
        self.invalidate_display_order();
        &mut self.items
    }

    /// 只读访问全部条目。
    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    /// 当前播放项下标（空列表为 None）。
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// 是否随机播放。
    pub fn shuffle_mode(&self) -> ShuffleMode {
        self.shuffle
    }

    /// 是否处于任一随机档（兼容视图：`shuffle_mode().is_on()` 的简写）。
    pub fn is_shuffle(&self) -> bool {
        self.shuffle.is_on()
    }

    /// 切换随机模式。开 → 启用并清空 history（让"未播"从全集开始）。
    pub fn set_shuffle(&mut self, on: bool) {
        self.set_shuffle_mode(if on {
            ShuffleMode::Tracks
        } else {
            ShuffleMode::Off
        });
    }

    /// 设定随机模式（曲目袋式 / 真随机 / 专辑袋式 / 关）。
    /// 切换即清空撤回历史（各模式的「已播」语义不同，跨模式历史无意义）。
    pub fn set_shuffle_mode(&mut self, mode: ShuffleMode) {
        if self.shuffle == mode {
            return;
        }
        self.shuffle = mode;
        if mode.is_on() {
            self.history.clear();
        }
    }

    /// 跳到指定项（用户手动点选）。失败（越界）返回 false。
    pub fn jump_to(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        if let Some(curr) = self.current {
            if curr != index {
                self.history.push(curr);
            }
        }
        self.current = Some(index);
        true
    }

    /// 仅设置当前项，不修改 history（供 next/prev 返回的 Switch 使用）。
    pub fn set_current(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.current = Some(index);
        true
    }

    /// 预览下一曲条目（不改变状态；Gapless 预载用）。
    ///
    /// 仅顺序播放可预测：单曲循环 / 随机播放返回 None。
    pub fn peek_next_item(&self, repeat: RepeatMode) -> Option<&PlaylistItem> {
        if self.items.is_empty() {
            return None;
        }
        if matches!(repeat, RepeatMode::Single) || self.shuffle.is_on() || self.items.len() == 1 {
            return None;
        }
        let order = self.display_order();
        let next_idx = match self
            .current
            .and_then(|c| order.iter().position(|&i| i == c))
        {
            Some(pos) if pos + 1 < order.len() => Some(order[pos + 1]),
            Some(_) if matches!(repeat, RepeatMode::List) => order.first().copied(),
            Some(_) => None,
            None => order.first().copied(),
        };
        next_idx.and_then(|idx| self.items.get(idx))
    }

    /// 进入下一曲（按 repeat + shuffle 算）。
    ///
    /// 顺序模式按 **display 顺序**走；shuffle 模式从所有未播过项里抽。
    pub fn next(&mut self, repeat: RepeatMode) -> NavOutcome {
        if self.items.is_empty() {
            return NavOutcome::End;
        }
        if matches!(repeat, RepeatMode::Single) || self.items.len() == 1 {
            return NavOutcome::Repeat;
        }

        let next_idx = if self.shuffle.is_on() {
            self.pick_by_shuffle_mode(repeat)
        } else {
            let order = self.display_order();
            match self
                .current
                .and_then(|c| order.iter().position(|&i| i == c))
            {
                Some(pos) if pos + 1 < order.len() => Some(order[pos + 1]),
                Some(_) if matches!(repeat, RepeatMode::List) => Some(order[0]),
                Some(_) => None,
                None => order.first().copied(),
            }
        };

        match next_idx {
            Some(idx) => {
                if let Some(curr) = self.current {
                    self.history.push(curr);
                }
                self.current = Some(idx);
                NavOutcome::Switch(idx)
            }
            None => NavOutcome::End,
        }
    }

    /// 进入上一曲。
    /// - 单曲循环：保持
    /// - shuffle：弹 history 栈顶
    /// - 顺序：按 display 顺序 current - 1
    pub fn prev(&mut self, repeat: RepeatMode) -> NavOutcome {
        if self.items.is_empty() {
            return NavOutcome::End;
        }
        if matches!(repeat, RepeatMode::Single) || self.items.len() == 1 {
            return NavOutcome::Repeat;
        }

        let prev_idx = if self.shuffle.is_on() {
            self.history.pop()
        } else {
            let order = self.display_order();
            match self
                .current
                .and_then(|c| order.iter().position(|&i| i == c))
            {
                Some(0) if matches!(repeat, RepeatMode::List) => order.last().copied(),
                Some(0) => None,
                Some(pos) => Some(order[pos - 1]),
                None => None,
            }
        };

        match prev_idx {
            Some(idx) => {
                self.current = Some(idx);
                NavOutcome::Switch(idx)
            }
            None => NavOutcome::End,
        }
    }

    /// 按 (album, track_number, path) 物理排序条目切片。
    pub fn sort_items(items: &mut [PlaylistItem]) {
        items.sort_by(Self::compare_items);
    }

    /// 两个条目的排序比较：(album, track_number, path) 字典序。
    pub fn compare_items(a: &PlaylistItem, b: &PlaylistItem) -> Ordering {
        match (a.album.as_deref(), b.album.as_deref()) {
            (Some(al), Some(bl)) => al
                .cmp(bl)
                .then(
                    a.track_number
                        .unwrap_or(u32::MAX)
                        .cmp(&b.track_number.unwrap_or(u32::MAX)),
                )
                .then(a.path.cmp(&b.path)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.path.cmp(&b.path),
        }
    }

    /// 使显示序缓存失效（列表内容变更后调用）。
    fn invalidate_display_order(&mut self) {
        *self.display_order_cache.borrow_mut() = None;
    }

    /// 计算显示序：按 (album, track_number) 排序，无专辑的项排末尾。
    /// 返回 indices 数组（插入序索引保持不变）。
    ///
    /// 结果缓存于 `display_order_cache`：列表未变更时多次调用零开销。
    pub fn display_order(&self) -> Vec<usize> {
        if let Some(cached) = self.display_order_cache.borrow().as_ref() {
            return cached.clone();
        }
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by(|&a, &b| Self::compare_items(&self.items[a], &self.items[b]));
        *self.display_order_cache.borrow_mut() = Some(order.clone());
        order
    }

    /// 拖拽重排：把插入序 `from` 的条目移动到 `to`（「移除后插入」语义：
    /// 先移除 from，再在 to 位置插入）。`current` 与 `history` 索引同步修正；
    /// 越界返回 false（不改动），from == to 视为成功空操作。
    ///
    /// 现代播放列表（tuneux-max）拖拽行重排用；经典 TUI 不消费。
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() {
            return false;
        }
        if from == to {
            return true;
        }
        let moved = self.items.remove(from);
        self.items.insert(to, moved);
        // 索引重映射：被移条目落在 to；区间内其余条目整体平移一格。
        let remap = |i: usize| -> usize {
            if i == from {
                return to;
            }
            if from < to {
                if i > from && i <= to {
                    i - 1
                } else {
                    i
                }
            } else if i >= to && i < from {
                i + 1
            } else {
                i
            }
        };
        self.current = self.current.map(remap);
        self.history = self.history.iter().map(|&i| remap(i)).collect();
        self.invalidate_display_order();
        true
    }

    /// 清空已播历史（随机模式的撤回栈）。物理重排后历史索引失效，
    /// 调用方应先清空再继续导航（现代列头排序用）；显示序缓存由
    /// [`Self::items_mut`] 自行失效，调用方无需关心。
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// 从"未播"集合（不含 current 和 history）随机抽一个。
    /// 按随机模式抽下一曲（next() 的 shuffle 分支唯一入口）。
    fn pick_by_shuffle_mode(&mut self, repeat: RepeatMode) -> Option<usize> {
        match self.shuffle {
            ShuffleMode::TrueRandom => self.pick_true_random(),
            ShuffleMode::Albums => self.pick_album_shuffle(repeat),
            _ => self.pick_tracks_bag(repeat),
        }
    }

    /// 真随机：全体（除当前曲）均匀抽取；一轮播完不清袋——连续两抽可能同曲。
    fn pick_true_random(&mut self) -> Option<usize> {
        let cands: Vec<usize> = (0..self.items.len())
            .filter(|i| Some(*i) != self.current)
            .collect();
        if cands.is_empty() {
            None
        } else {
            Some(cands[self.rng.gen_range(cands.len())])
        }
    }

    /// 曲目袋式（原 pick_random_unplayed 语义 + List 循环清袋）。
    fn pick_tracks_bag(&mut self, repeat: RepeatMode) -> Option<usize> {
        match self.pick_random_unplayed() {
            Some(i) => Some(i),
            None if matches!(repeat, RepeatMode::List) => {
                self.history.clear();
                if let Some(c) = self.current {
                    self.history.push(c);
                }
                self.pick_random_unplayed()
            }
            None => None,
        }
    }

    /// 专辑袋式：当前专辑未播完则按曲序续播；否则随机抽一张未播专辑播其首曲。
    /// 「已播」判定沿用条目级 history（专辑内全部条目进过 history 即视作播过）。
    fn pick_album_shuffle(&mut self, repeat: RepeatMode) -> Option<usize> {
        let groups = self.album_groups();
        // 当前曲所在组的剩余未播（组内按曲序）
        if let Some(cur) = self.current {
            if let Some(g) = groups.iter().find(|g| g.contains(&cur)) {
                let rest: Vec<usize> = g
                    .iter()
                    .copied()
                    .filter(|i| Some(*i) != self.current && !self.history.contains(i))
                    .collect();
                if let Some(next) = rest.first() {
                    return Some(*next);
                }
            }
        }
        // 抽未播组（组内存在未播条目）
        let fresh: Vec<&Vec<usize>> = groups
            .iter()
            .filter(|g| {
                g.iter()
                    .any(|i| Some(*i) != self.current && !self.history.contains(i))
            })
            .collect();
        if fresh.is_empty() {
            if matches!(repeat, RepeatMode::List) {
                self.history.clear();
                if let Some(c) = self.current {
                    self.history.push(c);
                }
                return self.pick_album_shuffle(RepeatMode::Off);
            }
            return None;
        }
        let g = fresh[self.rng.gen_range(fresh.len())];
        g.iter()
            .copied()
            .find(|i| Some(*i) != self.current && !self.history.contains(i))
    }

    /// 专辑分组：按 `PlaylistItem::album` 字符串分组（None 各自成组），
    /// 组内按（曲序, 路径序）稳定排序——专辑内按曲序连播。
    fn album_groups(&self) -> Vec<Vec<usize>> {
        let mut keyed: Vec<(Option<&str>, usize)> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.album.as_deref(), i))
            .collect();
        keyed.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| {
                let ta = self.items[a.1].track_number.unwrap_or(u32::MAX);
                let tb = self.items[b.1].track_number.unwrap_or(u32::MAX);
                ta.cmp(&tb).then_with(|| a.1.cmp(&b.1))
            })
        });
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for (album, idx) in keyed {
            match groups.last_mut() {
                Some(g) if g.iter().any(|i| self.items[*i].album.as_deref() == album) => {
                    g.push(idx)
                }
                _ => groups.push(vec![idx]),
            }
        }
        groups
    }

    fn pick_random_unplayed(&mut self) -> Option<usize> {
        let unplayed: Vec<usize> = (0..self.items.len())
            .filter(|i| Some(*i) != self.current && !self.history.contains(i))
            .collect();
        if unplayed.is_empty() {
            None
        } else {
            Some(unplayed[self.rng.gen_range(unplayed.len())])
        }
    }
}

// =============================================================================
// 单元测试（导航核心）
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn item(album: Option<&str>, track: Option<u32>, name: &str) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from(name),
            album: album.map(String::from),
            track_number: track,
            cue: None,
        }
    }

    #[test]
    fn peek_next_sequential() {
        let mut p = PlaylistCore::new();
        p.add(item(Some("A"), Some(1), "/m/1.mp3"));
        p.add(item(Some("A"), Some(2), "/m/2.mp3"));
        p.add(item(Some("A"), Some(3), "/m/3.mp3"));
        assert_eq!(
            p.peek_next_item(RepeatMode::Off).map(|it| it.path.clone()),
            Some(PathBuf::from("/m/1.mp3"))
        );
        p.set_current(0);
        assert_eq!(
            p.peek_next_item(RepeatMode::Off).map(|it| it.path.clone()),
            Some(PathBuf::from("/m/2.mp3"))
        );
        p.set_current(2);
        assert_eq!(p.peek_next_item(RepeatMode::Off), None);
        assert_eq!(
            p.peek_next_item(RepeatMode::List).map(|it| it.path.clone()),
            Some(PathBuf::from("/m/1.mp3"))
        );
    }

    #[test]
    fn next_off_advances_then_ends() {
        let mut p = PlaylistCore::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Switch(1));
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Switch(2));
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::End);
    }

    #[test]
    fn next_list_wraps_to_first() {
        let mut p = PlaylistCore::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(2);
        assert_eq!(p.next(RepeatMode::List), NavOutcome::Switch(0));
    }

    #[test]
    fn prev_off_decrements_then_ends() {
        let mut p = PlaylistCore::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(2);
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(1));
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(0));
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::End);
    }

    #[test]
    fn single_song_always_repeats() {
        let mut p = PlaylistCore::new();
        p.add(item(None, None, "/only.mp3"));
        p.jump_to(0);
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Repeat);
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Repeat);
    }

    #[test]
    fn shuffle_distribution_is_roughly_uniform() {
        let mut p = PlaylistCore::new_with_seed(0xcafe);
        p.set_shuffle(true);
        for i in 0..10 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        let mut counts = [0usize; 10];
        for _ in 0..1000 {
            if let NavOutcome::Switch(i) = p.next(RepeatMode::List) {
                if i < 10 {
                    counts[i] += 1;
                }
            }
        }
        for (i, &c) in counts.iter().enumerate() {
            assert!(
                c > 70 && c < 130,
                "位置 {i} 被选 {c} 次，分布偏 (期望 ~100)"
            );
        }
    }

    #[test]
    fn display_order_groups_by_album_then_track() {
        let mut p = PlaylistCore::new();
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(Some("A"), Some(3), "/a3.mp3"));
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        let order = p.display_order();
        let names: Vec<String> = order
            .iter()
            .map(|&i| p.items()[i].path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a3.mp3", "/b1.mp3"]);
    }

    #[test]
    fn sort_items_orders_by_album_track() {
        let mut items = vec![
            item(Some("B"), Some(1), "/b1.mp3"),
            item(Some("A"), Some(3), "/a3.mp3"),
            item(Some("A"), Some(1), "/a1.mp3"),
            item(Some("A"), Some(2), "/a2.mp3"),
        ];
        PlaylistCore::sort_items(&mut items);
        let names: Vec<&str> = items.iter().map(|i| i.path.to_str().unwrap()).collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a3.mp3", "/b1.mp3"]);
    }

    fn cue_item(path: &str, index: u32, title: &str) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from(path),
            album: Some("整轨专辑".to_string()),
            track_number: Some(index),
            cue: Some(crate::playlist_item::CueRef {
                index,
                title: title.to_string(),
                performer: None,
                start_ms: (index as u64 - 1) * 180_000,
                end_ms: None,
            }),
        }
    }

    #[test]
    fn dedup_skips_duplicate_path() {
        let mut p = PlaylistCore::new();
        assert!(p.add_dedup(item(None, None, "/a.mp3")));
        assert!(!p.add_dedup(item(None, None, "/a.mp3")));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn dedup_cue_tracks_with_different_index_not_deduped() {
        let mut p = PlaylistCore::new();
        assert!(p.add_dedup(cue_item("/album.flac", 1, "Track 1")));
        assert!(p.add_dedup(cue_item("/album.flac", 2, "Track 2")));
        assert!(p.add_dedup(cue_item("/album.flac", 3, "Track 3")));
        assert_eq!(p.len(), 3);
    }

    #[test]
    fn remove_adjusts_indices() {
        let mut p = PlaylistCore::new();
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(3);
        assert!(p.remove(1));
        assert_eq!(p.len(), 4);
        assert_eq!(p.current_index(), Some(2), "current 3 → 2");
    }

    #[test]
    fn remove_current_clears_it() {
        let mut p = PlaylistCore::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(1);
        assert!(p.remove(1));
        assert_eq!(p.current_index(), None);
    }

    #[test]
    fn add_dedup_invalidates_display_order_cache() {
        let mut p = PlaylistCore::new();
        assert_eq!(p.display_order().len(), 0);
        assert!(p.add_dedup(item(Some("A"), Some(1), "/a1.mp3")));
        assert_eq!(p.display_order().len(), 1);
    }

    // —— items_mut 物理重排的缓存失效回归（2026-10-13）——

    /// 与显示序同口径的物理重排：重排后 display_order 必须是恒等排列。
    /// 修复前此处返回预热阶段的旧缓存（[2,3,1,0]），行↔曲错位。
    #[test]
    fn items_mut_canonical_resort_invalidates_display_order_cache() {
        let mut p = PlaylistCore::new();
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(Some("A"), Some(3), "/a3.mp3"));
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        // 预热缓存（模拟排序前已渲染过列表）
        let warmed = p.display_order();
        assert_eq!(warmed, vec![2, 3, 1, 0]);
        // 列表排序的真实路径：经 items_mut 物理重排
        PlaylistCore::sort_items(p.items_mut());
        p.clear_history();
        // 物理序已与显示序口径一致，显示序必须是恒等排列
        let after = p.display_order();
        assert_eq!(after, vec![0, 1, 2, 3]);
        let names: Vec<&str> = after
            .iter()
            .map(|&i| p.items()[i].path.to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a3.mp3", "/b1.mp3"]);
    }

    /// 与显示序不同口径的物理重排（如按标题列头倒序）：display_order
    /// 必须按新物理序重算，而非返回旧缓存。这是误删/误播场景的直接回归。
    #[test]
    fn items_mut_custom_sort_recomputes_display_order() {
        let mut p = PlaylistCore::new();
        p.add(item(Some("A"), Some(1), "/x.mp3"));
        p.add(item(Some("A"), Some(2), "/y.mp3"));
        p.add(item(Some("A"), Some(3), "/z.mp3"));
        let warmed = p.display_order();
        assert_eq!(warmed, vec![0, 1, 2]);
        // 模拟按标题倒序列头排序：物理序变为 [z, y, x]
        p.items_mut().sort_by(|a, b| b.path.cmp(&a.path));
        p.clear_history();
        // 显示序按 (album, track) 对新物理序重算：
        // track 1/2/3 现在分别位于物理索引 2/1/0
        let after = p.display_order();
        assert_eq!(after, vec![2, 1, 0]);
        let tracks: Vec<u32> = after
            .iter()
            .map(|&i| p.items()[i].track_number.unwrap())
            .collect();
        assert_eq!(tracks, vec![1, 2, 3]);
    }

    #[test]
    fn splitmix64_is_deterministic() {
        let mut a = SplitMix64::with_seed(42);
        let mut b = SplitMix64::with_seed(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    // —— 拖拽重排（move_item）——

    /// 基本重排（后移）：条目顺序与 current 跟随。
    #[test]
    fn move_item_forward_moves_current_with_it() {
        let mut p = PlaylistCore::new();
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(1); // current = 1（jump_to 从 None 只设 current）
        assert!(p.move_item(1, 3));
        let names: Vec<String> = p
            .items()
            .iter()
            .map(|it| it.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["/0.mp3", "/2.mp3", "/3.mp3", "/1.mp3", "/4.mp3"]
        );
        // 被移条目（原 1）落在 to=3：current 跟随到 3。
        assert_eq!(p.current_index(), Some(3));
    }

    /// 区间平移：未移动条目的索引随移除方向整体修正（未移动 current 覆盖验证）。
    #[test]
    fn move_item_forward_shifts_range_indices() {
        let mut p = PlaylistCore::new();
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.set_current(4);
        // 后移（from=0 < to=3）：原 1..=3 各减一；原 4 不变。
        assert!(p.move_item(0, 3));
        assert_eq!(p.current_index(), Some(4), "区间外 current 不变");
        let names: Vec<String> = p
            .items()
            .iter()
            .map(|it| it.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["/1.mp3", "/2.mp3", "/3.mp3", "/0.mp3", "/4.mp3"]
        );
    }

    /// 前移（from > to）：区间条目后移一格。
    #[test]
    fn move_item_backward() {
        let mut p = PlaylistCore::new();
        for i in 0..4 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.set_current(3);
        assert!(p.move_item(3, 0));
        let names: Vec<String> = p
            .items()
            .iter()
            .map(|it| it.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["/3.mp3", "/0.mp3", "/1.mp3", "/2.mp3"]);
        assert_eq!(p.current_index(), Some(0), "current 跟随到新位置 0");
    }

    /// 越界拒绝（不改动）；from == to 空操作成功。
    #[test]
    fn move_item_bounds_and_noop() {
        let mut p = PlaylistCore::new();
        p.add(item(None, None, "/a.mp3"));
        p.add(item(None, None, "/b.mp3"));
        assert!(!p.move_item(0, 2), "to 越界应拒绝");
        assert!(!p.move_item(5, 0), "from 越界应拒绝");
        assert_eq!(p.len(), 2, "拒绝时不改动");
        assert!(p.move_item(1, 1), "from == to 空操作成功");
        let names: Vec<String> = p
            .items()
            .iter()
            .map(|it| it.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["/a.mp3", "/b.mp3"]);
    }

    /// clear_history：清空后 shuffle prev 无可撤回（End）。
    #[test]
    fn clear_history_empties_undo_stack() {
        let mut p = PlaylistCore::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.set_shuffle(true);
        p.jump_to(0);
        let _ = p.next(RepeatMode::List); // history 压入 0
        p.clear_history();
        assert_eq!(
            p.prev(RepeatMode::List),
            NavOutcome::End,
            "历史清空后 prev 无撤回"
        );
    }
}
