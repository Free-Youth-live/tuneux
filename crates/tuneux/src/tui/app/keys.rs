//! 按键处理：键描述规范化、keymap 反查、动作执行、主按键分派。
//!
//! 依赖 playback / search / actions，是按键的最终入口。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Config, LeftPanel};
use crate::playlist;
use tuneux_corex as audio;

use super::search::SearchTarget;
use super::App;

/// 把一个按键事件转成规范键描述（与 `config::parse_key_desc` 输出同一套格式）。
///
/// 规则：
/// - `Char(' ')` → `"space"`；
/// - 字母键：无修饰符原样（`'n'`→`"n"`）；带 SHIFT 时转小写并加 `"shift+"`
///   前缀（终端常把 Shift+n 上报为 `Char('N') + SHIFT`，统一成 `"shift+n"`）；
/// - 符号键（`+`、`-`、`=`、`/` 等）：忽略 SHIFT——打出符号本身常需按住
///   Shift，若计入前缀会导致 `"+"` 永远匹配不上，故按符号本身输出；
/// - 具名键映射为小写单词（`Up`→`"up"`、`Tab`→`"tab"` 等）；
/// - 无法描述的键（F1~F12、Insert、Delete、PageUp 等）返回 None，
///   不参与自定义映射。
fn key_to_desc(key: &KeyEvent) -> Option<String> {
    // 先解出基础键名；空值 = 不可自定义的键
    let base = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => {
            // 字母 + SHIFT：统一小写，交给下方 shift+ 前缀（屏蔽终端大小写差异）
            if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_alphabetic() {
                c.to_ascii_lowercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Tab => "tab".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        _ => return None,
    };

    // 前缀修饰符；符号键的 SHIFT 是"打出符号"本身，不计入前缀
    let is_alpha = matches!(key.code, KeyCode::Char(c) if c.is_ascii_alphabetic());
    let mut desc = String::new();
    if is_alpha && key.modifiers.contains(KeyModifiers::SHIFT) {
        desc.push_str("shift+");
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        desc.push_str("ctrl+");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        desc.push_str("alt+");
    }
    desc.push_str(&base);
    Some(desc)
}

/// 介质档位中文名（状态栏提示用；与 fx 介质菜单同源措辞）。
fn medium_display_name(m: audio::PlaybackMedium) -> &'static str {
    match m {
        audio::PlaybackMedium::None => "关闭",
        audio::PlaybackMedium::TapeClear => "磁带·透明（高保真）",
        audio::PlaybackMedium::TapeWhite => "磁带·白色（清新）",
        audio::PlaybackMedium::TapeClassic => "磁带·深棕（经典）",
        audio::PlaybackMedium::TapeAged => "磁带·红色（老化）",
        audio::PlaybackMedium::VinylClean => "黑胶·蓝色（低噪声）",
        audio::PlaybackMedium::VinylDynamic => "黑胶·红色（高动态）",
        audio::PlaybackMedium::VinylStandard => "黑胶·黑色（标准）",
        audio::PlaybackMedium::VinylAged => "黑胶·彩胶（老化）",
        _ => "未知",
    }
}

