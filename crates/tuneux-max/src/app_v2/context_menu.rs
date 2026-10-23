//! # 统一右键菜单：五套手写弹层删并为一套（2026-10-15 右键菜单统一重写）
//!
//! - 数据模型：右键表运行时构建（[`CtxEntry`]），动作复用顶栏
//!   [`MenuAction`] 单分发通道（无第二套执行路径）；动态态（文案/勾选/禁用/
//!   目标）由构建期上下文注入，不进顶栏静态表；
//! - 渲染统一 `Frame::menu` 基底 + 透明按钮悬停高亮：配色全走皮肤调色板，
//!   圆角/阴影随界面范式（现代小圆角浅阴影 / 经典直角——Palette 只供色）；
//!   min_width 180、快捷键列右对齐、禁用灰显、长名省略号截断、子菜单 ▸
//!   悬停展开；
//! - 生命周期只此一份：frame_tick 自杀防御、顶栏弹层互斥（开着不弹）、
//!   右/下缘防出屏翻转、右键换对象原地重定位不关闭、↑↓/Enter/Esc 键盘
//!   导航、点外/滚轮/失焦关闭。

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Ui, Vec2};

use tuneux_commonx::I18n;

use super::menu::{shortcut_text, MenuAction};
use super::{mono, truncate_to_width, MaxAppV2, PlRow, FONT};
use crate::dock;
use crate::theme::Palette;

/// 菜单最小宽度（统一观感）。
const MIN_WIDTH: f32 = 180.0;
/// 标签截断显示宽度（CJK 全角 = 2 列；超出加 …）。
const LABEL_MAX_COLS: usize = 30;

/// 右键菜单项（运行时构建）。
pub(super) enum CtxEntry {
    /// 普通项（enabled = false 灰显不隐藏；checked = Some 显示勾选位）。
    Item {
        label: String,
        /// 快捷键标注（右列右对齐；空串 = 无）。
        shortcut: String,
        action: MenuAction,
        enabled: bool,
        checked: Option<bool>,
    },
    /// 分隔线。
    Sep,
    /// 子菜单（▸ 悬停展开；只设一级）。
    Sub {
        label: String,
        entries: Vec<CtxEntry>,
    },
}

/// 菜单种类（目录变化等失配场景的关闭判定用）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CtxKind {
    Browser,
    Playlist,
    Blank,
    Mods,
    DockLeaf,
}

/// 弹层状态（一份生命周期）。
pub(super) struct CtxMenuState {
    pub kind: CtxKind,
    entries: Vec<CtxEntry>,
    /// 弹出锚点（右键点；翻转以此为轴）。
    anchor: Pos2,
    /// 尺寸（首帧为估值，次帧起用实测校正翻转）。
    size: Vec2,
    /// 打开帧号（自杀防御：打开帧的右键是开启事件本身，必须排除）。
    opened_tick: u64,
    /// 键盘选中（顶层索引；None = 未用键盘）。
    sel: Option<usize>,
    /// 展开的子菜单（顶层索引）。
    open_sub: Option<usize>,
    /// 键盘焦点在子菜单层。
    kb_sub: bool,
    sub_sel: Option<usize>,
    /// 本帧菜单矩形（点外关闭判定）。
    rect: Rect,
    sub_rect: Option<Rect>,
}

// ==================== 内容构建（纯函数，可测） ====================

/// 普通项构造（快捷键标注按平台换算）。
fn item(i18n: &I18n, key: &str, shortcut: &str, action: MenuAction) -> CtxEntry {
    let sc = shortcut_text(&action, shortcut);
    CtxEntry::Item {
        label: i18n.t(key).into_owned(),
        shortcut: sc,
        action,
        enabled: true,
        checked: None,
    }
}

/// 曲目行：播放 / 属性 / 在文件管理器中显示 / ── / 移除。
fn track_entries(i18n: &I18n, row: usize, item_idx: usize, path: PathBuf) -> Vec<CtxEntry> {
    vec![
        item(i18n, "menu.ctx_play", "", MenuAction::PlayItem(item_idx)),
        item(i18n, "menu.properties", "i", MenuAction::PropertiesRow(row)),
        item(i18n, "menu.ctx_reveal", "", MenuAction::RevealPath(path)),
        CtxEntry::Sep,
        item(
            i18n,
            "menu.ctx_remove",
            "d",
            MenuAction::RemoveItem(item_idx),
        ),
    ]
}

