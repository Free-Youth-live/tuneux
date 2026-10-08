//! # 指针表（VU 风格指针表：几何与针物理）
//!
//! 「仿硬件指针表」的**几何与物理**：产出整块字符画布的全部图层
//! 单元格，产品层（fx 终端 / max GUI）只按 [`GaugeKind`] 分家族着色。
//! 视觉口径（冻结规格，见 007 功能三）：**单线制框**（上/左亮、下/右暗）+
//! 凹陷盘壁（顶行一行 ▒）+ 连续刻度弧（'·' 基线 + '●•' 主刻度）+ 红区
//! 双厚 '▪' 带 + **一条线指针**（/ │ \ 整根同字符，弹簧阻尼 300ms 微过冲）+
//! 2×2 针帽（▄█▀▀）+ 数字标注（-20..+3 dB）。
//!
//! **无投影、无阴影、无半块光栅化**——像素半块合成、指针重影、针帽投影
//! 均在开发中被用户裁决否决，勿回潮（NeedleShadow / PivotShadow 图层
//! 枚举仅保留兼容产品 match，恒不绘制）。
//! 颜色由产品层按 [`GaugeKind`] 映射——任何皮肤可用。纯数学零依赖。

/// 指针弹簧-阻尼物理（300ms 弹道 + 微过冲——表针的"呼吸感"）。
#[derive(Debug, Clone)]
pub struct NeedlePhys {
    /// 当前位置（frac 0..1）。
    x: f32,
    /// 速度。
    v: f32,
    /// 固有角频率（rad/s；2π/0.3 ≈ 21）。
    omega: f32,
    /// 阻尼比（<1 欠阻尼过冲）。
    zeta: f32,
    /// 是否已收到首个目标（首帧直接就位）。
    primed: bool,
}

impl Default for NeedlePhys {
    fn default() -> Self {
        Self::new(0.3, 0.8)
    }
}

impl NeedlePhys {
    /// 创建：`rise_time` 固有周期（秒），`zeta` 阻尼比。
    pub fn new(rise_time: f32, zeta: f32) -> Self {
        let tau = rise_time.max(0.01);
        Self {
            x: 0.0,
            v: 0.0,
            omega: std::f32::consts::TAU / tau,
            zeta: zeta.clamp(0.1, 2.0),
            primed: false,
        }
    }

    /// 推进一个时间步，向 `target`（frac 0..1）收敛。返回当前针位。
    pub fn update(&mut self, target: f32, dt: f32) -> f32 {
        let dt = dt.clamp(0.0, 0.1);
        if !self.primed {
            self.x = target.clamp(0.0, 1.0);
            self.primed = true;
            return self.x;
        }
        let t = if target.is_finite() {
            target.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let accel = self.omega * self.omega * (t - self.x) - 2.0 * self.zeta * self.omega * self.v;
        self.v += accel * dt;
        self.x += self.v * dt;
        if !self.x.is_finite() {
            self.x = t;
            self.v = 0.0;
        }
        self.x = self.x.clamp(-0.08, 1.08); // 允许微过冲出界
        self.x
    }

    /// 当前针位（frac 0..1，可能略越界——过冲）。
    pub fn value(&self) -> f32 {
        self.x
    }
}

/// 网格单元格所属图层（产品层据此着色；灰阶由字符本身承担）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GaugeKind {
    /// 表框上轨（亮）。
    BezelTop,
    /// 表框下轨（暗）。
    BezelBottom,
    /// 凹陷盘壁（上壁阴影弧带）。
    RecessWall,
    /// 主刻度。
    TickMajor,
    /// 次刻度。
    TickMinor,
    /// 红区弧带（过载告警）。
    RedZone,
    /// 指针投影图层（**保留兼容产品 match，恒不绘制**——投影方案
    /// 已被用户裁决否决，勿回潮）。
    NeedleShadow,
    /// 指针本体。
    Needle,
    /// 针帽投影图层（**保留兼容产品 match，恒不绘制**——同上）。
    PivotShadow,
    /// 针帽（2×2 小盖 ▄█▀▀）。
    Pivot,
    /// 数字标注（-20/-10/-5/0/+3）。
    Label,
}

