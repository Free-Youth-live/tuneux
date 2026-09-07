============================================================
tuneux 使用说明
============================================================

一、简介

tuneux 是一个基于命令行的音乐播放器
使用 Rust 编写，纯离线运行，不收集任何数据。
支持 MP3 / FLAC / WAV / OGG / M4A / AAC / ALAC /
Opus / WavPack 等格式（纯 Rust 解码，无需系统编解码器）；
整轨 + .cue 自动分轨；同格式曲目间无缝衔接（Gapless）；
设备热切换（拔插耳机自动重连）；ReplayGain 响度归一；
系统媒体键（Linux 经 MPRIS、Windows 经低层键盘钩子控制播放）
（各专辑音量统一）；APE / WMA / DSD 等扩展格式可调用系统
ffmpeg 解码（未安装 ffmpeg 时自动跳过）。

二、快速开始

1. 解压下载的压缩包（zip / tar.gz）
2. 运行程序：
   - Windows：双击 tuneux.exe
   - macOS：先看下方「macOS 注意事项」，然后运行 tuneux
   - Linux：先执行 chmod +x tuneux，再 ./tuneux
   - 如果无法运行，请看下方「四、三个系统的注意事项」

三、快捷键

【列表浏览】
  ↑ / ↓（或 k / j）  浏览列表（浏览器或播放列表）
  Home / End         跳到列表顶部 / 底部
  Enter              浏览器：进入目录 / 播放文件
                     播放列表：播放选中曲 / 折叠展开专辑组
  Backspace          浏览器：返回上级目录
                     播放列表：切回浏览器

【面板与视图】
  Tab                切换焦点（浏览器 ↔ 播放列表）
  b                  显示 / 隐藏文件浏览器
  c                  显示 / 隐藏专辑封面
  v                  切换频谱（关 → 半屏 → 全屏）
  l                  显示 / 隐藏歌词（播放列表右侧）
  g                  切换播放列表视图（平铺 ↔ 按专辑分组）

【播放控制】
  空格               播放 / 暂停
  ← / →              快退 / 快进 5 秒
  n / p              下一曲 / 上一曲
  r                  切换循环（关 → 单曲循环 → 列表循环）
  s                  切换随机播放
  + / -              音量增减

【播放列表管理】
  a                  加入播放列表（文件或整个目录）
  d                  删除选中的曲目
  x                  清空播放列表（按两次确认）
  /                  搜索（浏览器按文件名递归搜索；
                     播放列表按曲名/歌手/专辑/路径）
                     Esc 退出搜索

【其他】
  ?                  显示「关于」（版本、开源声明、版权）
  q                  退出（自动保存配置）
  Ctrl+C             退出

【自定义键位】
  在 tuneux.toml 的 [keymap] 段可重映射播放控制键，
  动作名：toggle_play / next / prev / volume_up / volume_down；
  键描述语法：[shift+][ctrl+][alt+]<键名>，如 toggle_play = "ctrl+p"。
  非法键描述会被忽略并回退默认键。

四、三个系统的注意事项

【Windows】
  1. 首次运行会弹 SmartScreen「Windows 已保护你的电脑」，
     点「更多信息」→「仍要运行」即可。
  2. 配置保存在 tuneux.exe 同目录；若该目录不可写，
     则保存在 %APPDATA%\tuneux\ 目录。
  3. 文件浏览器支持跨盘符：退到盘符根目录（如 C:\）后，
     会列出其它盘符（D:\ 等），Enter 即可进入。

【macOS】
  1. 首次运行会被 Gatekeeper 拦截（提示移到废纸篓）。
     在「终端」里执行：
        cd 解压目录
        xattr -cr tuneux
        ./tuneux
  2. 配置保存在 tuneux 同目录；若不可写，
     则保存在 ~/Library/Application Support/tuneux/ 目录。

【Linux】
  1. 先赋予执行权限再运行：
        chmod +x tuneux
        ./tuneux
  2. 需要 libasound2（多数发行版已预装）。
  3. U 盘/外接盘上的程序不能直接运行（分区以 noexec 挂载），
     请先复制到本地磁盘（如家目录）再 chmod +x 运行。
  4. 配置保存在 tuneux 同目录；若不可写，
     则保存在 ~/.config/tuneux/ 目录。

