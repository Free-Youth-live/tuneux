# 变更日志（tuneux-corex）

本文件记录 tuneux-corex 各版本的变更。格式参考 Keep a Changelog，版本号遵循语义化版本。corex 为契约化一等 crate，与产品线版本解耦、独立版本线。


## [0.3.0] - 2026-09-03

### 新增

- **均衡器模块**：`EqParams` + `EQ_BANDS` / `EQ_FREQS` / `EQ_GAIN_MIN_DB` / `EQ_GAIN_MAX_DB` / `EQ_SLOTS`——10 段 peaking 均衡器（RBJ biquad，L/R 独立），音频线程执行、零锁零分配，供插件控制面写参数。
- **压缩器模块**：`CompressorParams` + `COMP_SLOTS`——峰值检测 + 一阶包络跟随 + 增益衰减 + 补偿增益，音频线程执行。
- **动态多槽位**：`Engine::{eq_slots, compressor_slots, alloc_eq_slot, alloc_compressor_slot, free_eq_slot, free_compressor_slot, set_eq_band, set_eq_enabled}`——均衡器 / 压缩器各 8 槽，支持同类效果器叠加，插件按槽位寻址写参数。
- **播放介质九档**：`PlaybackMedium` / `UnknownMedium` + `Engine::set_medium`——介质改为纯声音修饰（磁带 4 档 + 黑胶 4 档 + 关闭），无画面。
- **ReplayGain 开关**：`Engine::set_replay_gain_enabled`（默认关；关时回调恒用 1.0 增益、解码线程不建响度分析器省 CPU）。

### 修复

- Gapless 负进度基准改用流的真实采样率（原生直通重建流后尾巴时长换算不再偏约 8%）。
- `EqParams::band` 越界读返回 0.0（与 `set_band` 的越界防护对称，杜绝 panic）。

## [0.2.0] - 2026-08-30

### 新增

- `KNOWN_AUDIO_EXTS`：支持的音频扩展名白名单（原生 + FFmpeg 长尾），公开导出供产品侧过滤。
- `Engine::poll_failed`：播放失败事件通道（打开/解码/重采样失败），供产品侧强跳下一首、避免重复失败曲。
- `probe_metadata` / `ProbeTags`：音频文件探测（读技术参数 + 时长 + 原始标签，不解码），供媒体层组装元数据；文件探测从 mediax 收口回 corex。封面提取改为按用途三级优先（FrontCover → BackCover → 任意非空），不再取第一个非空 visual，属行为变化。
- `SpectrumPeakHold`：频段维度峰值保持状态机（按秒衰减、帧率无关），供产品侧频谱峰值白帽复用；输入逐频段消毒、构造期校验、显式 `reset`。
- `DEFAULT_PEAK_FALL_PER_SEC` 常量（0.6，满格约 1.7 秒落到 0）。

### 修复

- 新曲目无时长元数据（Opus / FFmpeg 后端恒 None）时未清时长、残留上一曲总时长 → 改为清零。
- gapless 无缝切换仅比对采样率、未比对声道数 → 加声道比对，不一致回退 draining。
- ffmpeg stderr 缓存与 Ogg 页头解析的生产路径 unwrap/expect 改为优雅降级。

## [0.1.0] - 初始版本

- 可插拔解码后端（symphonia / Opus / WavPack / FFmpeg 桥）、三线程引擎、无缝切曲状态机、频谱分析（`spectrum_lr`）、ReplayGain 测量与应用。