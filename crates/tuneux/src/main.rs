//! # tuneux 程序入口
//!
//! tuneux 是一个跨平台命令行音乐播放器，界面、提示、元数据全中文。
//!
//! 本文件负责：
//! 1. 初始化终端（进入 raw 模式、切换备用屏幕）；
//! 2. 加载配置并启动 TUI 主事件循环；
//! 3. 退出时恢复终端状态并保存配置。

// 项目内部模块
mod config;
mod fs_browser;
mod lyrics;
mod media_key;
mod metadata;
mod playlist;
mod tui;

// 标准库
use std::io::{self, Stdout};

// 第三方库
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::config::Config;
use crate::tui::app::App;
use crate::tui::render::{draw, layout_metrics};

/// 应用程序错误类型。
///
/// 错误聚合为 trait 对象，避免为每种底层错误单独定义类型，
/// 简化代码。配置/音频/IO 错误对用户均不致命，调用方据此回退。
type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

/// 程序入口。
///
/// 流程：
/// 1. 加载配置（失败回退默认值，绝不阻塞启动）；
/// 2. 初始化终端；
/// 3. 进入主循环；
/// 4. 退出时保存配置、恢复终端状态。
///
/// 捕获任何错误打印到 stderr，并在退出前确保终端状态已恢复
/// （避免退出后终端异常）。
fn main() -> AppResult<()> {
    let mut config = config::load();

    let terminal = init_terminal()?;
    // 进入 raw 模式 + 备用屏后安装 panic hook：任何 panic 都必须先恢复
    // 终端再打印错误，否则用户终端会停留在 raw 模式（看不到输入、
    // 无法正常使用 shell），只能手工 `reset` 修复。
    install_panic_hook();
    // 无论 run 是否出错，都要恢复终端状态并保存配置
    let result = run(terminal, &mut config);
    restore_terminal()?;
    // 正常退出时持久化配置（音量/循环/随机/列宽/上次目录）
    config::save(&config);
    result
}

