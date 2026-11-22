//! # tuneux 程序入口
//!
//! tuneux 是一个跨平台命令行音乐播放器。
//!
//! 本文件负责：
//! 1. 初始化终端（进入 raw 模式、切换备用屏幕）；
//! 2. 加载配置并启动 TUI 主事件循环；
//! 3. 退出时恢复终端状态并保存配置。

// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]
// 项目内部模块
mod config;
mod fs_browser;
mod media_key;
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

use crate::config::{Config, RepeatMode};
use crate::tui::app::App;
use crate::tui::render::{draw, layout_metrics};

/// 应用程序错误类型。
///
/// 错误聚合为 trait 对象，避免为每种底层错误单独定义类型，
/// 简化代码。配置/音频/IO 错误对用户均不致命，调用方据此回退。
type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

/// 从媒体键通道读取一轮事件，再映射为基础版 keymap 动作名。
/// 有界读取与同类合并由 commonx 统一实现。
fn drain_media_key_events(
    rx: &crossbeam_channel::Receiver<tuneux_commonx::media_key::MediaKeyEvent>,
    limit: usize,
) -> Vec<&'static str> {
    tuneux_commonx::media_key::drain_media_key_events(rx, limit)
        .into_iter()
        .map(|ev| ev.action())
        .collect()
}

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
    let terminal = (|| {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        Ok(Terminal::new(backend)?)
    })();
    if terminal.is_err() {
        let _ = restore_terminal();
    }
    terminal
}