/// 组头：折叠/展开（按状态切换文案）+ 展开全部。
fn header_entries(i18n: &I18n, album: &str, collapsed: bool) -> Vec<CtxEntry> {
    let key = if collapsed {
        "menu.ctx_expand"
    } else {
        "menu.ctx_collapse"
    };
    vec![
        item(
            i18n,
            key,
            "",
            MenuAction::ToggleGroupCollapse(album.to_string()),
        ),
        item(i18n, "menu.ctx_expand_all", "", MenuAction::ExpandAllGroups),
    ]
}

/// 浏览器行：目录 = 进入 / 加入此目录；文件 = 加入并播放 / 仅加入；
/// 末尾统一「在文件管理器中显示」。
/// （「属性」弹窗数据源仅播放列表行，浏览器文件行缓行——方案 §4.3 备注。）
fn browser_entries(i18n: &I18n, row: usize, path: PathBuf, is_dir: bool) -> Vec<CtxEntry> {
    let mut v = Vec::new();
    if is_dir {
        v.push(item(
            i18n,
            "menu.ctx_open",
            "",
            MenuAction::BrowserEnter(row),
        ));
        v.push(item(
            i18n,
            "menu.ctx_add_dir",
            "",
            MenuAction::BrowserAddDir(path.clone()),
        ));
    } else {
        v.push(item(
            i18n,
            "menu.ctx_add_play",
            "",
            MenuAction::BrowserAddFile(path.clone(), true),
        ));
        v.push(item(
            i18n,
            "menu.ctx_add_only",
            "a",
            MenuAction::BrowserAddFile(path.clone(), false),
        ));
    }
    v.push(CtxEntry::Sep);
    v.push(item(
        i18n,
        "menu.ctx_reveal",
        "",
        MenuAction::RevealPath(path),
    ));
    v
}

/// 空白区：打开文件 / 加入目录 / 载入 / 保存；播放列表侧加 清空。
fn blank_entries(i18n: &I18n, is_playlist: bool) -> Vec<CtxEntry> {
    let mut v = vec![
        item(i18n, "menu.open_files", "Ctrl+O", MenuAction::OpenFiles),
        item(i18n, "menu.add_dir", "", MenuAction::AddBrowserDir),
        CtxEntry::Sep,
        item(i18n, "menu.load_playlist", "", MenuAction::LoadPlaylist),
        item(i18n, "menu.save_playlist", "", MenuAction::SavePlaylist),
    ];
    if is_playlist {
        v.push(CtxEntry::Sep);
        v.push(item(
            i18n,
            "menu.clear_playlist",
            "x",
            MenuAction::ClearPlaylist,
        ));
    }
    v
}

/// dock 叶候选模块（未加装且非目标自身；与旧分页态同一过滤口径）。
fn dock_candidates(dock: Option<&dock::DockNode>, target: dock::ModuleId) -> Vec<dock::ModuleId> {
    dock::ModuleId::ALL
        .into_iter()
        .filter(|&m| m != target && !dock.is_some_and(|d| d.contains(m)))
        .collect()
}

/// dock 叶：关闭 + 拆分加装/更换为 ▸ 子菜单（模块候选为子项；空候选灰显）。
fn dock_leaf_entries(
    i18n: &I18n,
    target: dock::ModuleId,
    dock: Option<&dock::DockNode>,
) -> Vec<CtxEntry> {
    let names: Vec<(dock::ModuleId, String)> = dock_candidates(dock, target)
        .into_iter()
        .map(|m| (m, i18n.t(m.label_key()).into_owned()))
        .collect();
    let sub = |key: &str, f: &dyn Fn(dock::ModuleId) -> MenuAction| CtxEntry::Sub {
        label: i18n.t(key).into_owned(),
        entries: names
            .iter()
            .map(|(m, nm)| CtxEntry::Item {
                label: nm.clone(),
                shortcut: String::new(),
                action: f(*m),
                enabled: true,
                checked: None,
            })
            .collect(),
    };
    vec![
        item(
            i18n,
            "menu.dock_close",
            "",
            MenuAction::DockLeafClose(target),
        ),
        CtxEntry::Sep,
        sub("menu.dock_split_h", &|m| {
            MenuAction::DockLeafSplit(target, m, true)
        }),
        sub("menu.dock_split_v", &|m| {
            MenuAction::DockLeafSplit(target, m, false)
        }),
        sub("menu.dock_change", &|m| {
            MenuAction::DockLeafReplace(target, m)
        }),
    ]
}

