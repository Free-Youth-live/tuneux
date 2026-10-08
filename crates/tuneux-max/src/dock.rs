//! 停靠布局：分割树（叶 = 模块，内节点 = 可拖比例分割）。
//!
//! 对齐由构造保证（兄弟共享边）；任意模块组合共显（不像 fx 的面板互斥）；
//! 右键叶标题带关闭、菜单加装；整棵树 serde 持久化进用户配置。
//! 分割条双击回 50/50，拖拽实时改比例（fx Half 分隔条的同款手法推广到树）。

use serde::{Deserialize, Serialize};

/// 可停靠模块（serde 表示冻结：写入用户配置，变更 = 破坏性变更）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleId {
    Browser,
    Playlist,
    Spectrum,
    Cover,
    Lyrics,
    Eq,
    Comp,
    Waveform,
    Filter,
    Visualizer,
    DspChain,
    /// 指针表（VU 风格指针表，L/R 双表）。
    VuGauge,
}

impl ModuleId {
    /// 全模块清单（菜单加装列表用）。
    pub const ALL: [ModuleId; 12] = [
        ModuleId::Browser,
        ModuleId::Playlist,
        ModuleId::Spectrum,
        ModuleId::Cover,
        ModuleId::Lyrics,
        ModuleId::Eq,
        ModuleId::Comp,
        ModuleId::Waveform,
        ModuleId::Filter,
        ModuleId::Visualizer,
        ModuleId::DspChain,
        ModuleId::VuGauge,
    ];

    /// 小模块加装时优先纵向堆叠（占纵向位置，而非一律横排）。
    pub fn prefers_vertical(self) -> bool {
        matches!(
            self,
            ModuleId::Cover | ModuleId::Lyrics | ModuleId::Eq | ModuleId::Comp | ModuleId::VuGauge
        )
    }

    /// 显示名 i18n key（复用 panel.* 词条，不新增）。
    pub fn label_key(self) -> &'static str {
        match self {
            ModuleId::Browser => "panel.browser",
            ModuleId::Playlist => "panel.playlist",
            ModuleId::Spectrum => "panel.spectrum",
            ModuleId::Cover => "panel.cover",
            ModuleId::Lyrics => "panel.lyrics",
            ModuleId::Eq => "panel.eq",
            ModuleId::Comp => "panel.compressor",
            ModuleId::Waveform => "panel.waveform",
            ModuleId::Filter => "panel.filter",
            ModuleId::Visualizer => "panel.visualizer",
            ModuleId::DspChain => "panel.dspchain",
            ModuleId::VuGauge => "panel.gauge",
        }
    }
}

/// 分割树节点。ratio = a 侧占比（0.08–0.92 钳制）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DockNode {
    Leaf {
        module: ModuleId,
    },
    Split {
        /// true = 左右并排；false = 上下堆叠。
        side_by_side: bool,
        ratio: f32,
        a: Box<DockNode>,
        b: Box<DockNode>,
    },
}

impl DockNode {
    pub fn leaf(module: ModuleId) -> Self {
        DockNode::Leaf { module }
    }

    /// 是否包含该模块。
    pub fn contains(&self, m: ModuleId) -> bool {
        match self {
            DockNode::Leaf { module } => *module == m,
            DockNode::Split { a, b, .. } => a.contains(m) || b.contains(m),
        }
    }

