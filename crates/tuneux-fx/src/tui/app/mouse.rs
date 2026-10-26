//! 鼠标交互：单击选中行、双击激活（进目录 / 播放曲目）、滚轮移动选中并
//! 滚入可视窗；分组头单击折叠/展开。
//!
//! 命中矩形由渲染层每帧写入（`Cell`，&App 即可写）；处理层把 (col, row)
//! 映射为列表行号 = scroll + (row - rect.y)。双击 = 400ms 内同坐标两次
//! Left Down。终端鼠标为 opt-in 捕获，restore_terminal 与 panic hook 保证
//! 退出时恢复（不捕获则终端收不到鼠标事件，见 main.rs）。

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::App;
use crate::config::Config;
use crate::playlist::PlaylistRow;

/// 双击判定窗口（毫秒）。
const DOUBLE_CLICK_MS: u64 = 400;
/// 滚轮一档移动的行数。
const WHEEL_STEP: usize = 3;

/// 点击行 → 列表行索引（滚动基准 + 窗内偏移）；矩形上方点击返回 None。
fn row_index(rect: Rect, scroll: usize, y: u16) -> Option<usize> {
    let off = y.checked_sub(rect.y)? as usize;
    Some(scroll + off)
}

impl App {
    /// 鼠标事件入口（主循环 `Event::Mouse` 分支）。
    pub fn handle_mouse(&mut self, me: MouseEvent, config: &mut Config) {
        match me.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // 菜单栏（顶部行 0）：点击标题打开菜单
                if me.row == 0 {
                    self.mouse_menu_bar(me.column, config);
                    return;
                }
                // 菜单下拉已打开：命中下拉框内的项执行 / 框外只关闭不执行
                if self.menu_active && self.menu_dropdown {
                    self.menu_dropdown = false;
                    self.menu_active = false;
                    self.menu_sub = None;
                    // 命中判定：用渲染侧写入的下拉框实际矩形（inner 区），
                    // 而非按行号猜测——下拉 inner.y 起始于菜单栏下第 2 行
                    //（菜单栏 1 行 + 下拉框上边框 1 行），此前用 row-1 导致
                    // 整体差一行：点「载入播放列表」实际执行下一项「退出」。
                    let rect = self.menu_dropdown_rect.get();
                    if rect.contains((me.column, me.row).into()) {
                        let inner_y = rect.y;
                        let idx = (me.row - inner_y) as usize;
                        if let Some(menu) = crate::tui::app::menu::menus().get(self.menu_top) {
                            if let Some(item) = menu.items.get(idx) {
                                if item.children.is_some() {
                                    self.menu_active = true;
                                    self.menu_dropdown = true;
                                    self.menu_sub = Some(idx);
                                    self.menu_sub_item = 0;
                                } else {
                                    self.execute_menu_action(item.action, config);
                                }
                            }
                        }
                    }
                    // 框外点击只关闭（上面已关），不执行任何动作
                    return;
                }
                // 菜单已激活但点在下拉外：关闭
                if self.menu_active {
                    self.menu_active = false;
                    self.menu_dropdown = false;
                    return;
                }
                self.mouse_down(&me, config);
            }
            MouseEventKind::ScrollUp => self.mouse_wheel(&me, -1),
            MouseEventKind::ScrollDown => self.mouse_wheel(&me, 1),
            _ => {}
        }
    }

    /// 菜单栏鼠标点击：根据列坐标定位菜单标题并打开。
    fn mouse_menu_bar(&mut self, col: u16, _config: &mut Config) {
        let ms = crate::tui::app::menu::menus();
        let mut x: u16 = 1;
        for (i, menu) in ms.iter().enumerate() {
            let title = self.i18n.t(menu.title).into_owned();
            let title_w = unicode_width::UnicodeWidthStr::width(title.as_str()) as u16;
            let end = x + 1 + title_w + 1;
            if col >= x && col < end {
                self.menu_active = true;
                self.menu_top = i;
                self.menu_dropdown = true;
                return;
            }
            x += 1 + title_w + 2;
        }
    }

    /// 滚轮：选中移动 WHEEL_STEP 行并滚入可视窗（TUI 无自由滚动状态，
    /// 选中驱动滚动是诚实语义；与键盘上下键同口径）。
    fn mouse_wheel(&mut self, me: &MouseEvent, dir: i32) {
        let pos = Position::new(me.column, me.row);
        if self.browser_list_rect.get().contains(pos) {
            let n = self.browser.entries().len();
            if n == 0 {
                return;
            }
            let cur = self.browser.selected();
            let next = if dir < 0 {
                cur.saturating_sub(WHEEL_STEP)
            } else {
                (cur + WHEEL_STEP).min(n - 1)
            };
            self.browser.select(next);
        } else if self.playlist_list_rect.get().contains(pos) {
            let n = self.playlist.len();
            if n == 0 {
                return;
            }
            let cur = self.playlist.selected_track().unwrap_or(0);
            let next = if dir < 0 {
                cur.saturating_sub(WHEEL_STEP)
            } else {
                (cur + WHEEL_STEP).min(n - 1)
            };
            self.playlist.set_selected(next);
        }
    }

    /// 左键：单击 = 选中行（分组头 = 折叠/展开）；双击 = 激活
    /// （浏览器进目录 / 加入并播放；播放列表播放该曲）。
    fn mouse_down(&mut self, me: &MouseEvent, config: &mut Config) {
        let (x, y) = (me.column, me.row);
        let now = Instant::now();
        let dbl = matches!(
            self.last_click,
            Some((t, lx, ly))
                if now.duration_since(t) < Duration::from_millis(DOUBLE_CLICK_MS)
                    && lx == x
                    && ly == y
        );
        self.last_click = Some((now, x, y));
        let pos = Position::new(x, y);
        let brect = self.browser_list_rect.get();
        let prect = self.playlist_list_rect.get();
        if brect.contains(pos) {
            let Some(idx) = row_index(brect, self.browser.scroll(), y) else {
                return;
            };
            if idx < self.browser.entries().len() {
                self.browser.select(idx);
                if dbl {
                    self.handle_browser_enter(config);
                }
            }
        } else if prect.contains(pos) {
            let rows = self.playlist.visible_rows(config.playlist_view);
            let Some(ri) = row_index(prect, self.playlist.scroll(), y) else {
                return;
            };
            match rows.get(ri) {
                Some(PlaylistRow::Track { item_index }) => {
                    let item = *item_index;
                    self.playlist.set_selected(item);
                    if dbl {
                        self.play_and_update_current(item, config);
                    }
                }
                Some(PlaylistRow::AlbumHeader { album, .. }) => {
                    let album = album.clone();
                    self.playlist.toggle_album(&album);
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_index_maps_scroll_and_offset() {
        let rect = Rect::new(5, 10, 40, 20);
        assert_eq!(row_index(rect, 0, 10), Some(0));
        assert_eq!(row_index(rect, 0, 12), Some(2));
        assert_eq!(row_index(rect, 7, 10), Some(7));
        // 矩形上沿之上：无映射
        assert_eq!(row_index(rect, 3, 9), None);
    }
}