impl App {
    /// 把按键事件翻译成自定义动作名：`KeyEvent → 键描述 → 反查 keymap`。
    ///
    /// 两侧（事件侧与配置侧）都经 `parse_key_desc` 规范化后再比较，
    /// 容忍大小写、别名、修饰符顺序等写法差异。找不到返回 None。
    fn keycode_to_action<'a>(
        &self,
        key: &KeyEvent,
        keymap: &'a std::collections::HashMap<String, String>,
    ) -> Option<&'a str> {
        let desc = key_to_desc(key)?;
        keymap.iter().find_map(|(action, configured)| {
            if crate::config::parse_key_desc(configured).as_deref() == Some(desc.as_str()) {
                Some(action.as_str())
            } else {
                None
            }
        })
    }

    /// 执行一个自定义动作，返回 true 表示按键已被消费。
    ///
    /// 未知动作名返回 false，调用方据此回退到硬编码默认键。
    pub(crate) fn execute_action(&mut self, action: &str, config: &mut Config) -> bool {
        match action {
            // toggle_play：与硬编码空格键分支等价
            "toggle_play" => {
                self.toggle_play();
                true
            }
            // next / prev：复用既有切曲函数（与硬编码 n / p 同路径）
            "next" => {
                self.advance_to_next_track(config);
                true
            }
            "prev" => {
                self.advance_to_prev_track(config);
                true
            }
            // volume_up / volume_down：与硬编码 + / - 分支等价
            "volume_up" => {
                self.volume_up();
                true
            }
            "volume_down" => {
                self.volume_down();
                true
            }
            // 未知动作名：忽略，回退默认键
            _ => false,
        }
    }

    /// 封面浏览模式按键：Up/Down/j/k 移动、Enter 播放、PageUp/PageDown 翻页；
    /// 其余键（含 c 切换、n/p 切曲、Left/Right seek、l 歌词）返回 false 走全局，避免冲突。
    fn handle_cover_browser_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        let albums = self.cover_browser_albums();
        let count = albums.len();
        if count == 0 {
            // 空列表无可导航：不吞键，让 c 仍能关闭封面面板。
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cover_browser_sel = self.cover_browser_sel.saturating_sub(1);
                self.cover_browser_scroll_into_view(count);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cover_browser_sel = (self.cover_browser_sel + 1).min(count - 1);
                self.cover_browser_scroll_into_view(count);
            }
            KeyCode::Enter => {
                if let Some((_, path)) = albums.get(self.cover_browser_sel) {
                    let path = path.clone();
                    if let Some(idx) = self.playlist.items().iter().position(|it| it.path == path) {
                        self.playlist.jump_to(idx);
                        self.play_and_update_current(idx, config);
                    }
                }
            }
            KeyCode::PageDown => {
                let step = self.cover_browser_visible.max(1);
                self.cover_browser_scroll = (self.cover_browser_scroll + step).min(count - 1);
                self.cover_browser_sel = self.cover_browser_scroll;
            }
            KeyCode::PageUp => {
                let step = self.cover_browser_visible.max(1);
                self.cover_browser_scroll = self.cover_browser_scroll.saturating_sub(step);
                self.cover_browser_sel = self.cover_browser_scroll;
            }
            _ => return false,
        }
        true
    }

    /// 让选中项滚入可视窗：选中在 scroll 之前则回退 scroll，超出窗口末尾则前推 scroll。
    fn cover_browser_scroll_into_view(&mut self, count: usize) {
        let visible = self.cover_browser_visible.max(1);
        if self.cover_browser_sel < self.cover_browser_scroll {
            self.cover_browser_scroll = self.cover_browser_sel;
        }
        if self.cover_browser_sel >= self.cover_browser_scroll + visible {
            self.cover_browser_scroll = self.cover_browser_sel + 1 - visible;
        }
        self.cover_browser_scroll = self.cover_browser_scroll.min(count - 1);
    }

    /// 处理一个按键事件。返回 false 表示请求退出。
    pub fn handle_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        // —— 关于弹窗（最优先：任意键关闭并吃掉，避免误触发其它操作）——
        if self.about_visible {
            self.about_visible = false;
            return true;
        }

        // 清空确认：5 秒过期（与提示同寿命），或按非 x 键取消。
        if self.pending_clear {
            let expired = self
                .pending_clear_at
                .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(5));
            if expired || !matches!(key.code, KeyCode::Char('x')) {
                self.pending_clear = false;
                self.pending_clear_at = None;
            }
        }

        // —— 搜索模式（吃掉所有按键，按目标面板分派）——
        if self.search_mode {
            match key.code {
                KeyCode::Esc => {
                    self.search_mode = false;
                    self.search_query.clear();
                    // 退出浏览器搜索时恢复一级条目
                    if self.search_target == SearchTarget::Browser {
                        self.browser.end_search();
                    }
                    return true;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.apply_search_query();
                    return true;
                }
                // 搜索态导航只用 ↑/↓：j/k 让位给输入（否则含 j/k 的歌名敲不出来）。
                KeyCode::Up => {
                    self.search_nav(-1);
                    return true;
                }
                KeyCode::Down => {
                    self.search_nav(1);
                    return true;
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.apply_search_query();
                    return true;
                }
                KeyCode::Enter => {
                    match self.search_target {
                        SearchTarget::Playlist => {
                            if let Some(sel) = self.playlist.selected_track() {
                                if self.filter_playlist().contains(&sel) {
                                    // 手动点选播放：压入 history。
                                    self.playlist.jump_to(sel);
                                    self.play_and_update_current(sel, config);
                                }
                            }
                        }
                        SearchTarget::Browser => {
                            self.handle_browser_enter(config);
                        }
                    }
                    return true;
                }
                _ => return true,
            }
        }

        // —— 封面浏览模式：Up/Down/j/k 移动、Enter 播放、PageUp/PageDown 翻页 ——
        // 置于搜索模式之后：搜索态优先吃键，封面浏览不与搜索输入抢键。
        if self.left_panel == LeftPanel::CoverBrowser && self.handle_cover_browser_key(key, config)
        {
            return true;
        }

        // —— 自定义键映射层（配置驱动，优先于下方硬编码默认键）——
        // KeyEvent → 键描述 → 反查 keymap：命中且为已知动作则执行并返回；
        // 未命中或动作未知则回退到原有硬编码默认键逻辑。
        if let Some(action) = self.keycode_to_action(&key, &config.keymap) {
            // 先复制出动作名，断开对 config 的不可变借用，便于下方可变借用
            let action = action.to_string();
            if self.execute_action(&action, config) {
                return true;
            }
            // 未知动作名：忽略该映射，继续走默认键
        }

        // —— 全局键（不受焦点影响）——
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return false;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    playlist::Panel::Browser => playlist::Panel::Playlist,
                    playlist::Panel::Playlist => playlist::Panel::Browser,
                };
                return true;
            }
            KeyCode::Char('r') => {
                config.repeat = config.repeat.next();
                // 循环模式变了，刷新预载目标：顺序模式预载下一曲，
                // 单曲/随机清掉旧预载（否则旧预载曲会在切曲时抢先播放，声音与界面错位）。
                self.refresh_preload(config);
                return true;
            }
            KeyCode::Char('s') => {
                config.shuffle = !config.shuffle;
                self.playlist.set_shuffle(config.shuffle);
                // 随机开关变了，同样刷新预载目标。
                self.refresh_preload(config);
                return true;
            }
            KeyCode::Char('n') => {
                self.advance_to_next_track(config);
                return true;
            }
            KeyCode::Char('p') => {
                self.advance_to_prev_track(config);
                return true;
            }
            KeyCode::Char(' ') => {
                self.toggle_play();
                return true;
            }
            // ←/→：快退/快进 5 秒（全局）
            // CUE 分轨曲目：seek 目标经 clamp_cue_seek 钳制在本曲区间 [start, end] 内
            KeyCode::Left => {
                if let Some(engine) = &self.engine {
                    let new_pos = (engine.position() - 5.0).max(0.0);
                    engine.send(audio::AudioCmd::Seek(self.clamp_cue_seek(new_pos)));
                }
                return true;
            }
            KeyCode::Right => {
                if let Some(engine) = &self.engine {
                    let dur = engine.duration();
                    let new_pos = if dur > 0.0 {
                        (engine.position() + 5.0).min(dur)
                    } else {
                        engine.position() + 5.0
                    };
                    engine.send(audio::AudioCmd::Seek(self.clamp_cue_seek(new_pos)));
                }
                return true;
            }
            // +/-：音量增减（全局）
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.volume_up();
                return true;
            }
            KeyCode::Char('-') => {
                self.volume_down();
                return true;
            }
            KeyCode::Char('a') => {
                self.add_current_browser_to_playlist(config);
                return true;
            }
            KeyCode::Char('b') => {
                // b：Hidden ↔ Browser（互斥：开浏览器会关掉封面）
                if self.left_panel == LeftPanel::Browser {
                    self.left_panel = LeftPanel::Hidden;
                    self.focus = playlist::Panel::Playlist;
                } else {
                    self.left_panel = LeftPanel::Browser;
                    self.focus = playlist::Panel::Browser;
                }
                config.left_panel = self.left_panel;
                return true;
            }
            KeyCode::Char('v') => {
                self.spectrum_mode = self.spectrum_mode.next();
                config.spectrum_mode = self.spectrum_mode;
                return true;
            }
            KeyCode::Char('l') => {
                // l：切换歌词（显示/隐藏），可与频谱共存。
                self.lyrics_mode = self.lyrics_mode.next();
                config.lyrics_mode = self.lyrics_mode;
                return true;
            }
            KeyCode::Char('m') => {
                // m：循环播放介质风格（无 → 4 磁带 → 4 黑胶 → 无），只改声音，
                // 用 corex 的 ALL 单一真源，新增介质无需改此处。
                let all = audio::PlaybackMedium::ALL;
                let idx = all
                    .iter()
                    .position(|&m| m == self.playback_medium)
                    .unwrap_or(0);
                self.playback_medium = all[(idx + 1) % all.len()];
                // 介质只改声音（corex DSP），不占界面、不动左面板。
                if let Some(engine) = &self.engine {
                    engine.set_medium(self.playback_medium);
                }
                config.playback_medium = self.playback_medium.as_str().to_string();
                // 界面不再显示介质，状态栏提示当前档位（声音变化不易一眼看出）。
                self.last_error = Some(format!(
                    "介质：{}",
                    medium_display_name(self.playback_medium)
                ));
                self.last_error_at = Some(std::time::Instant::now());
                return true;
            }
            KeyCode::Char('x') => {
                // x：清空播放列表（两次确认，防误触）。
                if self.pending_clear {
                    self.clear_playlist();
                    self.pending_clear = false;
                    self.pending_clear_at = None;
                    self.last_error = Some("播放列表已清空".to_string());
                    self.last_error_at = Some(std::time::Instant::now());
                } else if !self.playlist.is_empty() {
                    self.pending_clear = true;
                    self.pending_clear_at = Some(std::time::Instant::now());
                    self.last_error = Some("再按一次 x 确认清空播放列表".to_string());
                    self.last_error_at = Some(std::time::Instant::now());
                }
                return true;
            }
            KeyCode::Char('d') => {
                // d：删除选中的曲目（选中组头时忽略）。
                self.remove_selected_track();
                return true;
            }
            KeyCode::Char('g') => {
                // g：切换播放列表视图（平铺 ↔ 按专辑分组），并记入配置。
                self.playlist.toggle_view();
                config.playlist_view = self.playlist.view();
                return true;
            }
            KeyCode::Char('c') => {
                // c：Hidden → Cover → CoverBrowser → Hidden（互斥：开封面会关掉浏览器）。
                // 封面/封面浏览不是可聚焦面板，显示时焦点归回播放列表。
                self.left_panel = match self.left_panel {
                    LeftPanel::Cover => LeftPanel::CoverBrowser,
                    LeftPanel::CoverBrowser => LeftPanel::Hidden,
                    _ => LeftPanel::Cover,
                };
                if self.left_panel != LeftPanel::Hidden {
                    self.focus = playlist::Panel::Playlist;
                }
                config.left_panel = self.left_panel;
                return true;
            }
            KeyCode::Char('?') => {
                self.about_visible = !self.about_visible;
                return true;
            }
            _ => {}
        }

        // —— 焦点相关键 ——
        match self.focus {
            playlist::Panel::Browser => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.browser.move_up(),
                KeyCode::Down | KeyCode::Char('j') => self.browser.move_down(),
                KeyCode::Home => self.browser.move_to_top(),
                KeyCode::End => self.browser.move_to_bottom(),
                KeyCode::Char('/') => {
                    self.search_mode = true;
                    self.search_target = SearchTarget::Browser;
                    self.search_query.clear();
                    // 进入搜索态后异步收集目录树（大目录按 `/` 不卡 UI），
                    // 收集期间维持一级列表，结果到达后关键字立即生效。
                    self.browser.begin_search();
                    self.search_async();
                }
                KeyCode::Enter => {
                    self.handle_browser_enter(config);
                }
                KeyCode::Backspace => {
                    // 异步返回上级目录（后台线程读，不卡 UI）。
                    if let Some(parent) = self.browser.cwd().parent() {
                        let parent = parent.to_path_buf();
                        self.navigate_async(&parent);
                    }
                }
                _ => {}
            },
            playlist::Panel::Playlist => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.playlist.move_selection_up(),
                KeyCode::Down | KeyCode::Char('j') => self.playlist.move_selection_down(),
                KeyCode::Home => self.playlist.move_selection_to_top(),
                KeyCode::End => self.playlist.move_selection_to_bottom(),
                KeyCode::Char('/') => {
                    self.search_mode = true;
                    self.search_target = SearchTarget::Playlist;
                    self.search_query.clear();
                    self.jump_selected_to_filter_first();
                }
                KeyCode::Enter => {
                    // 先取出选中对象（clone），避免后续可变借用与 selected() 的
                    // 不可变借用冲突。
                    let selection = self.playlist.selected().cloned();
                    match selection {
                        Some(playlist::Selection::Track(i)) => {
                            // 手动点选播放：压入 history。
                            self.playlist.jump_to(i);
                            self.play_and_update_current(i, config);
                        }
                        Some(playlist::Selection::Album(a)) => {
                            self.playlist.toggle_album(&a);
                        }
                        None => {
                            if !self.playlist.is_empty() {
                                self.playlist.jump_to(0);
                                self.play_and_update_current(0, config);
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.focus = playlist::Panel::Browser;
                }
                _ => {}
            },
        }
        true
    }
}