五、关于 FFmpeg（可选扩展格式）

【核心格式不需要它】
  MP3 / FLAC / WAV / OGG / M4A / AAC / ALAC /
  Opus / WavPack 全部由纯 Rust 解码库进程内解码，
  自带、零依赖，不装任何东西都能播。

【FFmpeg 只服务长尾格式】
  APE / WMA / FLV / TAK / AC3 / DTS / DSD 等较少见的格式，
  tuneux 检测到系统里有 ffmpeg 命令就自动调用它解码；
  没有则优雅跳过（打开这类文件时会提示），无需任何配置。

【为什么让用户自己安装，而不是 tuneux 替你装好？】
  1. 保持「纯 Rust、纯离线」定位：核心解码完全自包含，
     无系统依赖、无联网安装步骤；捆绑 ffmpeg 会把几十 MB
     的外部二进制塞进安装包，破坏单文件便携与离线特性。
  2. 体积与维护成本：ffmpeg 本体 + 依赖动辄几十 MB，
     且需随上游持续更新安全补丁；捆绑会显著增大每个
     平台的下载体积和发布负担。
  3. 许可证复杂度：ffmpeg 采用 GPL / LGPL 双轨授权
     （取决于编译选项），不同发行版打包的许可状态不同；
     tuneux 本身是木兰宽松许可证 v2，捆绑分发会引入
     额外的许可证兼容负担与法律风险。
  4. 平台习惯不同：Windows / macOS / Linux 各有成熟
     的安装渠道（包管理器、Homebrew 等），让用户按平台
     习惯安装最省事、最安全。

【三个系统的安装方法】
  Windows：winget install ffmpeg
           或到 ffmpeg.org 下载解压后，把 bin 目录加入 PATH
  macOS：  brew install ffmpeg
  Linux：  Debian/Ubuntu：sudo apt install ffmpeg
           Fedora：sudo dnf install ffmpeg
           Arch：sudo pacman -S ffmpeg
  安装后验证：终端执行 ffmpeg -version 有输出版本信息即可。
  安装完成后无需任何配置：tuneux 每次打开扩展格式文件时
  自动探测 ffmpeg，找到即用。

【注意事项与风险】
  1. 只装扩展格式才需要 ffmpeg；不装也不影响核心格式
     的播放、Gapless、CUE 等全部功能。
  2. 请从官方渠道安装（系统包管理器 / ffmpeg.org / Homebrew），
     不要下载来路不明的「绿色版」，以免捆绑恶意软件。
  3. PATH 生效问题：Windows 安装后如仍提示「ffmpeg 未安装
     或不在 PATH 中」，需重开终端，或手动把 ffmpeg 的 bin
     目录加入系统 PATH；macOS 用 Homebrew 时请确认
     /opt/homebrew/bin（Apple Silicon）或 /usr/local/bin（Intel）
     在 PATH 中。
  4. 版本建议：推荐较新的 ffmpeg（5.x 及以上）；过旧版本
     可能缺少个别格式的解码器。
  5. 解码质量：无论是否使用 ffmpeg，tuneux 统一把解码结果
     转换为 32 位浮点 PCM 送入音频设备，不引入额外质量损失。

六、歌词

- 把歌词文件（.lrc）放在歌曲同目录、同名（如 稻香.mp3 配 稻香.lrc），
  播放时按 l 键即可显示。
- 支持 UTF-8、UTF-16、GBK/GB18030 编码，自动识别。
- 歌词随播放进度自动滚动，当前行高亮。

七、配置与播放列表

- 配置（音量、循环、随机、界面状态等）保存在 tuneux.toml。
- 播放列表与断点续播进度保存在 playlist.toml。
- 退出时自动保存，下次启动自动恢复播放列表和界面状态；
  选中上次播放的歌曲后按 Enter 可从上次位置续播。
- 正常退出用 q 键；直接关窗口也会每 5 秒自动保存一次。

八、许可

本项目采用木兰宽松许可证 v2（MulanPSL-2.0）开源。
版权所有 © 2026 不羁的青春（FreeYouth）。
不羁的青春® 和 FreeYouth® 是注册商标