/// 安装 panic hook：panic 时先尽力恢复终端，再执行默认的 panic 打印。
///
/// 设计要点：
/// - TUI 程序进入 raw 模式 + 备用屏幕后，若 panic 直接 unwind 出 `main`，
///   `restore_terminal()` 不会执行，终端将残废——所以必须在进入终端态后
///   立即安装本 hook 兜底；
/// - hook 内不允许再次 panic（会导致 abort），因此恢复终端的所有调用
///   都忽略错误（`let _ =`）；
/// - 保留默认 hook 打印 panic 位置与信息，不吞掉诊断内容。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// 初始化终端：返回一个绑定了 crossterm 后端的 ratatui Terminal。
///
/// 步骤：
/// 1. `enable_raw_mode`：让终端不回显、不缓冲、不处理特殊键；
/// 2. `EnterAlternateScreen`：切换到备用屏幕缓冲，退出时自动恢复。
fn init_terminal() -> AppResult<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// 恢复终端到正常状态。
///
/// 必须在程序退出前调用，否则终端会停留在 raw 模式，
/// 用户看不到输入也无法正常使用 shell。
fn restore_terminal() -> AppResult<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// 主事件循环。
///
/// 标准的 TUI 循环：渲染 → 等待事件 → 处理事件 → 重复。
/// 按下 q 或 Ctrl+C 时退出循环，控制权回到 main 做配置保存与终端恢复。
fn run(mut terminal: Terminal<CrosstermBackend<Stdout>>, config: &mut Config) -> AppResult<()> {
    // 初始化应用状态：浏览器打开配置中的上次目录
    let mut app = App::new(config);
    // 系统媒体键：默认开启，可由 tuneux.toml 的 `media_keys_enabled = false` 关闭
    //（Windows LL 钩子是系统级全局行为、Linux MPRIS 服务名可能与其他播放器冲突）。
    // 事件经 crossbeam-channel 到达，主循环每帧 poll 并映射为播放动作；
    // 同时把引擎真实播放状态回灌（Linux MPRIS 桌面面板显示准确状态）。
    let media_key_handle = if config.media_keys_enabled {
        media_key::spawn_media_key_listener()
    } else {
        None
    };
    // 上次自动保存时间：用于定期落盘，防止直接关窗口丢失状态。
    let mut last_save = std::time::Instant::now();

    loop {
        // 在绘制前，根据当前终端尺寸调整浏览器滚动偏移，确保选中项可见。
        // 放在事件循环而非 draw 里，是因为：
        // 1. draw 拿到的是 &App（不可变），无法调 &mut ensure_visible；
        //    若在 draw 内 clone 整个 FsBrowser 会每帧深拷贝 Vec<Entry>；
        // 2. 在事件循环里基于 terminal.size() 直接计算高度，draw 只负责
        //    读取已算好的 scroll 值渲染——职责清晰且无多余拷贝。
        //
        // 高度推算统一走 render::layout_metrics（布局单一真相源），
        // 与 draw 内布局天然一致——此前这里手写 term_h-13 / main_h/2 等
        // 公式，与 draw 内 Constraint 是两份独立实现，容易漂移。
        //
        // 给 ensure_visible 传"实际能放多少行"，否则它以为还有空间不滚——
        // Half 模式时播放列表只能显示 ~7 行但 ensure_visible 以为是 16，
        // 选中项落到 7 之外就出"光标不动、列表不滚"的假象。
        let size = terminal.size()?;
        let metrics = layout_metrics(
            (size.width, size.height),
            app.spectrum_mode,
            app.lyrics_mode,
            app.left_panel,
            app.search_mode,
            app.search_target,
            config.browser_ratio,
        );
        app.browser.ensure_visible(metrics.browser_visible_h);
        // 播放列表同样需要滚动跟随（选中项移出可视区时滚动）。
        // playlist_visible_rows 已扣除边框 2 行与（列表搜索时的）输入框 1 行。
        app.ensure_playlist_visible(metrics.playlist_visible_rows);

        // 渲染界面（draw 需要 &mut app：电平乱码每帧推进 rng 状态）
        terminal.draw(|frame| draw(frame, &mut app, config))?;

        // 拉取 engine 错误 + 自动清除过期——每帧都做（100ms 一次轮询）
        app.refresh_last_error();

        // 事件等待：用 poll（100ms 超时）而非阻塞 read，
        // 这样每 100ms 能醒来检查音频引擎的 EOF 事件（用于自动下一曲等），
        // 同时也让进度条等动态信息能周期性刷新。
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // KeyEventKind::Press 过滤掉释放/重复事件，只处理按下
                if key.kind == KeyEventKind::Press {
                    if !app.handle_key(key, config) {
                        break;
                    }
                    // 浏览器位置变化后，记入配置便于下次启动恢复
                    config.last_dir = Some(app.browser.cwd().to_path_buf());
                }
            }
        }

        // 检查音频引擎是否报告"播放结束"（EOF）
        let got_finished = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_finished().is_some());
        // Gapless 无缝切曲事件：解码线程已无缝切换到预载曲目，
        // 前端只更新 UI 状态（不重发 Play，避免打断无缝衔接）。
        let gapless_switched = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_track_switched().is_some());
        if gapless_switched {
            // ReplayGain：无缝切曲也保存旧曲分析结果（引擎在切换时已写入）
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.replay_gain_cache.insert(path, db);
                }
            }
            app.advance_ui_on_gapless(config);
        }

        // CUE 分轨曲目到达终点（position >= end_ms）：整轨文件未 EOF，
        // 但本曲（INDEX 片段）已播完，同样视为"本曲结束"触发切下一曲
        // （修复：无结束边界会一路播到整轨末尾）。
        let cue_finished = app
            .playlist
            .current_index()
            .and_then(|i| app.playlist.items().get(i))
            .and_then(|item| item.cue.as_ref())
            .and_then(|cue| cue.end_ms)
            .is_some_and(|end_ms| {
                app.engine
                    .as_ref()
                    .is_some_and(|e| e.position() >= end_ms as f64 / 1000.0)
            });
        // ReplayGain：曲目分析完成，把整曲增益缓存（下次播放该曲生效）
        if got_finished || cue_finished {
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.replay_gain_cache.insert(path, db);
                }
            }
        }
        if got_finished || cue_finished {
            let outcome = app.playlist.next(config.repeat);
            match outcome {
                playlist::NavOutcome::Switch(_) | playlist::NavOutcome::Repeat => {
                    app.handle_nav_outcome(outcome, config);
                }
                playlist::NavOutcome::End => {
                    // 列表到尽头且关闭循环：什么都不做（流已自动暂停）
                }
            }
        }

        // 系统媒体键事件：Linux 桌面媒体键经 MPRIS / Windows 经 rdev 钩子到达，
        // 映射为 keymap 动作并执行（与 TUI 按键同路径）。
        if let Some(handle) = &media_key_handle {
            // 回灌真实播放状态与曲目标题（MPRIS 桌面面板显示用）
            if let (Some(playing), Some(title)) = (&handle.playing, &handle.title) {
                let is_playing = app.engine.as_ref().is_some_and(|e| e.is_playing());
                playing.store(is_playing, std::sync::atomic::Ordering::Relaxed);
                let title_str = app
                    .current_metadata
                    .as_ref()
                    .and_then(|m| m.title.clone())
                    .unwrap_or_default();
                *title.lock().unwrap() = title_str;
            }
            while let Ok(ev) = handle.rx.try_recv() {
                let action = ev.action().to_string();
                if app.execute_action(&action, config) {
                    // 动作已消费（播放/暂停/切曲）
                }
            }
        }

        // 同步音量到配置（退出时持久化）
        if let Some(engine) = &app.engine {
            config.volume = engine.volume();
            // 退出时存当前播放位置（"接着听"的关键数据）
            if let Some(path) = &app.current_path {
                let pos = engine.position();
                app.playlist_state.save_position(path, pos);
            }
        }

        // 每 5 秒自动落盘一次：直接关窗口（强杀）时，
        // 最多只丢最近 5 秒的状态，而非全部丢失。
        if last_save.elapsed() >= std::time::Duration::from_secs(5) {
            app.save_playlist_to_state();
            config::save_playlist_state(&app.playlist_state);
            config::save(config);
            last_save = std::time::Instant::now();
        }
    }
    // 退出前保存播放列表 + 进度到独立文件 playlist.toml，下次启动自动恢复。
    app.save_playlist_to_state();
    config::save_playlist_state(&app.playlist_state);
    Ok(())
}