/// 一个网格单元格：列 / 行 / 字符 / 图层。
#[derive(Debug, Clone, Copy)]
pub struct GaugeCell {
    /// 列（0 基，画布左起）。
    pub x: u16,
    /// 行（0 基，画布顶起）。
    pub y: u16,
    /// 灰阶/半块/文字字符。
    pub ch: char,
    /// 图层（着色依据）。
    pub kind: GaugeKind,
}

/// 画布最小宽（低于此几何挤不下）。
pub const GAUGE_MIN_W: u16 = 17;
/// 画布最小高。
pub const GAUGE_MIN_H: u16 = 10;

/// 指针扫描半角（度，自正上方起 ±；45° ≈ 真实 VU 表量级，
/// 且陡角处斜率 ≤ ~1.8 列/行——单格线针不断线感）。
const SWEEP_DEG: f32 = 45.0;
/// 红区起点（-20..+3dB 域里 0dB ≈ 20/23）。
const RED_FROM: f32 = 0.87;

/// 生成一块指针表的全部单元格。
///
/// `w` / `h` 为字符画布尺寸（低于最小值自动抬到最小值）；`frac` 为针位
/// 0..1（0 = 最左 -45°，1 = 最右 +45°；允许略越界呈现物理过冲）。
///
/// 风格（冻结规格口径）：单线制框（┌─┐│└┘）+ 凹陷盘壁一行 ▒ +
/// 连续刻度弧（· 基线 + ●• 主刻度）+ 红区 ▪ 双厚弧带 + 一条线指针
/// （/ │ \ 按斜率整根同字符）+ 针帽（▄█▀▀ 小盖）+ 数字标注。
/// 图层叠序：框 → 盘壁 → 刻度/红区 → 针 → 帽。
pub fn gauge_cells(w: u16, h: u16, frac: f32) -> Vec<GaugeCell> {
    let w = w.clamp(GAUGE_MIN_W, 60) as i32;
    let h = h.clamp(GAUGE_MIN_H, 24) as i32;
    // 网格 (x,y) 键去重——后画覆盖先画，天然实现图层序
    let mut grid: std::collections::HashMap<(i32, i32), GaugeCell> =
        std::collections::HashMap::new();
    let mut put = |x: i32, y: i32, ch: char, kind: GaugeKind| {
        if (0..w).contains(&x) && (0..h).contains(&y) {
            grid.insert(
                (x, y),
                GaugeCell {
                    x: x as u16,
                    y: y as u16,
                    ch,
                    kind,
                },
            );
        }
    };

    // —— 几何：针轴在底部中央；弧取**视觉正圆**——
    // 终端字符宽高 ≈ 1:2，视觉圆要求 ry(行) ≈ rx(列) × 0.55。
    // （旧版 ry=cy-1 偏大，弧被拉成「高瘦椭圆」——不自然的来源。）
    let cx = w / 2;
    let cy = h - 3;
    let rx = (w / 2 - 2).max(4) as f32;
    let ry = (rx * 0.55).min(cy as f32 - 1.5).max(3.0);
    let sweep = SWEEP_DEG.to_radians();
    let angle_of = |f: f32| -sweep + 2.0 * sweep * f.clamp(-0.05, 1.05);
    let pt_on = |r: f32, a: f32| -> (f32, f32) {
        (cx as f32 + rx * r * a.sin(), cy as f32 - ry * r * a.cos())
    };

    // —— 1. 单线制框（上/左 = 亮家族，下/右 = 暗家族）——
    for x in 1..w - 1 {
        put(x, 0, '─', GaugeKind::BezelTop);
        put(x, h - 1, '─', GaugeKind::BezelBottom);
    }
    for y in 1..h - 1 {
        put(0, y, '│', GaugeKind::BezelTop);
        put(w - 1, y, '│', GaugeKind::BezelBottom);
    }
    put(0, 0, '┌', GaugeKind::BezelTop);
    put(w - 1, 0, '┐', GaugeKind::BezelTop);
    put(0, h - 1, '└', GaugeKind::BezelBottom);
    put(w - 1, h - 1, '┘', GaugeKind::BezelBottom);

    // —— 2. 凹陷盘壁（顶壁一行 ▒——盘面「陷下去」的读法来源）——
    for x in 1..w - 1 {
        put(x, 1, '▒', GaugeKind::RecessWall);
    }

    // —— 3. 刻度弧（连续基线 + 径向主刻度 + 双厚红区带）——
    // 基线：高密度采样（步距 ≪ 1 格）让 '·' 连成不间断的印刷弧线；
    // 主刻度：弧上 '●' + 向心一格 '•'（径向短线，读作长刻度）；
    // 红区：基线 + 内圈双厚度 '▪' 弧带（过载区加重）。
    let arc_steps = 80;
    for k in 0..=arc_steps {
        let f = k as f32 / arc_steps as f32;
        let a = angle_of(f);
        let (x, y) = pt_on(1.0, a);
        put(
            x.round() as i32,
            y.round() as i32,
            '·',
            GaugeKind::TickMinor,
        );
    }
    for k in 0..=32 {
        let f = k as f32 / 32.0;
        if k % 8 != 0 {
            continue;
        }
        let a = angle_of(f);
        let (x, y) = pt_on(1.0, a);
        put(
            x.round() as i32,
            y.round() as i32,
            '●',
            GaugeKind::TickMajor,
        );
        let (xi, yi) = pt_on(0.89, a);
        put(
            xi.round() as i32,
            yi.round() as i32,
            '•',
            GaugeKind::TickMajor,
        );
    }
    for k in 0..=arc_steps {
        let f = k as f32 / arc_steps as f32;
        if f < RED_FROM {
            continue;
        }
        let a = angle_of(f);
        let (x, y) = pt_on(1.0, a);
        put(x.round() as i32, y.round() as i32, '▪', GaugeKind::RedZone);
        let (xi, yi) = pt_on(0.89, a);
        put(
            xi.round() as i32,
            yi.round() as i32,
            '▪',
            GaugeKind::RedZone,
        );
    }

    // —— 4. 一条线指针（轴到针尖；每行恰一格）——
    // 整根针用**同一字符**（按主导斜率一次选定）：逐行舍入的 dx 在
    // 0/1 间跳变会让 |/ 交替出现「打弯」——这是上一版不直的根源。
    // 字形斜度天然弥合 ≤1 列的行间间隙，视觉上是一条连续直线。
    let a = angle_of(frac);
    let tip = pt_on(0.92, a);
    let slope = (tip.0 - cx as f32) / ((cy as f32 - tip.1).max(1.0));
    let needle_ch = if slope < -0.3 {
        '\\'
    } else if slope > 0.3 {
        '/'
    } else {
        '|'
    };
    let y_top = tip.1.round().max(1.0) as i32;
    let rows = (cy - y_top).max(1);
    for k in 0..=rows {
        let y = cy - k;
        let t = k as f32 / rows as f32;
        let x = cx as f32 + (tip.0 - cx as f32) * t;
        put(x.round() as i32, y, needle_ch, GaugeKind::Needle);
    }

    // —— 5. 针帽（2×2 小盖 ▄█▀▀；无投影）——
    for (dx, dy, ch) in [(-1i32, 0i32, '▄'), (0, 0, '█'), (-1, 1, '▀'), (0, 1, '▀')] {
        put(cx + dx, cy + dy, ch, GaugeKind::Pivot);
    }

    // 输出按行序排序
    let mut out: Vec<GaugeCell> = grid.into_values().collect();
    out.sort_by_key(|c| (c.y, c.x));

    // —— 7. 数字标注（最外层文字覆盖；刻度向外 6px，越界贴边钳制）——
    if w >= 27 {
        let labels: [(f32, &str); 5] = [
            (0.0, "-20"),
            (0.25, "-10"),
            (0.5, "-5"),
            (RED_FROM, "0"),
            (1.0, "+3"),
        ];
        let mut text_overlay: std::collections::HashMap<(u16, u16), char> =
            std::collections::HashMap::new();
        for (f, txt) in labels {
            let ang = angle_of(f);
            // 弧内圈（r=0.78）——数字安在盘面上，不与外框/刻度尺争位
            let (lx, ly) = pt_on(0.78, ang);
            let len = txt.len() as i32;
            let start = (lx.round() as i32 - len / 2).clamp(1, (w - 1 - len).max(1));
            let row_c = (ly.round() as i32).clamp(2, h - 2);
            for (i, ch) in txt.chars().enumerate() {
                let x = (start + i as i32).clamp(1, w - 2);
                text_overlay.insert((x as u16, row_c as u16), ch);
            }
        }
        let have: std::collections::HashSet<(u16, u16)> = out.iter().map(|c| (c.x, c.y)).collect();
        let mut extra: Vec<GaugeCell> = Vec::new();
        for cell in out.iter_mut() {
            if let Some(ch) = text_overlay.get(&(cell.x, cell.y)) {
                cell.ch = *ch;
                cell.kind = GaugeKind::Label;
            }
        }
        for ((x, y), ch) in text_overlay {
            if !have.contains(&(x, y)) {
                extra.push(GaugeCell {
                    x,
                    y,
                    ch,
                    kind: GaugeKind::Label,
                });
            }
        }
        out.extend(extra);
        out.sort_by_key(|c| (c.y, c.x));
    }

    out
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 所有单元格落在画布内；输出按行序有序。
    #[test]
    fn cells_in_bounds_and_sorted() {
        for (w, h) in [(17, 10), (23, 11), (40, 14), (8, 5), (60, 24)] {
            for frac in [0.0, 0.25, 0.5, 0.75, 1.0, -0.04, 1.05] {
                let cells = gauge_cells(w, h, frac);
                assert!(!cells.is_empty());
                let (ew, eh) = (w.clamp(GAUGE_MIN_W, 60), h.clamp(GAUGE_MIN_H, 24));
                for c in &cells {
                    assert!(c.x < ew, "x 越界 {}/{}", c.x, ew);
                    assert!(c.y < eh, "y 越界 {}/{}", c.y, eh);
                }
                for pair in cells.windows(2) {
                    assert!(
                        (pair[0].y, pair[0].x) <= (pair[1].y, pair[1].x),
                        "应按 (y,x) 有序"
                    );
                }
            }
        }
    }

    /// 核心图层完备（标准画布）。
    #[test]
    fn core_kinds_present() {
        let cells = gauge_cells(31, 12, 0.5);
        let kinds: HashSet<_> = cells.iter().map(|c| c.kind).collect();
        for k in [
            GaugeKind::BezelTop,
            GaugeKind::BezelBottom,
            GaugeKind::RecessWall,
            GaugeKind::TickMajor,
            GaugeKind::TickMinor,
            GaugeKind::RedZone,
            GaugeKind::Needle,
            GaugeKind::Pivot,
            GaugeKind::Label,
        ] {
            assert!(kinds.contains(&k), "缺少图层 {k:?}");
        }
    }

    /// 窄画布自动跳过数字标注（不 panic、无 Label）。
    #[test]
    fn narrow_canvas_skips_labels() {
        let cells = gauge_cells(17, 10, 0.5);
        assert!(cells.iter().all(|c| c.kind != GaugeKind::Label));
    }

    /// 字符集合法（制框 / 灰阶 / 线画 / 刻度数字）。
    #[test]
    fn charset_legal() {
        let legal: HashSet<char> = "┌┐└┘─│▒█▄▀·●•▪/|\\-0123456789+".chars().collect();
        for frac in [0.0, 0.33, 0.66, 1.0] {
            for c in gauge_cells(31, 12, frac) {
                assert!(
                    legal.contains(&c.ch),
                    "非法字符 {:?}（kind {:?}）",
                    c.ch,
                    c.kind
                );
            }
        }
    }

    /// 指针位置随 frac 单调（针尖最右格右移）。
    #[test]
    fn needle_monotonic() {
        // 判据：最高（y 最小）的针元格 = 针尖；其 x 随 frac 右移
        let tip_x = |frac: f32| -> i32 {
            gauge_cells(31, 12, frac)
                .iter()
                .filter(|c| c.kind == GaugeKind::Needle)
                .min_by_key(|c| (c.y, c.x))
                .map(|c| c.x as i32)
                .unwrap_or(0)
        };
        let l = tip_x(0.05);
        let m = tip_x(0.5);
        let r = tip_x(0.95);
        assert!(l < m && m < r, "针尖应随 frac 右移：{l} {m} {r}");
    }

    /// 弧线连续性：刻度格数量级达到「连线」而非「散点」（80 步采样
    /// 覆盖 ≈21 列弧程，四舍五入去重后应剩 ≥18 格）。
    #[test]
    fn arc_is_continuous_not_scattered() {
        let cells = gauge_cells(31, 12, 0.5);
        let n = cells
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    GaugeKind::TickMajor | GaugeKind::TickMinor | GaugeKind::RedZone
                )
            })
            .count();
        assert!(n >= 18, "刻度弧应接近连续，实际 {n} 格");
    }

    /// 一条线不变式：整根针所有单元格用同一字符（无 |/ 交替抖动）。
    #[test]
    fn needle_single_char() {
        for frac in [0.05, 0.3, 0.5, 0.7, 0.95] {
            let cells = gauge_cells(31, 12, frac);
            let chars: HashSet<char> = cells
                .iter()
                .filter(|c| c.kind == GaugeKind::Needle)
                .map(|c| c.ch)
                .collect();
            assert_eq!(chars.len(), 1, "frac={frac} 针字符应唯一：{chars:?}");
            assert!(
                chars.iter().all(|&c| c == '|' || c == '/' || c == '\\'),
                "针字符应为线画字符"
            );
        }
    }

    /// 无阴影：投影图层不再绘制（净版）。
    #[test]
    fn no_shadows_painted() {
        let cells = gauge_cells(31, 12, 0.5);
        assert!(cells.iter().all(|c| c.kind != GaugeKind::NeedleShadow));
        assert!(cells.iter().all(|c| c.kind != GaugeKind::PivotShadow));
    }

    /// 物理收敛 + 过冲 + NaN 防御。
    #[test]
    fn needle_phys_converges_and_overshoots() {
        let mut n = NeedlePhys::default();
        assert_eq!(n.update(0.5, 0.016), 0.5, "首帧直接就位");
        let mut over = false;
        for _ in 0..60 {
            let x = n.update(1.0, 0.016);
            if x > 1.0 {
                over = true;
            }
        }
        assert!((n.value() - 1.0).abs() < 0.02, "应收敛");
        assert!(over, "ζ=0.8 应有微过冲");
        let x = n.update(f32::NAN, 0.016);
        assert!(x.is_finite(), "NaN 不污染状态");
    }

    /// 模糊化烟雾：任意尺寸 + 任意针位不 panic。
    #[test]
    fn never_panics_on_odd_sizes() {
        let mut s: u64 = 0x5EED_2026;
        let mut rng = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..200 {
            let w = 1 + (rng() % 60) as u16;
            let h = 1 + (rng() % 24) as u16;
            let f = (rng() % 200) as f32 / 100.0 - 0.5;
            let _ = gauge_cells(w, h, f);
        }
    }
}
