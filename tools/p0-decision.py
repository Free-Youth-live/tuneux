#!/usr/bin/env python3
"""
max 重写 P0 决策脚本：统计 fx UiAction 中「语义必须一致」的占比，
输出 60/70/80 三档敏感性报告，决定方案一（共享 UI 模型）还是方案二（规格对拍）。

用法：python3 tools/p0-decision.py（在 code/ 根执行）
"""

# —— fx UiAction 全集（P0 冻结快照，分母）——
# 每条：(action_id, 触发方式, 语义必须一致?, 说明)
# 「语义必须一致」= max 重写中该动作的行为/效果必须与 fx 完全一致，
# 渲染方式（字符/像素）不计入此维度。
ACTIONS = [
    # —— 播放控制 ——
    ("play_toggle",      "Space",           True,  "播放/暂停切换"),
    ("prev_track",       "p",               True,  "上一曲"),
    ("next_track",       "n",               True,  "下一曲"),
    ("seek_back",        "←",               True,  "后退 5 秒"),
    ("seek_forward",     "→",               True,  "前进 5 秒"),
    ("volume_up",        "+/=",             True,  "音量 +5%"),
    ("volume_down",      "-",               True,  "音量 -5%"),
    ("cycle_repeat",     "r",               True,  "循环模式三态切换"),
    ("toggle_shuffle",   "s",               True,  "随机开关"),
    # —— 介质 ——
    ("medium_cycle",     "m",               True,  "介质循环切换"),
    ("medium_prev",      "[",               True,  "上一介质"),
    ("medium_next",      "]",               True,  "下一介质"),
    ("medium_set",       "菜单 9 项",       True,  "指定介质（9 变体）"),
    # —— 面板开合 ——
    ("toggle_browser",   "b / F5",          True,  "浏览器面板"),
    ("toggle_cover",     "c / F6",          True,  "封面面板"),
    ("toggle_lyrics",    "l / F7",          True,  "歌词面板"),
    ("toggle_spectrum",  "v / F8",          True,  "频谱面板"),
    ("toggle_eq",        "F9",              True,  "均衡器面板"),
    ("toggle_group",     "g",               True,  "播放列表分组视图"),
    # —— 播放列表管理 ——
    ("add_current",      "a",               True,  "加入当前条目"),
    ("add_dir",          "浏览器 Enter",    True,  "加入目录"),
    ("delete_item",      "d",               True,  "删除选中条目"),
    ("clear_confirm",    "x x",             True,  "二次确认清空"),
    # —— 导航 ——
    ("nav_up",           "↑ / k",           True,  "列表上移"),
    ("nav_down",         "↓ / j",           True,  "列表下移"),
    ("nav_enter",        "Enter",           True,  "进入/播放/确认"),
    ("nav_back",         "Esc",             True,  "返回/取消"),
    ("nav_home",         "Home",            True,  "跳到列表头"),
    ("nav_end",          "End",             True,  "跳到列表尾"),
    ("cycle_focus",      "Tab",             True,  "焦点循环"),
    # —— 功能面板 ——
    ("compressor",       "菜单",            True,  "压缩器面板"),
    ("plugin_eq",        "菜单",            True,  "EQ 插件面板"),
    ("plugin_comp",      "菜单",            True,  "压缩器插件面板"),
    ("plugin_visual",    "菜单",            True,  "可视化插件面板"),
    # —— 设置 ——
    ("skin_select",      "菜单",            True,  "皮肤选择"),
    ("lang_select",      "菜单",            True,  "语言选择"),
    ("output_device",    "菜单",            True,  "输出设备"),
    ("replaygain",       "菜单",            True,  "ReplayGain 开关"),
    # —— 信息 ——
    ("about",            "? / 菜单",        True,  "关于/快捷键速查"),
    # —— 搜索 ——
    ("search_start",     "/",               True,  "打开搜索"),
    ("search_navigate",  "↑↓",              True,  "搜索结果导航"),
    ("search_exit",      "Esc",             True,  "退出搜索"),
    # —— 命令模式 ——
    ("cmd_mode",         ":",               True,  "进入命令模式"),
    ("cmd_execute",      "Enter",           True,  "执行命令"),
    ("cmd_cancel",       "Esc",             True,  "取消命令"),
    ("cmd_repeat",       "repeat <mode>",   True,  "命令：设置循环模式"),
    ("cmd_volume",       "vol <N>",         True,  "命令：设置音量"),
    ("cmd_bookmark",     "bm",              True,  "命令：添加书签"),
    ("cmd_bm_del",       "bm-del <N>",      True,  "命令：删除书签"),
    ("cmd_m3u_save",     "m3u-save",        True,  "命令：保存 m3u"),
    ("cmd_help",         "help",            True,  "命令：帮助"),
    # —— 禁用项（占位，max 可不实现）——
    ("dsp_chain",        "菜单（灰显）",    False, "DSP 链管理（未实现）"),
    ("tag_edit",         "菜单（灰显）",    False, "标签编辑（未实现）"),
    ("convert",          "菜单（灰显）",    False, "格式转换（未实现）"),
    ("cover_manage",     "菜单（灰显）",    False, "封面管理（未实现）"),
    # —— 呈现差异（行为一致但渲染形式不同）——
    ("spectrum_render",  "—",               False, "频谱渲染细节（字符/像素）"),
    ("cover_render",     "—",               False, "封面渲染方式（字符/图像）"),
    ("waveform_render",  "—",               False, "波形渲染方式"),
    ("level_render",     "—",               False, "电平表渲染方式"),
    ("menu_bar_render",  "—",               False, "菜单栏渲染方式（文字/像素）"),
]

def analyze():
    total = len(ACTIONS)
    semantic = sum(1 for _, _, s, _ in ACTIONS if s)
    pct = semantic / total * 100

    print(f"UiAction 全集（分母，冻结）: {total}")
    print(f"语义必须一致（分子）:         {semantic}")
    print(f"占比:                        {pct:.1f}%")
    print()

    # 敏感性分析：60% / 70% / 80% 三档
    for threshold in [60, 70, 80]:
        if pct >= threshold:
            decision = "方案一（共享 UI 模型 commonx::ui）"
        else:
            decision = "规格对拍（独立实现 + 规格快照验证）"
        marker = " ←" if threshold == 70 else ""
        print(f"  阈值 {threshold}%: {pct:.1f}% ≥ {threshold}% → {decision}{marker}")

    print()
    # 判定（评审结论：脚本未就绪或恰好落在区间内默认方案一）
    if pct >= 70:
        print(f"★ 终选: 方案一（共享 UI 模型）—— 占比 {pct:.1f}% ≥ 70%")
    elif pct >= 60:
        print(f"★ 终选: 方案一（共享 UI 模型）—— 占比 {pct:.1f}% 落在 60-70% 区间，按评审结论默认方案一")
    else:
        print(f"★ 终选: 规格对拍 —— 占比 {pct:.1f}% < 60%")

    print()
    print("无条件共享项（无论终选哪条）: i18n / PlaylistState / artwork / keymap")

if __name__ == "__main__":
    analyze()