/// 状态栏：皮肤模块加装勾选表。
fn mod_ctx_entries(entries: Vec<(&'static str, String, bool)>) -> Vec<CtxEntry> {
    entries
        .into_iter()
        .map(|(key, label, on)| CtxEntry::Item {
            label,
            shortcut: String::new(),
            action: MenuAction::ToggleMod(key),
            enabled: true,
            checked: Some(on),
        })
        .collect()
}

// ==================== 定位与测量（纯函数，可测） ====================

/// 弹出定位：右/下缘触界即以锚点为轴翻转；极小窗口钳进屏幕（永不出屏）。
fn place_menu(screen: Rect, anchor: Pos2, size: Vec2) -> Pos2 {
    let mut x = anchor.x;
    let mut y = anchor.y;
    if x + size.x > screen.right() {
        x = anchor.x - size.x;
    }
    if y + size.y > screen.bottom() {
        y = anchor.y - size.y;
    }
    x = x.clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    y = y.clamp(screen.top(), (screen.bottom() - size.y).max(screen.top()));
    Pos2::new(x, y)
}

/// 菜单尺寸估算（弹出前翻转用；略高估，首帧后由实测校正）。
fn measure_entries(ctx: &egui::Context, entries: &[CtxEntry]) -> Vec2 {
    let font = mono(FONT);
    let style = ctx.style();
    let pad = style.spacing.button_padding;
    let row_h =
        (FONT + pad.y * 2.0).max(style.spacing.interact_size.y) + style.spacing.item_spacing.y;
    let mut w: f32 = MIN_WIDTH;
    let mut h = 8.0;
    for e in entries {
        match e {
            CtxEntry::Sep => h += 9.0,
            CtxEntry::Item {
                label,
                shortcut,
                checked,
                ..
            } => {
                let mut t = truncate_label(label);
                if checked.is_some() {
                    t = format!("√ {t}");
                }
                if !shortcut.is_empty() {
                    t = format!("{t}    {shortcut}");
                }
                let g = ctx.fonts(|f| f.layout_no_wrap(t, font.clone(), Color32::WHITE));
                w = w.max(g.size().x + pad.x * 2.0 + 24.0);
                h += row_h;
            }
            CtxEntry::Sub { label, .. } => {
                let g = ctx.fonts(|f| {
                    f.layout_no_wrap(
                        format!("{}    ▸", truncate_label(label)),
                        font.clone(),
                        Color32::WHITE,
                    )
                });
                w = w.max(g.size().x + pad.x * 2.0 + 24.0);
                h += row_h;
            }
        }
    }
    Vec2::new(w + 8.0, h + 4.0)
}

/// 标签截断（长名省略号）。
fn truncate_label(label: &str) -> String {
    truncate_to_width(label, LABEL_MAX_COLS)
}

/// 可键盘选中的行（跳过分隔线/灰显/空子菜单）。
fn selectable(e: &CtxEntry) -> bool {
    match e {
        CtxEntry::Item { enabled, .. } => *enabled,
        CtxEntry::Sub { entries, .. } => !entries.is_empty(),
        CtxEntry::Sep => false,
    }
}

/// 步进到下一可选行（环形；无可选项返回 None）。
fn step_sel(entries: &[CtxEntry], cur: Option<usize>, dir: i32) -> Option<usize> {
    let n = entries.len();
    if n == 0 || !entries.iter().any(selectable) {
        return None;
    }
    let mut i = match cur {
        Some(c) => (c as i32 + dir).rem_euclid(n as i32) as usize,
        None if dir >= 0 => 0,
        None => n - 1,
    };
    for _ in 0..n {
        if selectable(&entries[i]) {
            return Some(i);
        }
        i = (i as i32 + dir).rem_euclid(n as i32) as usize;
    }
    None
}

/// 取普通项动作（灰显/分隔线/子菜单 → None）。
fn entry_action(e: Option<&CtxEntry>) -> Option<MenuAction> {
    match e {
        Some(CtxEntry::Item {
            action,
            enabled: true,
            ..
        }) => Some(action.clone()),
        _ => None,
    }
}

// ==================== 渲染 ====================

/// 菜单基底（圆角/阴影/底色随界面范式与皮肤调色板，不硬编码）。
fn menu_frame(ui: &Ui) -> egui::Frame {
    egui::Frame::menu(ui.style()).inner_margin(egui::Margin::same(4))
}

/// 菜单项观感：透明底 + 悬停高亮（配色全走皮肤调色板）。
fn prep_item_visuals(ui: &mut Ui, pal: &Palette) {
    let v = ui.visuals_mut();
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke.color = pal.fg;
    v.widgets.hovered.weak_bg_fill = pal.sel_bg;
    v.widgets.hovered.bg_fill = pal.sel_bg;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke.color = pal.fg;
    v.widgets.active.weak_bg_fill = pal.sel_bg;
    v.widgets.open.bg_stroke = Stroke::NONE;
}

/// 普通项按钮（勾选前缀 + 标签截断 + 快捷键右列；键盘选中行高亮）。
fn item_widget(
    label: &str,
    shortcut: &str,
    checked: Option<bool>,
    selected: bool,
    pal: &Palette,
) -> egui::Button<'static> {
    let text = match checked {
        Some(true) => format!("√ {}", truncate_label(label)),
        Some(false) => format!("  {}", truncate_label(label)),
        None => truncate_label(label),
    };
    let mut b = egui::Button::new(text);
    if !shortcut.is_empty() {
        b = b.right_text(egui::RichText::new(shortcut.to_string()).color(pal.fg_weak));
    }
    if selected {
        b = b.fill(pal.sel_bg);
    }
    b
}

