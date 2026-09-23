# 变更日志（tuneux-corex）

本文件记录 tuneux-corex 各版本的变更。格式参考 Keep a Changelog，版本号遵循语义化版本。corex 为契约化一等 crate，与产品线版本解耦、独立版本线。


## [0.3.1] - 2026-09-23

### 新增

- **自研解码三阶段（无损优先）**：`.wav`（`wav.rs`）、`.flac`（`flac.rs`）解码路径接管为纯 Rust 手写后端（流式、零新增依赖；不支持的形态自动回退 symphonia，存量文件零退化）；`.wv`（WavPack）解码改为自研实现（第三方 wavicle 退出生产依赖，仅留测试对拍参照物）。
- **区间播放 `AudioCmd::PlayRange`**：只播放文件中的 [start_secs, end_secs) 区间——终点按源文件时间轴在重采样前累计判定、尾巴播完再上报结束；时长与进度按区间上报（cue 分轨 / 光盘镜像单曲目 / 外部消费场景）。
- **`PreloadTarget` 预载目标结构化**：区间预载（后端先 seek 到起点再缓存，同文件相邻区间无缝衔接）；seek 失败即弃预载，不从文件头误播。
- **CD-DA 镜像解码后端（`cdda.rs`）**：`.bin` 光盘镜像当作可 seek 的原始 PCM 文件流式解码（44.1kHz/16bit/2ch）；2352/2448 扇区探测（DAO-96 子通道剥离）；帧级游标记账（块边界非扇区对齐时不丢帧）；构造泛型 `from_reader` 支持内存源（`Cursor<Vec<u8>>`）；`probe_sample_rate` 直通判定；`.bin` 不进 `KNOWN_AUDIO_EXTS`（入口由 .cue 引导）。
- **CUE 解析扩展（`cue.rs`）**：`CueSheet`（专辑级 TITLE/PERFORMER/FILE 引用 + `parse_cue_sheet`）；`CueTrack` 新增 INDEX 00 间隙起点、终点、轨迹类型（数据轨标记不可播）、起始扇区换算；`decode_cue_bytes` 编码链（UTF-8 → BOM → GB18030）；原 `parse_cue` 签名不变。
- `probe_metadata` 支持 `.bin` 镜像（时长精确、无标签）。

### 破坏性变更

- `AudioCmd::PreloadNext` 参数由 `Option<PathBuf>` 改为 `Option<PreloadTarget>`（corex 未到 1.0，随 0.3.1 标注；版本口径按产品线「只增一档」约定）。

### 修复与加固

- **FLAC 解码还原 wasted bits**：子帧声明 wasted 时样本最低 k 位须左移还原，旧实现只扣有效位深不左移（16bit 母带封装进 24bit 容器等**合法文件整体衰减 2^k 倍**，帧 CRC 覆盖原始字节故无法发现）；补位流级回归测试。
- **均衡器 Nyquist 保护**：`f0 ≥ sr/2` 时 peaking 系数的 `alpha < 0` → 极点出单位圆 → 输出发散为 Inf/NaN，而末级 `clamp` 不拦 NaN；越界段返回单位系数（直通），`process` 出口再加非有限值兜底。
- **压缩器出口兜底**：参数被绕过 setter 直接污染时 `powf` 会放大出 Inf/NaN，出口回退为输入样本（直通）。
- **ReplayGain 测量**：seek 后不再产出残缺测量值（跳过的中段不参与统计；残缺值会被上层按路径缓存并跨曲次回灌）；`set_measured_gain_db` 的钳制下界改用 `i32::MIN + 1`，把「未测量」哨兵排除出合法值域。
- **设备热切换（`audio_loop.rs`）**：暂停态改发 `LoadAndSeek` 保留进度（原先无视播放态一律从头加载）；进度按**旧**流采样率换算（原先把旧率的帧数按新率折算，44.1k→48k 播 3 分钟偏小约 15 秒）；重建流的通道数保持启动时取值（解码线程的 `adapt_channels` / `push_all` / 重采样器构建均基于该值，跟随新设备会产生变速变调）。完整修复（在 `SharedState` 同步流通道数）留待后续版本。
- **区间播放 seek**：`range_base` 与响度分析器的重置改为**仅在 seek 成功时**执行（失败即位置未变，提前重置会让区间终点按错误基准累计）。
- **解码器健壮性**：WavPack 尾部标签宽容（非 `wvpk` 魔数视为流结束）；CD-DA 扇区尺寸互素歧义加同步模式二次确认（扇区数为 49 的倍数时 2448 曾被误判为 2352）；CUE `start_sector` 改整数运算（消浮点截断：原先 5.34% 的 MM:SS:FF 少一扇区）；FLAC seek 偏移越界回退从头线性解码；WAV `fmt` chunk 与 WavPack 块声明长度上限、WavPack 轻量探测块数上限（防无界分配与探测挂起）。
- **`ringbuf` 升级 0.5.2（供应链）**：消除 RUSTSEC-2026-0293——`Consumer::skip` / `Consumer::clear` 先就地析构元素、后推进读索引，若元素 `Drop` panic 则索引停在已析构槽位，缓冲区析构时二次析构同一批元素（双重释放 / use-after-free）。本仓缓冲元素为 `f32`：无 `Drop` 实现、析构不可能 panic，该前提不可满足故原不可达；但换曲冲刷路径确实调用 `Consumer::clear()`，随本次一并升级。所用 API 面 0.4 → 0.5 兼容，业务代码零改动。

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

- 可插拔解码后端（symphonia / Opus / WavPack / FFmpeg 桥）、四线程引擎、无缝切曲状态机、频谱分析（`spectrum_lr`）、ReplayGain 测量与应用。