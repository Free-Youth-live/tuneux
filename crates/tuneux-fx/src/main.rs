//! # tuneux-fx 程序入口
//!
//! tuneux-fx 是插件化命令行音乐播放器（TUI）。界面组织参照 foobar2000
//! （菜单栏 + 以播放列表为核心的工作区 + 状态栏 + 功能键栏）；快捷键与
//! tuneux 统一。
//!
//! 本文件负责：
//! 1. 初始化终端（进入 raw 模式、切换备用屏幕）；
//! 2. 加载配置并启动 TUI 主事件循环；
//! 3. 退出时恢复终端状态并保存配置与播放列表。

// 项目内部模块（配置/浏览器/媒体键/播放列表/TUI；歌词/元数据在 mediax）
mod config;
mod fs_browser;
mod media_key;
mod playlist;
mod tui;

// 标准库
use std::io::{self, Stdout};
use std::time::Duration;

// 第三方库
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::config::{RepeatMode, SpectrumMode};
use crate::playlist::NavOutcome;
use crate::tui::app::App;
use crate::tui::render::{draw, layout_metrics};

/// 应用错误类型：聚合为 trait 对象，配置/音频/IO 错误对用户均不致命。
type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

/// 程序入口：加载配置 → 初始化终端 → 主循环 → 退出时保存配置、恢复终端。
fn main() -> AppResult<()> {
    let mut config = config::load();
    let terminal = init_terminal()?;
    // 进入 raw 模式 + 备用屏后安装 panic hook：任何 panic 都必须先恢复终端。
    install_panic_hook();
    // 无论 run 是否出错，都要恢复终端状态并保存配置。
    let result = run(terminal, &mut config);
    restore_terminal()?;
    config::save(&config);
    result
}

/// panic hook：panic 时先尽力恢复终端再打印，避免终端残废在 raw 模式。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// 初始化终端：raw 模式 + 备用屏幕，返回绑定 crossterm 后端的 Terminal。
fn init_terminal() -> AppResult<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    Ok(Terminal::new(backend)?)
}