/// 子菜单按钮（▸ 右对齐）。
fn sub_widget(label: &str, selected: bool, pal: &Palette) -> egui::Button<'static> {
    let b = egui::Button::new(truncate_label(label))
        .right_text(egui::RichText::new("▸").color(pal.fg_weak));
    if selected {
        b.fill(pal.sel_bg)
    } else {
        b
    }
}

/// 渲染一层条目（顶层/子菜单共用；子菜单层不允许再嵌套）。
#[allow(clippy::too_many_arguments)]
fn render_layer(
    ui: &mut Ui,
    ctx: &egui::Context,
    entries: &[CtxEntry],
    is_sub: bool,
    pal: &Palette,
    screen: Rect,
    sel: Option<usize>,
    open_sub: Option<usize>,
    sub_sel: Option<usize>,
    acts: &mut Vec<MenuAction>,
    hover_sub: &mut Option<usize>,
    sub_rect: &mut Option<Rect>,
) {
    prep_item_visuals(ui, pal);
    ui.set_min_width(MIN_WIDTH);
    ui.scope_builder(
        egui::UiBuilder::new().layout(egui::Layout::top_down_justified(egui::Align::Min)),
        |ui| {
            for (i, e) in entries.iter().enumerate() {
                match e {
                    CtxEntry::Sep => {
                        ui.separator();
                    }
                    CtxEntry::Item {
                        label,
                        shortcut,
                        action,
                        enabled,
                        checked,
                    } => {
                        let cur_sel = if is_sub { sub_sel } else { sel };
                        let b = item_widget(label, shortcut, *checked, cur_sel == Some(i), pal);
                        if ui.add_enabled(*enabled, b).clicked() {
                            acts.push(action.clone());
                        }
                    }
                    CtxEntry::Sub {
                        label,
                        entries: sub,
                    } => {
                        if is_sub {
                            // 子菜单只设一级：嵌套 Sub 防御性渲染为灰显行
                            ui.add_enabled(false, egui::Button::new(truncate_label(label)));
                            continue;
                        }
                        let r =
                            ui.add_enabled(!sub.is_empty(), sub_widget(label, sel == Some(i), pal));
                        if r.hovered() && !sub.is_empty() {
                            *hover_sub = Some(i);
                        }
                        if open_sub == Some(i) && !sub.is_empty() {
                            let size = measure_entries(ctx, sub);
                            let spos = place_menu(screen, r.rect.right_top(), size);
                            let sr = egui::Area::new(egui::Id::new("ctx_menu_sub"))
                                .order(egui::Order::Foreground)
                                .fixed_pos(spos)
                                .show(ctx, |ui| {
                                    menu_frame(ui).show(ui, |ui| {
                                        render_layer(
                                            ui, ctx, sub, true, pal, screen, sel, open_sub,
                                            sub_sel, acts, hover_sub, sub_rect,
                                        );
                                    });
                                });
                            *sub_rect = Some(sr.response.rect);
                        }
                    }
                }
            }
        },
    );
}

impl MaxAppV2 {
    /// 右键弹出许可：顶栏下拉/弹层开着时不弹（互斥；帧首快照 + 当前态双判）。
    fn ctx_can_open(&self, ctx: &egui::Context) -> bool {
        !egui::Popup::is_any_open(ctx) && !self.top_popup_at_frame_start
    }