/// 恢复终端到正常状态。
///
/// 必须在程序退出前调用，否则终端会停留在 raw 模式，
/// 用户看不到输入也无法正常使用 shell。
fn restore_terminal() -> AppResult<()> {
    let raw_result = disable_raw_mode();
    let screen_result = execute!(io::stdout(), LeaveAlternateScreen);
    raw_result?;
    screen_result?;
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
    // 上一帧时刻：用于计算帧间隔，驱动频谱峰值按秒衰减（帧率无关）。
    let mut last_frame = std::time::Instant::now();
    let mut scan_added = 0usize;

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
        // 目录异步载入结果：到达后应用到浏览器（后台线程读大目录不卡 UI；
        // 放在 ensure_visible/draw 之前，导航结果未应用前不参与本帧渲染）。
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
                    Err(e) => {
                        app.flash_message(&e);
                    }
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
        // 多批次累积加入；显示排序由 Playlist 的 display_order 负责，
        // 不重排内部条目，以免 current/history 中的索引失效。
        {
            let mut drained = 0;
            while drained < 64 {
                match app.scan_pool.try_recv() {
                    Some(tuneux_mediax::scan_pool::ScanEvent::Items(batch)) => {
                        let was_idle = app.current_path.is_none();
                        drained += batch.len();
                        scan_added += if config.dedup_on_add {
                            app.playlist.add_many_dedup(batch)
                        } else {
                            let count = batch.len();
                            app.playlist.add_many(batch);
                            count
                        };
                        if was_idle && app.playlist.current_index().is_none() {
                            app.playlist.jump_to(0);
                            app.handle_nav_outcome(playlist::NavOutcome::Switch(0), config, false);
                        }
                    }
                    Some(tuneux_mediax::scan_pool::ScanEvent::CueError { path, message }) => {
                        drained += 1;
                        app.flash_message(&format!("{}：{message}", path.display()));
                    }
                    None => break,
                }
            }
            if app.scan_pool.is_idle() && scan_added > 0 {
                app.flash_message(
                    &app.i18n
                        .t("msg.added")
                        .replace("{}", &scan_added.to_string()),
                );
                scan_added = 0;
            }
        }
        // 恢复期元数据后台回填：逐帧有界合入（与扫描池同款节流），只补缺失项
        // ——启动恢复不再逐首同步 from_file（内含封面字节读取）阻塞数十秒。
        app.drain_restore_metadata(64);
        // 播放列表行每帧只算一次，供滚动可见性校正与渲染共用（与 fx 同源）。
        let rows = app.playlist_rows();
        app.browser.ensure_visible(metrics.browser_visible_h);
        app.ensure_playlist_visible(&rows, metrics.playlist_visible_rows);

        // 渲染界面（draw 需要 &mut app：电平乱码每帧推进 rng 状态）
        let dt = last_frame.elapsed();
        last_frame = std::time::Instant::now();
        terminal.draw(|frame| draw(frame, &mut app, config, &rows, dt))?;

        // 拉取 engine 错误 + 自动清除过期——每帧都做（100ms 一次轮询）
        app.refresh_last_error();

        // 帧率自适应：播放中且频谱可见时缩短 poll 超时（约 30 FPS），
        // 让频谱动画（含峰值保持白帽）顺滑；否则维持 100 ms，降低空转开销。
        let animating = app.spectrum_mode != crate::config::SpectrumMode::Hidden
            && app.engine.as_ref().is_some_and(|e| e.is_playing());
        let frame_wait = if animating {
            std::time::Duration::from_millis(33)
        } else {
            std::time::Duration::from_millis(100)
        };
        // 事件等待：用 poll（frame_wait 超时）而非阻塞 read，
        // 这样每帧能醒来检查音频引擎的 EOF 事件（用于自动下一曲等），
        // 同时也让进度条等动态信息能周期性刷新。
        if event::poll(frame_wait)? {
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
        // 播放失败事件（打开/解码/重采样失败）：强制下一首，单曲循环也不重复失败曲。
        let got_failed = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_failed().is_some());
        // Gapless 无缝切曲事件：解码线程已无缝切换到预载曲目，
        // 前端只更新 UI 状态（不重发 Play，避免打断无缝衔接）。
        let gapless_switched = app
            .engine
            .as_ref()
            .is_some_and(|e| e.poll_track_switched().is_some());
        if gapless_switched {
            app.consecutive_failures = 0;
            // ReplayGain：无缝切曲也保存旧曲分析结果（引擎在切换时已写入）；
            // 经 cache_replay_gain 走 LRU 上限，防止增益表无界累积。
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.cache_replay_gain(path, db);
                }
            }
            app.advance_ui_on_gapless(config);
        }

        // ReplayGain：曲目分析完成，把整曲增益缓存（下次播放该曲生效）
        if got_finished {
            app.consecutive_failures = 0;
            if let Some(db) = app.engine.as_ref().and_then(|e| e.take_measured_gain_db()) {
                if let Some(path) = app.current_path.clone() {
                    app.cache_replay_gain(path, db);
                }
            }
        }
        if got_finished {
            let outcome = app.playlist.next(config.repeat);
            match outcome {
                playlist::NavOutcome::Switch(_) | playlist::NavOutcome::Repeat => {
                    app.handle_nav_outcome(outcome, config, false);
                }
                playlist::NavOutcome::End => {
                    // 列表到尽头且关闭循环：什么都不做（流已自动暂停）
                }
            }
        }
        if got_failed {
            // 三端共用同一份失败决议（mediax），语义见其函数文档：
            // 未达上限继续自动跳曲；达到上限计数饱和、停止自动跳曲。
            let (count, keep_skipping) =
                tuneux_mediax::advance_failure_count(app.consecutive_failures);
            app.consecutive_failures = count;
            if keep_skipping {
                // 播放失败：强制跳下一首（单曲循环按"顺序"语义，不重复失败曲）。
                // 错误提示已由 refresh_last_error 显示（"打开失败/解码错误"）。
                let skip_repeat = if config.repeat == RepeatMode::Single {
                    RepeatMode::Off
                } else {
                    config.repeat
                };
                let outcome = app.playlist.next(skip_repeat);
                if let playlist::NavOutcome::Switch(_) = outcome {
                    app.handle_nav_outcome(outcome, config, false);
                }
            } else {
                // 连续失败达到上限：真正停止自动跳曲，避免列表全损坏时
                // 逐首尝试刷屏。计数保持在上限不清零——用户手动发起点播
                // （play_and_update_current 的手动路径）才清零恢复。
                let msg = app.i18n.t("msg.consecutive_fail").into_owned();
                app.flash_message(&msg);
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
                *title.lock().unwrap_or_else(|e| e.into_inner()) = title_str;
            }
            // 洪泛防护：每轮至多消费 MEDIA_KEY_DRAIN_LIMIT 条，同类事件合并
            //（连发音量加只执行一次），见 drain_media_key_events。
            for action in
                drain_media_key_events(&handle.rx, tuneux_commonx::media_key::MEDIA_KEY_DRAIN_LIMIT)
            {
                let _ = app.execute_action(action, config);
            }
        }

        // 同步音量到配置（退出时持久化），并记录当前播放位置（接着听）。
        // 仅在真实播放中且位置 > 0.5 秒时写：避免切曲间隙把上一曲旧位置写
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

        // 每 5 秒自动落盘一次：直接关窗口（强杀）时，
        // 最多只丢最近 5 秒的状态，而非全部丢失。
        if last_save.elapsed() >= std::time::Duration::from_secs(5) {
            app.save_playlist_to_state();
            config::save_playlist_state(&app.playlist_state);
            config::save(config);
            last_save = std::time::Instant::now();
        }
    }
    // 退出前保存播放列表 + 进度到独立文件 tuneux-playlist.toml，下次启动自动恢复。
    // 先裁剪失效路径（断点与 ReplayGain 表按文件存在性清理，防删除文件后
    // 跨会话无界累积）；启动侧 App::new 同样裁剪一次，双向兜底强杀场景。
    app.playlist_state.prune_missing_files();
    app.save_playlist_to_state();
    config::save_playlist_state(&app.playlist_state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 媒体键排空：单轮消费有界（洪泛不拖垮主循环）+ 同类事件合并
    /// （每种动作只执行一次），剩余事件保留在通道供后续轮次消费。
    #[test]
    fn media_key_drain_caps_and_merges() {
        use tuneux_commonx::media_key::MediaKeyEvent;
        let (tx, rx) = crossbeam_channel::bounded(64);
        // 交错投递事件：首轮 16 条内多类事件都出现，验证合并而非漏消费。
        for _ in 0..20 {
            tx.send(MediaKeyEvent::Next).unwrap();
            tx.send(MediaKeyEvent::VolumeUp).unwrap();
        }
        tx.send(MediaKeyEvent::Prev).unwrap();
        let actions = drain_media_key_events(&rx, tuneux_commonx::media_key::MEDIA_KEY_DRAIN_LIMIT);
        assert_eq!(
            actions,
            vec!["next", "volume_up"],
            "同类事件应合并为一次执行"
        );
        // 单轮只消费 16 条：41 - 16 = 25 条保留在通道，供后续轮次继续消费。
        let mut remaining = 0;
        while rx.try_recv().is_ok() {
            remaining += 1;
        }
        assert_eq!(remaining, 25, "超出单轮上限的事件应保留到下一轮");
    }
}