/// 恢复终端到正常状态（退出前必须调用）。
fn restore_terminal() -> AppResult<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// 主事件循环：渲染 → 等待事件 → 处理事件 → 重复；q / Ctrl+C 退出。
///
/// 用带超时的 poll 而非阻塞 read，以便每帧轮询音频引擎的 EOF 事件
///（自动切下一曲）并周期刷新进度条。超时自适应：播放中且频谱可见时
/// 约 30 FPS（频谱顺滑），否则 100 ms（省电）。
fn run(
    mut terminal: Terminal<CrosstermBackend<Stdout>>,
    config: &mut crate::config::Config,
) -> AppResult<()> {
    let mut app = App::new(config);
    // 上次自动保存时间：定期落盘，防直接关窗口丢状态。
    let mut last_save = std::time::Instant::now();
    // 上一帧时刻：用于计算帧间隔，驱动频谱峰值按秒衰减（帧率无关）。
    let mut last_frame = std::time::Instant::now();
    // 系统媒体键：默认开启，可由配置的 `media_keys_enabled = false` 关闭。
    // 平台不支持（macOS）或 D-Bus 不可用（headless）时返回 None，静默缺失。
    let media_key_handle = if config.media_keys_enabled {
        media_key::spawn_media_key_listener()
    } else {
        None
    };

    loop {
        // 绘制前按当前终端尺寸校正浏览器/播放列表的滚动偏移（确保选中项可见）。
        // 高度推算统一走 layout_metrics（布局单一真相源）。
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
        // 目录异步载入结果：到达后应用到浏览器（后台线程读大目录不卡 UI）。
        if let Ok((gen, resolved, result)) = app.dir_load_rx.try_recv() {
            // 只应用最新一次导航的结果，丢弃陈旧结果。
            if gen + 1 == app.dir_load_gen {
                match result {
                    Ok(entries) => {
                        // 搜索期间到达的导航结果：新目录使搜索范围失效，
                        // 一并退出搜索态（apply_loaded 会复位浏览器搜索状态，
                        // 这里同步清 App 侧标志，避免两边状态错位）。
                        if app.in_browser_search() {
                            app.search_mode = false;
                            app.search_query.clear();
                        }
                        // 应用即记录「上次目录」：异步导航的 cwd 此刻才变，
                        // 只在按键后记录会存进旧目录（Enter 后立即退出尤甚）。
                        config.last_dir = Some(resolved.clone());
                        app.browser.apply_loaded(resolved, entries);
                    }
                    Err(e) => app.flash_message(&e),
                }
            }
        }
        // 浏览器搜索的异步收集结果：代次匹配（重进搜索前的陈旧收集丢弃）
        // 且仍在搜索态时提交给浏览器；已退出搜索则由其内部丢弃。
        if let Ok((gen, entries, truncated)) = app.search_load_rx.try_recv() {
            if gen + 1 == app.search_load_gen {
                app.browser.apply_search_collected(entries, truncated);
            }
        }
        // 目录递归加入的后台结果：批量合入（条目 + 新探测元数据补缓存）。
        // 多批次全部生效——加入是累积语义（与导航的「最新生效」不同）。
        if let Ok((dir, items, mds)) = app.add_load_rx.try_recv() {
            app.apply_dir_add(dir, items, mds, config);
        }
        // 播放列表行每帧只算一次，供滚动可见性校正与渲染共用。
        let rows = app.playlist_rows();
        app.browser.ensure_visible(metrics.browser_visible_h);
        app.ensure_playlist_visible(&rows, metrics.playlist_visible_rows);

        // 渲染界面。
        let dt = last_frame.elapsed();
        last_frame = std::time::Instant::now();
        terminal.draw(|frame| draw(frame, &mut app, config, &rows, dt))?;

        // 拉取 engine 错误 + 自动清除过期（每帧轮询）。
        app.refresh_last_error();

        // 帧率自适应：播放中且频谱可见时缩短 poll 超时（约 30 FPS），
        // 让频谱/进度动画顺滑；否则维持 100 ms，降低空转开销。
        let animating = app.spectrum_mode != SpectrumMode::Hidden
            && app.engine.as_ref().is_some_and(|e| e.is_playing());
        let frame_wait = if animating {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(100)
        };
        // 事件：frame_wait 超时 poll，只处理按下事件。
        if event::poll(frame_wait)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if !app.handle_key(key, config) {
                        break;
                    }
                    // 浏览器位置变化后记入配置，便于下次启动恢复。
                    config.last_dir = Some(app.browser.cwd().to_path_buf());
                }
            }
        }

        // 切曲生效守卫倒计时：Play/PlayResume/Seek 异步生效，守卫期内 position 仍是
        // 旧值，暂停 CUE 终点判定，防"点选更早分轨"被滞后 position 误判连跳。
        if app.switch_guard > 0 {
            app.switch_guard -= 1;
        }

        // 音频引擎报告"播放结束"（EOF）：自动切下一曲。
        let got_finished = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_finished().is_some());
        // 播放失败事件（打开/解码/重采样失败）：强制下一首，单曲循环也不重复失败曲。
        let got_failed = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_failed().is_some());
        // CUE 分轨曲目到达终点（position >= end_ms）：整轨文件未 EOF，
        // 但本曲（INDEX 片段）已播完，同样视为"本曲结束"触发切下一曲。
        // 守卫期内不判定（position 尚未追上切曲后的新值）。
        let cue_finished = app.switch_guard == 0
            && app
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
        if got_finished || cue_finished {
            app.consecutive_failures = 0;
            // ReplayGain：曲目播完，缓存其测量增益（下次播放该曲生效）。
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.replay_gain_cache.insert(path, db);
                }
            }
            let outcome = app.playlist.next(config.repeat);
            match outcome {
                NavOutcome::Switch(_) | NavOutcome::Repeat => {
                    app.handle_nav_outcome(outcome, config);
                }
                NavOutcome::End => {}
            }
        }
        if got_failed {
            app.consecutive_failures += 1;
            if app.consecutive_failures >= 10 {
                // 连续失败达到上限：停止自动跳曲，避免列表全损坏时无限循环刷屏。
                app.flash_message("连续 10 首无法播放，已停止自动切换");
                app.consecutive_failures = 0;
            } else {
                // 播放失败：强制跳下一首（单曲循环按"顺序"语义，不重复失败曲）。
                // 错误提示已由 refresh_last_error 显示（"打开失败/解码错误"）。
                let skip_repeat = if config.repeat == RepeatMode::Single {
                    RepeatMode::Off
                } else {
                    config.repeat
                };
                let outcome = app.playlist.next(skip_repeat);
                if let NavOutcome::Switch(_) = outcome {
                    app.handle_nav_outcome(outcome, config);
                }
            }
        }

        // Gapless 无缝切曲：解码线程已切到预载曲目，只发 track_switched 不发
        // finished。前端只更新 UI 状态（索引/元数据/进度基准），不重发 Play。
        let gapless_switched = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_track_switched().is_some());
        if gapless_switched {
            app.consecutive_failures = 0;
            // ReplayGain：无缝切曲也缓存旧曲测量结果；必须先于
            // advance_ui_on_gapless（那会把 current_path 换成新曲）。
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.replay_gain_cache.insert(path, db);
                }
            }
            app.advance_ui_on_gapless(config);
        }

        // 系统媒体键事件：Linux 经 MPRIS / Windows 经 rdev 到达，映射为播放操作。
        if let Some(handle) = &media_key_handle {
            // 回灌真实播放状态与曲目标题（Linux MPRIS 桌面面板显示用）。
            if let (Some(playing), Some(title)) = (&handle.playing, &handle.title) {
                let is_playing = app.engine.as_ref().is_some_and(|e| e.is_playing());
                playing.store(is_playing, std::sync::atomic::Ordering::Relaxed);
                let title_str = app
                    .current_metadata
                    .as_ref()
                    .and_then(|m| m.title.clone())
                    .unwrap_or_default();
                // 锁中毒安全：即便锁被某线程 panic 污染，也取出守卫正常写入。
                *title.lock().unwrap_or_else(|e| e.into_inner()) = title_str;
            }
            while let Ok(ev) = handle.rx.try_recv() {
                match ev {
                    media_key::MediaKeyEvent::PlayPause => app.toggle_play(),
                    media_key::MediaKeyEvent::Next => app.advance_to_next_track(config),
                    media_key::MediaKeyEvent::Prev => app.advance_to_prev_track(config),
                    media_key::MediaKeyEvent::VolumeUp => app.volume_up(),
                    media_key::MediaKeyEvent::VolumeDown => app.volume_down(),
                }
            }
        }

        // 同步音量到配置（退出时持久化），并记录当前播放位置（接着听）。
        // 仅在真实播放中且位置 > 0.5 秒时写：避免切曲间隙把上一曲的旧位置写
        // 到新曲路径，也避免 0 位置误删已有断点。CUE 分轨不写断点——断点表
        // 以文件路径为键，整轨位置会覆盖该文件普通播放的续播点。
        let current_is_cue = app.current_item_is_cue();
        if let Some(engine) = &app.engine {
            config.volume = engine.volume();
            if engine.is_playing() && !current_is_cue {
                if let Some(path) = &app.current_path {
                    let pos = engine.position();
                    if pos > 0.5 {
                        app.playlist_state.save_position(path, pos);
                    }
                }
            }
        }

        // 每 5 秒自动落盘一次：直接关窗口时最多丢最近 5 秒状态。
        if last_save.elapsed() >= Duration::from_secs(5) {
            app.save_playlist_to_state();
            config::save_playlist_state(&app.playlist_state);
            config::save(config);
            last_save = std::time::Instant::now();
        }
    }
    // 退出前保存播放列表 + 进度，下次启动自动恢复。
    app.save_playlist_to_state();
    config::save_playlist_state(&app.playlist_state);
    Ok(())
}