    /// 打开/重定位菜单（换对象原地重开 = 覆盖旧状态，不经历关闭）。
    fn open_ctx(
        &mut self,
        ctx: &egui::Context,
        kind: CtxKind,
        entries: Vec<CtxEntry>,
        anchor: Pos2,
    ) {
        if entries.is_empty() {
            return;
        }
        let size = measure_entries(ctx, &entries);
        self.ctx_menu = Some(CtxMenuState {
            kind,
            entries,
            anchor,
            size,
            opened_tick: self.frame_tick,
            sel: None,
            open_sub: None,
            kb_sub: false,
            sub_sel: None,
            rect: Rect::NOTHING,
            sub_rect: None,
        });
    }

    /// 浏览器行右键（对象触发：换行原地重定位，不关闭）。
    pub(super) fn trigger_browser_ctx(&mut self, ctx: &egui::Context, row: usize, pos: Pos2) {
        if !self.ctx_can_open(ctx) {
            return;
        }
        let Some(e) = self.browser.entries().get(row) else {
            return;
        };
        let entries = browser_entries(&self.i18n, row, e.path().to_path_buf(), e.is_dir());
        self.open_ctx(ctx, CtxKind::Browser, entries, pos);
    }

    /// 播放列表行右键（曲目行 / 组头；对象触发同上）。
    pub(super) fn trigger_playlist_ctx(&mut self, ctx: &egui::Context, row: usize, pos: Pos2) {
        if !self.ctx_can_open(ctx) {
            return;
        }
        let rows = self.build_rows();
        let entries = match rows.get(row) {
            Some(PlRow::Track { item, .. }) => {
                let Some(it) = self.playlist.items().get(*item) else {
                    return;
                };
                track_entries(&self.i18n, row, *item, it.path.clone())
            }
            Some(PlRow::Header { album, .. }) => {
                header_entries(&self.i18n, album, self.collapsed.contains(album))
            }
            None => return,
        };
        self.open_ctx(ctx, CtxKind::Playlist, entries, pos);
    }

    /// dock 叶标题带右键（对象触发同上）。
    pub(super) fn trigger_dock_leaf_ctx(
        &mut self,
        ctx: &egui::Context,
        target: dock::ModuleId,
        pos: Pos2,
    ) {
        if !self.ctx_can_open(ctx) {
            return;
        }
        let entries = dock_leaf_entries(&self.i18n, target, self.config.dock.as_ref());
        self.open_ctx(ctx, CtxKind::DockLeaf, entries, pos);
    }

    /// 区域触发（空白区/状态栏）：帧首已有菜单开着时只关不开
    /// （防「旧菜单关闭的同帧新菜单弹出」观感）。
    pub(super) fn trigger_zone_ctx(&mut self, ctx: &egui::Context, hp: Pos2) {
        if self.ctx_menu.is_some() || self.menu_open_at_frame_start || !self.ctx_can_open(ctx) {
            return;
        }
        if self.status_rect.contains(hp) {
            let mods = self
                .mod_entries()
                .into_iter()
                .map(|(k, l)| (k, l, self.mod_on(k)))
                .collect();
            self.open_ctx(ctx, CtxKind::Mods, mod_ctx_entries(mods), hp);
        } else if self.browser_rect.contains(hp) {
            self.open_ctx(ctx, CtxKind::Blank, blank_entries(&self.i18n, false), hp);
        } else if self.playlist_rect.contains(hp) {
            self.open_ctx(ctx, CtxKind::Blank, blank_entries(&self.i18n, true), hp);
        }
    }