    /// 移除叶子：返回剪枝后的子树（None = 整棵树空了）。
    pub fn prune(self, m: ModuleId) -> Option<DockNode> {
        match self {
            DockNode::Leaf { module } => {
                if module == m {
                    None
                } else {
                    Some(self)
                }
            }
            DockNode::Split {
                side_by_side,
                ratio,
                a,
                b,
            } => match (a.prune(m), b.prune(m)) {
                (Some(a), Some(b)) => Some(DockNode::Split {
                    side_by_side,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            },
        }
    }

    /// 纵向加装：最右叶处垂直分割、新叶居下（小模块默认路径）。
    pub fn append_small(self, m: ModuleId) -> DockNode {
        match self {
            DockNode::Leaf { .. } => DockNode::Split {
                side_by_side: false,
                ratio: 0.55,
                a: Box::new(self),
                b: Box::new(DockNode::leaf(m)),
            },
            DockNode::Split {
                side_by_side,
                ratio,
                a,
                b,
            } => DockNode::Split {
                side_by_side,
                ratio,
                a,
                b: Box::new(b.append_small(m)),
            },
        }
    }

    /// 在 target 叶处拆分并插入新模块（f2k Split horizontally/vertically：
    /// 原叶居 a 侧、新叶居 b 侧，五五开）。
    pub fn split_insert(&mut self, target: ModuleId, new: ModuleId, side_by_side: bool) {
        fn go(node: &mut DockNode, target: ModuleId, new: ModuleId, sbs: bool) -> bool {
            match node {
                DockNode::Leaf { module } => {
                    if *module == target {
                        *node = DockNode::Split {
                            side_by_side: sbs,
                            ratio: 0.5,
                            a: Box::new(DockNode::leaf(target)),
                            b: Box::new(DockNode::leaf(new)),
                        };
                        true
                    } else {
                        false
                    }
                }
                DockNode::Split { a, b, .. } => go(a, target, new, sbs) || go(b, target, new, sbs),
            }
        }
        go(self, target, new, side_by_side);
    }

    /// 原位更换模块（f2k Change panel to…）。
    pub fn replace_leaf(&mut self, target: ModuleId, new: ModuleId) {
        fn go(node: &mut DockNode, target: ModuleId, new: ModuleId) {
            match node {
                DockNode::Leaf { module } => {
                    if *module == target {
                        *module = new;
                    }
                }
                DockNode::Split { a, b, .. } => {
                    go(a, target, new);
                    go(b, target, new);
                }
            }
        }
        go(self, target, new);
    }

    /// 交换两个叶子模块的位置（拖拽换位；递归全树替换标记）。
    pub fn swap_modules(&mut self, a: ModuleId, b: ModuleId) {
        fn go(node: &mut DockNode, a: ModuleId, b: ModuleId) {
            match node {
                DockNode::Leaf { module } => {
                    if *module == a {
                        *module = b;
                    } else if *module == b {
                        *module = a;
                    }
                }
                DockNode::Split { a: na, b: nb, .. } => {
                    go(na, a, b);
                    go(nb, a, b);
                }
            }
        }
        go(self, a, b);
    }

    /// 右侧追加叶子（根级并排分割）。
    pub fn append_right(self, m: ModuleId) -> DockNode {
        DockNode::Split {
            side_by_side: true,
            ratio: 0.72,
            a: Box::new(self),
            b: Box::new(DockNode::leaf(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> DockNode {
        DockNode::Split {
            side_by_side: true,
            ratio: 0.3,
            a: Box::new(DockNode::leaf(ModuleId::Browser)),
            b: Box::new(DockNode::Split {
                side_by_side: false,
                ratio: 0.6,
                a: Box::new(DockNode::leaf(ModuleId::Playlist)),
                b: Box::new(DockNode::leaf(ModuleId::Cover)),
            }),
        }
    }

    #[test]
    fn contains_and_prune() {
        let t = tree();
        assert!(t.contains(ModuleId::Browser));
        assert!(t.contains(ModuleId::Cover));
        assert!(!t.contains(ModuleId::Eq));
        // 剪掉叶子：兄弟提升
        let t2 = t.clone().prune(ModuleId::Cover).unwrap();
        assert!(!t2.contains(ModuleId::Cover));
        assert!(t2.contains(ModuleId::Playlist));
        // 剪到空 = None
        let mut t3 = tree();
        for m in [ModuleId::Browser, ModuleId::Playlist, ModuleId::Cover] {
            t3 = t3.prune(m).unwrap_or(DockNode::leaf(ModuleId::Eq));
            if !t3.contains(m) && t3.contains(ModuleId::Eq) {
                break;
            }
        }
        let empty = DockNode::leaf(ModuleId::Eq).prune(ModuleId::Eq);
        assert!(empty.is_none());
    }

    #[test]
    fn append_small_stacks_rightmost() {
        let t = tree().append_small(ModuleId::Eq);
        assert!(t.contains(ModuleId::Eq));
        // 最右列被垂直分割：Cover 与 Eq 同列上下
        let DockNode::Split { a, b, .. } = &t else {
            panic!("root split");
        };
        let DockNode::Split {
            side_by_side,
            b: b2,
            ..
        } = &**b
        else {
            panic!("right split");
        };
        assert!(!a.contains(ModuleId::Eq));
        assert!(!side_by_side);
        assert!(b2.contains(ModuleId::Eq));
    }

    #[test]
    fn swap_modules_exchanges_positions() {
        let mut t = tree();
        // 浏览器（左）与封面（右下）换位
        t.swap_modules(ModuleId::Browser, ModuleId::Cover);
        // 树里两模块仍在，且结构不变（叶互换）
        assert!(t.contains(ModuleId::Browser));
        assert!(t.contains(ModuleId::Cover));
        let DockNode::Split { a, b, .. } = &t else {
            panic!("root split");
        };
        assert!(matches!(
            **a,
            DockNode::Leaf {
                module: ModuleId::Cover
            }
        ));
        assert!(b.contains(ModuleId::Browser));
    }

    #[test]
    fn split_insert_and_replace() {
        let mut t = DockNode::leaf(ModuleId::Playlist);
        // 原地拆分插入：列表旁加 EQ（水平）
        t.split_insert(ModuleId::Playlist, ModuleId::Eq, true);
        assert!(t.contains(ModuleId::Playlist));
        assert!(t.contains(ModuleId::Eq));
        // 原位更换：EQ → 压缩器
        t.replace_leaf(ModuleId::Eq, ModuleId::Comp);
        assert!(!t.contains(ModuleId::Eq));
        assert!(t.contains(ModuleId::Comp));
        assert!(t.contains(ModuleId::Playlist));
    }

    #[test]
    fn append_right_nests() {
        let t = DockNode::leaf(ModuleId::Playlist).append_right(ModuleId::Eq);
        assert!(t.contains(ModuleId::Playlist));
        assert!(t.contains(ModuleId::Eq));
    }
}
