# tuneux

# **不羁的青春**® **FreeYouth**®
> 一个基于命令行的音乐播放器。
> 用 Rust 编写，跨平台，界面、提示等信息为中文。

本项目由 tuneterm 进化而来。

## 特性

- 支持 MP3 / FLAC / WAV / OGG / M4A / AAC / ALAC / Opus / WavPack 等格式（核心格式全部纯 Rust 解码，无需系统编解码器）
- **FFmpeg 扩展格式**（如果您自己在电脑上安装ffmpeg）：APE / WMA / FLV / TAK / AC3 / DTS / DSD（DSF/DFF）等长尾格式，自动调用系统 ffmpeg 进程解码；未安装 ffmpeg 则跳过
- **无缝播放**：同格式曲目间无间隙衔接（Gapless，下一曲预载）
- **设备热切换**：播放中拔插耳机 / 切换输出设备自动重连，无需重启程序
- **系统媒体键**：Linux 经 MPRIS（桌面媒体键自动路由）；Windows 用低层键盘钩子捕获媒体键——终端在后台也能控制；macOS 因需 app bundle 暂不支持；可用 `media_keys_enabled = false` 关闭
- **ReplayGain 响度归一**：按 −14 LUFS 目标统一各专辑音量（默认开启，下次播放生效）
- **自定义键位**：`tuneux.toml` 的 `keymap` 可重映射播放控制键
- **CUE 分轨**：整轨 + 同名 `.cue` 自动分轨播放
- **歌词**：优先同目录 `.lrc`，无则读取歌曲内嵌歌词标签
- 直接运行 `tuneux` 进入交互式 TUI，无子命令
- TUI 内置文件浏览器：浏览目录、播放文件、加入播放列表；`/` 递归搜索子目录
- 播放控制：单曲循环、列表循环、随机、音量、5 秒快进快退、断点续播
- 播放列表：加入目录后按"专辑-曲序"自动排序展示与播放
- 专辑封面：`c` 键在左侧面板显示内嵌封面
- 元数据：标题、艺术家、专辑、曲序、编码格式、码率、采样率、位深
- 实时频谱可视化（`v` 键：关 → 半屏 → 全屏 Matrix 风格）
- `?` 键"关于"弹窗：版本号、简介、开源声明、版权
- Windows / Linux / macOS 功能一致（macOS 暂缺系统媒体键），配置可便携

## 支持的音频格式