    /// 键盘导航（↑↓ 移动 · Enter 执行 · Enter/→ 展开子菜单 · ←/Esc 收层/关闭）。
    /// 菜单开着时由输入轮询调用，这些键不穿透到底层列表。
    pub(super) fn ctx_menu_keys(&mut self, ctx: &egui::Context) {
        let (up, dn, lf, rt, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if !(up || dn || lf || rt || enter || esc) {
            return;
        }
        let mut act: Option<MenuAction> = None;
        let mut close = false;
        if let Some(st) = &mut self.ctx_menu {
            if st.kb_sub {
                // 子菜单层
                let sub_idx = st.open_sub.filter(|&p| {
                    matches!(st.entries.get(p), Some(CtxEntry::Sub { entries, .. }) if !entries.is_empty())
                });
                match sub_idx {
                    None => {
                        st.kb_sub = false;
                        st.sub_sel = None;
                    }
                    Some(p) => {
                        if esc || lf {
                            st.kb_sub = false;
                            st.open_sub = None;
                            st.sub_sel = None;
                        } else if up || dn {
                            let dir = if dn { 1 } else { -1 };
                            if let Some(CtxEntry::Sub { entries, .. }) = st.entries.get(p) {
                                st.sub_sel = step_sel(entries, st.sub_sel, dir);
                            }
                        } else if enter {
                            if let Some(CtxEntry::Sub { entries, .. }) = st.entries.get(p) {
                                act = st.sub_sel.and_then(|j| entry_action(entries.get(j)));
                            }
                        }
                    }
                }
            } else if esc {
                close = true;
            } else if up || dn {
                let dir = if dn { 1 } else { -1 };
                st.sel = step_sel(&st.entries, st.sel, dir);
                st.open_sub = None;
            } else if enter || rt {
                match st.sel.and_then(|i| st.entries.get(i)) {
                    Some(CtxEntry::Item {
                        action,
                        enabled: true,
                        ..
                    }) => act = Some(action.clone()),
                    Some(CtxEntry::Sub { entries, .. }) if !entries.is_empty() => {
                        st.open_sub = st.sel;
                        st.kb_sub = true;
                        st.sub_sel = step_sel(entries, None, 1);
                    }
                    _ => {}
                }
            }
        }
        if let Some(a) = act {
            self.ctx_menu = None;
            self.exec_menu_action(a, ctx);
        } else if close {
            self.ctx_menu = None;
        }
    }

    /// 渲染弹层 + 生命周期判定（全应用只此一份）。
    pub(super) fn render_ctx_menu(&mut self, ctx: &egui::Context) {
        if self.ctx_menu.is_none() {
            return;
        }
        // 顶栏下拉/弹层开着 → 互斥关闭；窗口失焦 → 关闭
        if egui::Popup::is_any_open(ctx) || !ctx.input(|i| i.focused) {
            self.ctx_menu = None;
            return;
        }
        let pal = self.palette.clone();
        let screen = ctx.screen_rect();
        let frame_tick = self.frame_tick;

        let mut acts: Vec<MenuAction> = Vec::new();
        let mut hover_sub: Option<usize> = None;
        let mut sub_rect: Option<Rect> = None;

        let Some(st) = self.ctx_menu.as_ref() else {
            return;
        };
        let pos = place_menu(screen, st.anchor, st.size);
        let resp = egui::Area::new(egui::Id::new("ctx_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                menu_frame(ui).show(ui, |ui| {
                    render_layer(
                        ui,
                        ctx,
                        &st.entries,
                        false,
                        &pal,
                        screen,
                        st.sel,
                        st.open_sub,
                        st.sub_sel,
                        &mut acts,
                        &mut hover_sub,
                        &mut sub_rect,
                    );
                });
            });

        let mut close = !acts.is_empty();
        if let Some(st) = self.ctx_menu.as_mut() {
            st.rect = resp.response.rect;
            st.sub_rect = sub_rect;
            // 实测尺寸校正翻转（首帧估值偏差次帧起消除）
            st.size = resp.response.rect.size();
            if let Some(i) = hover_sub {
                // 仅在切换目标时重置子菜单键盘态（鼠标驻留当前行不打断键盘导航）
                if st.open_sub != Some(i) {
                    st.open_sub = Some(i);
                    st.kb_sub = false;
                    st.sub_sel = None;
                }
            }
            // 唯一一份「同帧自杀」防御：打开帧的右键是开启事件本身，必须排除。
            // 左键点外 / 右键点外（非打开帧）/ 滚轮点外 → 关闭；
            // 右键换对象重定位已把打开帧刷成当前帧，此处自然豁免。
            let (pri, sec, scroll, hp) = ctx.input(|i| {
                (
                    i.pointer.button_clicked(egui::PointerButton::Primary),
                    i.pointer.button_clicked(egui::PointerButton::Secondary),
                    i.smooth_scroll_delta != Vec2::ZERO,
                    i.pointer.hover_pos(),
                )
            });
            let inside = hp
                .is_some_and(|p| st.rect.contains(p) || st.sub_rect.is_some_and(|r| r.contains(p)));
            close = close
                || (pri && !inside)
                || (sec && st.opened_tick != frame_tick && !inside)
                || (scroll && !inside);
        }
        if close {
            self.ctx_menu = None;
        }
        for a in acts {
            self.exec_menu_action(a, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{DockNode, ModuleId};

    fn zh() -> I18n {
        tuneux_commonx::build_i18n("zh", crate::config::zh_table())
    }

    fn actions_of(entries: &[CtxEntry]) -> Vec<&MenuAction> {
        entries
            .iter()
            .filter_map(|e| match e {
                CtxEntry::Item { action, .. } => Some(action),
                _ => None,
            })
            .collect()
    }

    // —— 定位翻转 ——

    /// 中央点击不翻转。
    #[test]
    fn place_menu_center_no_flip() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let pos = place_menu(screen, Pos2::new(100.0, 100.0), Vec2::new(180.0, 100.0));
        assert_eq!(pos, Pos2::new(100.0, 100.0));
    }

    /// 右缘触界向左翻转，下缘触界向上翻转，右下角两边翻转。
    #[test]
    fn place_menu_edge_flip() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let size = Vec2::new(180.0, 100.0);
        let r = place_menu(screen, Pos2::new(790.0, 100.0), size);
        assert_eq!(r, Pos2::new(610.0, 100.0));
        let b = place_menu(screen, Pos2::new(100.0, 590.0), size);
        assert_eq!(b, Pos2::new(100.0, 490.0));
        let rb = place_menu(screen, Pos2::new(790.0, 590.0), size);
        assert_eq!(rb, Pos2::new(610.0, 490.0));
        for p in [r, b, rb] {
            let rect = Rect::from_min_size(p, size);
            assert!(screen.contains_rect(rect), "翻转后菜单必须完整在屏内");
        }
    }

    /// 极小窗口（菜单比窗口大）钳到左上角，不出屏。
    #[test]
    fn place_menu_tiny_window_clamps() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 80.0));
        let pos = place_menu(screen, Pos2::new(90.0, 70.0), Vec2::new(180.0, 100.0));
        assert_eq!(pos, Pos2::ZERO);
    }

    // —— 右键表项与 MenuAction 映射 ——

    /// 空白区：打开/加目录/载入/保存四项 + 播放列表侧追加清空（x）。
    #[test]
    fn blank_entries_mapping() {
        let i18n = zh();
        let pl = blank_entries(&i18n, true);
        assert_eq!(
            actions_of(&pl),
            vec![
                &MenuAction::OpenFiles,
                &MenuAction::AddBrowserDir,
                &MenuAction::LoadPlaylist,
                &MenuAction::SavePlaylist,
                &MenuAction::ClearPlaylist,
            ]
        );
        let br = blank_entries(&i18n, false);
        assert!(!actions_of(&br).contains(&&MenuAction::ClearPlaylist));
        // 快捷键列：清空 = x；打开文件按平台 ⌘O / Ctrl+O
        let clear = pl.iter().find_map(|e| match e {
            CtxEntry::Item {
                action: MenuAction::ClearPlaylist,
                shortcut,
                ..
            } => Some(shortcut),
            _ => None,
        });
        assert_eq!(clear.map(|s| s.as_str()), Some("x"));
    }

    /// 曲目行：播放 / 属性(i) / reveal / 移除(d)，动作携带行与底层索引。
    #[test]
    fn track_entries_mapping() {
        let i18n = zh();
        let v = track_entries(&i18n, 5, 3, PathBuf::from("/tmp/a.flac"));
        assert_eq!(
            actions_of(&v),
            vec![
                &MenuAction::PlayItem(3),
                &MenuAction::PropertiesRow(5),
                &MenuAction::RevealPath(PathBuf::from("/tmp/a.flac")),
                &MenuAction::RemoveItem(3),
            ]
        );
        // 分隔线恰一条
        assert_eq!(v.iter().filter(|e| matches!(e, CtxEntry::Sep)).count(), 1);
    }

    /// 组头：折叠/展开文案随状态切换（回归红线 3）。
    #[test]
    fn header_entries_dynamic_label() {
        let i18n = zh();
        let collapsed = header_entries(&i18n, "专辑A", true);
        let expanded = header_entries(&i18n, "专辑A", false);
        let label_of = |v: &[CtxEntry]| match &v[0] {
            CtxEntry::Item { label, .. } => label.clone(),
            _ => String::new(),
        };
        assert_eq!(label_of(&collapsed), i18n.t("menu.ctx_expand"));
        assert_eq!(label_of(&expanded), i18n.t("menu.ctx_collapse"));
        assert_eq!(
            actions_of(&expanded),
            vec![
                &MenuAction::ToggleGroupCollapse("专辑A".to_string()),
                &MenuAction::ExpandAllGroups,
            ]
        );
    }

    /// 浏览器行：目录 = 进入/加入目录/reveal；文件 = 加入并播放/仅加入(a)/reveal。
    #[test]
    fn browser_entries_mapping() {
        let i18n = zh();
        let dir = browser_entries(&i18n, 2, PathBuf::from("/tmp/d"), true);
        assert_eq!(
            actions_of(&dir),
            vec![
                &MenuAction::BrowserEnter(2),
                &MenuAction::BrowserAddDir(PathBuf::from("/tmp/d")),
                &MenuAction::RevealPath(PathBuf::from("/tmp/d")),
            ]
        );
        let file = browser_entries(&i18n, 2, PathBuf::from("/tmp/f.mp3"), false);
        assert_eq!(
            actions_of(&file),
            vec![
                &MenuAction::BrowserAddFile(PathBuf::from("/tmp/f.mp3"), true),
                &MenuAction::BrowserAddFile(PathBuf::from("/tmp/f.mp3"), false),
                &MenuAction::RevealPath(PathBuf::from("/tmp/f.mp3")),
            ]
        );
    }

    /// dock 叶：候选 = 全集减自身减已加装（回归红线 6：模块全集不丢项）；
    /// 三个 ▸ 子菜单（水平/垂直拆分、更换）覆盖同一候选集。
    #[test]
    fn dock_leaf_candidates_complete() {
        let i18n = zh();
        // 空树：候选 = ALL 减目标
        let cands = dock_candidates(None, ModuleId::Browser);
        assert_eq!(cands.len(), ModuleId::ALL.len() - 1);
        assert!(!cands.contains(&ModuleId::Browser));
        // 已有 Browser+Playlist：候选排除两者
        let tree = DockNode::Split {
            side_by_side: true,
            ratio: 0.5,
            a: Box::new(DockNode::leaf(ModuleId::Browser)),
            b: Box::new(DockNode::leaf(ModuleId::Playlist)),
        };
        let cands = dock_candidates(Some(&tree), ModuleId::Browser);
        assert_eq!(cands.len(), ModuleId::ALL.len() - 2);
        assert!(!cands.contains(&ModuleId::Playlist));

        let v = dock_leaf_entries(&i18n, ModuleId::Browser, Some(&tree));
        // 顶层：关闭 + 分隔线 + 三个子菜单
        assert_eq!(v.len(), 5);
        assert!(matches!(v[0], CtxEntry::Item { .. }));
        let subs: Vec<&[CtxEntry]> = v
            .iter()
            .filter_map(|e| match e {
                CtxEntry::Sub { entries, .. } => Some(entries.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(subs.len(), 3);
        // 每个子菜单都覆盖完整候选集（与旧分页态条目集合一致）
        for sub in subs {
            let acts = actions_of(sub);
            assert_eq!(acts.len(), ModuleId::ALL.len() - 2);
            for m in &cands {
                assert!(
                    acts.iter().any(|a| matches!(a,
                        MenuAction::DockLeafSplit(ModuleId::Browser, mm, _)
                        | MenuAction::DockLeafReplace(ModuleId::Browser, mm) if mm == m)),
                    "候选 {m:?} 应在子菜单中"
                );
            }
        }
        // 水平/垂直拆分动作标志位
        let h = actions_of(match &v[2] {
            CtxEntry::Sub { entries, .. } => entries,
            _ => &[],
        });
        assert!(h
            .iter()
            .all(|a| matches!(a, MenuAction::DockLeafSplit(_, _, true))));
        // 全模块都已加装：子菜单为空 → 行灰显不可选
        let mut full_tree = DockNode::leaf(ModuleId::Browser);
        for &m in ModuleId::ALL.iter().skip(1) {
            full_tree.split_insert(ModuleId::Browser, m, true);
        }
        let full = dock_leaf_entries(&i18n, ModuleId::Browser, Some(&full_tree));
        for e in &full {
            if let CtxEntry::Sub { entries, .. } = e {
                assert!(entries.is_empty(), "全加装后子菜单应为空");
                assert!(!selectable(e), "空子菜单应灰显");
            }
        }
    }

    /// 键盘步进：跳过分隔线，环形。
    #[test]
    fn step_sel_skips_separators() {
        let i18n = zh();
        let v = blank_entries(&i18n, true);
        // 布局：0 打开 1 加目录 Sep 3 载入 4 保存 Sep 6 清空
        assert_eq!(step_sel(&v, None, 1), Some(0));
        assert_eq!(step_sel(&v, Some(1), 1), Some(3));
        assert_eq!(step_sel(&v, Some(4), 1), Some(6));
        assert_eq!(step_sel(&v, Some(6), 1), Some(0));
        assert_eq!(step_sel(&v, Some(0), -1), Some(6));
    }
}