tuneux 的解码采用「纯 Rust 为主 + FFmpeg 进程外补位」的双层架构：MP3 / FLAC / WAV / OGG / M4A / AAC / ALAC / Opus / WavPack 等核心格式由纯 Rust 解码库（[symphonia](https://github.com/pdeljanov/Symphonia) + Opus + WavPack）进程内解码，**无需任何系统编解码器**；APE / WMA / DSD 等长尾格式则调用系统 ffmpeg 进程解码（未安装时自动跳过）。核心格式如下：

| 格式 | 扩展名 | 编码类型 | 码率范围 | 采样率 | 位深 | 是否无损 |
|------|--------|---------|---------|--------|------|---------|
| MP3 | `.mp3` | MPEG-1/2/2.5 Layer III | 8 – 320 kbps（CBR/VBR） | 8 / 11.025 / 12 / 16 / 22.05 / 24 / 32 / 44.1 / 48 kHz | 16-bit（解码输出） | 否 |
| FLAC | `.flac` | FLAC | 800 – 1500 kbps（取决于内容） | 最高 655 kHz，常见 44.1 / 48 / 96 / 192 / 384 kHz | 4 – 32 bit，常见 16 / 24 | 是 |
| WAV | `.wav` | PCM | 取决于采样率×位深×声道 | 任意（常见 8 – 384 kHz） | 8 / 16 / 24 / 32 bit | 是 |
| OGG | `.ogg` | Vorbis | 45 – 500 kbps（VBR） | 8 – 192 kHz，常见 44.1 / 48 kHz | 16 / 24 / 32 bit（解码输出） | 否 |
| M4A / AAC | `.m4a` `.aac` | AAC-LC | 8 – 576 kbps | 8 – 96 kHz | 16 / 24 / 32 bit（解码输出） | 否 |
| ALAC | `.m4a` `.alac` | Apple Lossless | 取决于内容 | 8 – 384 kHz | 16 / 24 bit | 是 |
| Opus | `.opus` | Opus（Ogg 容器） | 6 – 510 kbps（可变） | 8 / 12 / 16 / 24 / 48 kHz（输入任意） | 16-bit（解码输出） | 否 |
| WavPack | `.wv` | WavPack（无损，含浮点） | 取决于内容 | 任意 | 16 / 24 / 32 bit + 浮点 | 是 |

说明：
- **码率**：有损格式（MP3/OGG/AAC）的码率由编码决定，播放器按原始码率解码；无损格式（FLAC/WAV/ALAC）码率 = 采样率 × 位深 × 声道数，随内容变化。
- **位深**：有损格式在文件中无固定"位深"概念，此处为解码后 PCM 输出的位深，统一以 32-bit 浮点送入音频设备，由设备完成数模转换。
- **采样率/位深不匹配**：当文件采样率与音频输出设备不一致时，tuneux 自动使用高质量重采样（rubato）避免变调变速。
- **无缝播放（Gapless）**：同采样率曲目间预载下一曲、无间隙衔接；采样率变化时自动降级（短间隙）。
- **CUE 分轨**：`.cue` 支持 UTF-8 / UTF-16 / GB18030 编码；分轨曲目快进快退限制在本曲区间内。
- **DSD（可选，经 ffmpeg）**：DSF / DFF（SACD 使用的 1-bit Delta-Sigma 调制格式）可经系统 ffmpeg 解码（ffmpeg 自动完成 DSD→PCM 转换后以 32-bit 浮点送入音频设备）；未安装 ffmpeg 时提示安装（见「关于 FFmpeg」）。DSD 原生位流不直通输出，统一转为 PCM 播放。

### 单个文件大小限制

- **软件层面无硬性大小上限**：tuneux 采用流式解码，同一时刻仅缓冲数秒音频数据在内存中，不会将整个文件读入内存。因此可播放大小仅受**可用内存**和**文件系统**限制。
- **WAV 格式**：受 RIFF 容器规范约束，标准 WAV 单文件上限约 **4 GB**（RF64 扩展格式可更大）。
- **FLAC / ALAC / MP3 / OGG / AAC**：格式本身无 4 GB 限制，实际可播放数 GB 乃至更大的高清无损文件。
- **64 位系统**下实测可正常播放数 GB 的 24-bit/192 kHz 无损文件；32 位进程受地址空间限制，建议单文件不超过约 2 GB。

## 关于 FFmpeg（可选扩展格式）

**核心格式不需要它**：MP3 / FLAC / WAV / OGG / M4A / AAC / ALAC / Opus / WavPack 全部由纯 Rust 解码库进程内解码，tuneux 自带、零依赖，不装任何东西都能播。

**FFmpeg 只服务"长尾"格式**：APE / WMA / FLV / TAK / AC3 / DTS / DSD（DSF/DFF）等较少见的格式，tuneux 检测到系统里有 `ffmpeg` 命令就自动调用它解码；没有则优雅跳过（打开这类文件时会提示），**不需要任何配置**。

### 为什么让用户自己安装，而不是 tuneux 替你装好？

- **保持"纯 Rust、纯离线"定位**：核心解码完全自包含、无系统依赖、无联网安装步骤；捆绑 ffmpeg 会把几十 MB 的外部二进制塞进安装包，破坏单文件便携与离线特性。
- **体积与维护成本**：ffmpeg 本体 + 依赖动辄几十 MB，且需随上游持续更新安全补丁；捆绑会显著增大每个平台的下载体积和发布负担。
- **许可证复杂度**：ffmpeg 采用 GPL / LGPL 双轨授权（取决于编译选项），不同发行版打包的 ffmpeg 许可状态不同；tuneux 本身是木兰宽松许可证 v2，捆绑分发会引入额外的许可证兼容负担与法律风险。
- **平台习惯不同**：Windows / macOS / Linux 各有成熟的安装渠道（包管理器、Homebrew 等），让用户按平台习惯安装最省事、最安全。

### 三个系统的安装方法

| 系统 | 安装命令 / 方法 | 验证 |
|------|----------------|------|
| Windows | `winget install ffmpeg`，或到 [ffmpeg.org](https://ffmpeg.org/download.html) 下载解压后把 `bin` 目录加入 PATH | 新开终端执行 `ffmpeg -version` |
| macOS | `brew install ffmpeg` | 终端执行 `ffmpeg -version` |
| Linux（Debian/Ubuntu） | `sudo apt install ffmpeg` | 终端执行 `ffmpeg -version` |
| Linux（Fedora） | `sudo dnf install ffmpeg` | 终端执行 `ffmpeg -version` |
| Linux（Arch） | `sudo pacman -S ffmpeg` | 终端执行 `ffmpeg -version` |

安装完成后**无需任何配置**：tuneux 每次打开扩展格式文件时自动探测 `ffmpeg`，找到即用。

### 注意事项与风险

- **只装扩展格式才需要 ffmpeg**；不装也不影响核心格式的播放、Gapless、CUE 等全部功能。
- **请从官方渠道安装**（系统包管理器 / ffmpeg.org / Homebrew），不要下载来路不明的"绿色版"，以免捆绑恶意软件。
- **PATH 生效问题**：Windows 安装后如仍提示"ffmpeg 未安装或不在 PATH 中"，需**重开终端**，或手动把 ffmpeg 的 `bin` 目录加入系统 PATH；macOS 用 Homebrew 时请确认 `/opt/homebrew/bin`（Apple Silicon）或 `/usr/local/bin`（Intel）在 PATH 中。
- **版本建议**：推荐较新的 ffmpeg（5.x 及以上）；过旧版本可能缺少个别格式的解码器。
- **解码质量**：无论是否使用 ffmpeg，tuneux 统一把解码结果转换为 32 位浮点 PCM 送入音频设备，不引入额外质量损失。

## 快捷键

| 按键 | 作用 |
|------|------|
| `↑` / `↓`（或 `k` / `j`） | 浏览列表（文件浏览器或播放列表，取决于当前焦点） |
| `Home` / `End` | 列表跳到顶部 / 底部 |
| `Enter` | 文件浏览器：进入目录 / 播放文件；播放列表：播放选中曲 |
| `Backspace` | 文件浏览器：返回上级目录；播放列表：回到浏览器 |
| `a` | 加入播放列表（文件或整个目录，目录内容按专辑-曲序排序） |
| `d` | 删除选中的曲目 |
| `x` | 清空播放列表（按两次确认） |
| `b` | 显示 / 隐藏左侧文件浏览器 |
| `c` | 显示 / 隐藏左侧专辑封面（占用文件浏览器位置） |
| `/` | 搜索：浏览器递归搜索子目录；播放列表按曲名/歌手/专辑/路径；`Esc` 退出 |
| `Tab` | 切换面板焦点（文件浏览器 ↔ 播放列表） |
| `空格` | 播放 / 暂停 |
| `←` / `→` | 快退 / 快进 5 秒 |
| `+` / `-` | 音量增减 |
| `n` / `p` | 下一曲 / 上一曲 |
| `r` | 切换循环模式（关 → 单曲 → 列表） |
| `s` | 切换随机播放 |
| `g` | 切换播放列表视图（平铺 ↔ 按专辑分组，分组可折叠/展开） |
| `v` | 切换频谱（关 → 半屏 → 全屏） |
| `l` | 显示/隐藏歌词（播放列表右侧面板，同目录同名 .lrc 自动加载） |
| `?` | 显示"关于"（版本号、简介、开源声明、版权），任意键关闭 |
| `q` | 退出（自动保存配置） |
| `Ctrl+C` | 退出 |

## 配置

正常退出时自动保存音量、循环、随机、列宽、上次浏览目录、每首曲目的播放进度（断点续播）等设置，下次启动自动恢复。

- **自定义键位（keymap）**：在 `tuneux.toml` 中可重映射播放控制键。动作名：`toggle_play`（播放/暂停）、`next`（下一曲）、`prev`（上一曲）、`volume_up`（音量+）、`volume_down`（音量−）。键描述语法：`[shift+][ctrl+][alt+]<键名>`，键名为单字符或 `space` / `tab` / `enter` / `esc` / `backspace` / `up` / `down` / `left` / `right` / `home` / `end`。示例：
  ```toml
  [keymap]
  toggle_play = "ctrl+p"
  next = "ctrl+n"
  prev = "ctrl+b"
  volume_up = "shift+="
  ```
  非法键描述会被忽略并回退默认键，不会导致启动失败。

- **默认**：配置存放在 `tuneux`（或 `tuneux.exe`）**同目录的 `tuneux.toml`**，适合 U 盘随身带——单文件就是一个完整的播放器（Windows / macOS）。
  > 注：Linux 的 U 盘默认以 `noexec` 挂载、无法直接运行程序，需先复制到本地磁盘再运行，此时配置自动保存到本地或系统目录。
- **回退**：如果 exe 同目录不可写（只读 U 盘、装到系统位置 `/usr/local/bin` 等），配置自动改存到系统目录：
  - Windows：`%APPDATA%\tuneux\tuneux.toml`
  - macOS：`~/Library/Application Support/tuneux/tuneux.toml`
  - Linux：`~/.config/tuneux/tuneux.toml`

## 系统要求

| 平台 | 最低系统版本 | CPU 架构 | 系统依赖 |
|------|------------|---------|---------|
| Windows | Windows 10 | x86_64、aarch64 | 无 |
| macOS | macOS 10.13（High Sierra）及以上 | x86_64（Intel）、aarch64（M 系列） | 无 |
| Linux | 主流发行版（内核自带 ALSA 即可） | x86_64、aarch64（另支持 armv7、riscv64 等） | libasound2（多数发行版预装）；可选 ffmpeg（播放 APE/WMA 等扩展格式）；桌面环境（媒体键经 MPRIS，GNOME/KDE 均可） |

最低 Rust 工具链版本：**1.88+**（ratatui 0.30 要求）

## 构建

```bash
# 调试构建
cargo build

# 发布构建（启用 LTO、去除符号，体积更小）
cargo build --release

# 运行
cargo run --release
```

编译产物位于 `target/release/tuneux`（或 `tuneux.exe`），可单独复制到任意目录运行。

## 下载与首次运行


| 系统 | CPU 架构 | 文件（以 v0.4.4 为例） |
|------|---------|----------------------|
| Windows | x86_64（多数 PC） | `tuneux-v0.4.4-windows-x86_64.zip` |
| Windows | arm64 | `tuneux-v0.4.4-windows-arm64.zip` |
| macOS | Apple Silicon（M 系列） | `tuneux-v0.4.4-macos-arm64.tar.gz` |
| macOS | Intel | `tuneux-v0.4.4-macos-x86_64.tar.gz` |
| Linux | x86_64 | `tuneux-v0.4.4-linux-x86_64.tar.gz` |
| Linux | arm64 | `tuneux-v0.4.4-linux-arm64.tar.gz` |

> 预编译二进制未做代码签名，首次运行会被系统拦截，按下面步骤放行即可。
> 若不想处理提示，用源码自行编译（`cargo build --release`）则不会有任何拦截。

### Windows 首次运行

1. 解压 zip，双击 `tuneux.exe`
2. 出现蓝色「Windows 已保护你的电脑」→ 点「更多信息」
3. 点「仍要运行」

### macOS 首次运行

在「终端」里执行：

```bash
# 1. 进入下载目录（假设解压后的 tuneux 在 ~/Downloads）
cd ~/Downloads

# 2. 解压
tar xzf tuneux-v0.4.4-macos-arm64.tar.gz

# 3. 清除隔离属性（关键一步）
xattr -cr tuneux

# 4. 运行
./tuneux
```

`xattr -cr` 会清除文件上所有隔离标记，之后就能正常打开了（双击或 `./tuneux` 都行）。

### Linux 首次运行

```bash
tar xzf tuneux-v0.4.4-linux-x86_64.tar.gz
chmod +x tuneux
./tuneux
```

## 项目代码约定

1. 许可证：木兰宽松许可证 v2（MulanPSL-2.0），新增依赖须与其兼容
2. 安全：纯离线，禁止引入任何联网依赖（reqwest/http 等）
3. 代码：简洁优先，所有公开项与复杂逻辑必须写详细中文注释

## 隐私与安全

本软件**完全离线**，不含任何联网功能（播放NAS上的音乐需要操作系统将网盘映射到本地），**本软件不收集任何数据**。

## 许可

版权所有 © 2026 不羁的青春（FreeYouth）。

本项目采用 [木兰宽松许可证，第 2 版](LICENSE)（MulanPSL-2.0）开源。

### 第三方组件声明

本项目使用以下开源库，在此致谢：

| 库 | 许可证 | 用途 |
|----|--------|------|
| cpal | MIT / Apache-2.0 | 跨平台音频输出 |
| symphonia | MPL-2.0 | 音频解码（MP3/FLAC/WAV/OGG/M4A/AAC/ALAC） |
| opus-decoder | MIT / Apache-2.0 | Opus 解码 |
| wavicle | MIT / Apache-2.0 | WavPack 解码 |
| ratatui | MIT | 终端界面框架 |
| crossterm | MIT | 终端后端 |
| rustfft | MIT / Apache-2.0 | FFT 频谱分析 |
| rubato | MIT / Apache-2.0 | 采样率重采样 |
| ringbuf | MIT / Apache-2.0 | 无锁环形缓冲 |
| crossbeam-channel | MIT / Apache-2.0 | 多线程通道 |
| serde | MIT / Apache-2.0 | 序列化 |
| toml | MIT / Apache-2.0 | TOML 配置解析 |
| dirs | MIT / Apache-2.0 | 系统目录定位 |
| encoding_rs | Apache-2.0 / MIT / BSD-3-Clause | 歌词编码识别（UTF-8 / UTF-16 / GBK） |
| zbus | MIT | Linux 系统媒体键 MPRIS D-Bus 服务 |
| rdev | MIT | Windows 系统媒体键低层键盘钩子 |
| image | MIT / Apache-2.0 | 专辑封面解码（JPEG / PNG） |
| unicode-width | MIT / Apache-2.0 | 终端字符宽度计算（对齐布局） |

其中 symphonia 采用 MPL-2.0（Mozilla Public License 2.0），属文件级弱 copyleft 许可——仅对 symphonia 库本身的修改需以 MPL-2.0 开源，不影响以木兰宽松许可证 v2 分发本项目整体。MPL-2.0 与木兰宽松许可证 v2 兼容。
