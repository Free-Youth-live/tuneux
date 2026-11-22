//! # 居中弹窗建造者
//!
//! 不用 `Window::anchor`：anchor 每帧把窗口钉回锚点，拖动会被逐帧抵消
//! （表现「弹窗拖不动」）。改用 `default_pos`——仅无记忆状态时生效，
//! 首帧居中，之后位置进 egui 记忆、可拖动。

use eframe::egui;

/// 居中弹窗建造者。
pub struct Dialog {
    title: String,
    collapsible: bool,
    min_width: Option<f32>,
    max_height: Option<f32>,
    default_width: Option<f32>,
}

/// 新建居中弹窗。
pub fn dialog(title: impl Into<String>) -> Dialog {
    Dialog {
        title: title.into(),
        collapsible: true,
        min_width: None,
        max_height: None,
        default_width: None,
    }
}

impl Dialog {
    /// 是否可折叠（默认 true；信息弹窗通常关闭）。
    pub fn collapsible(mut self, value: bool) -> Self {
        self.collapsible = value;
        self
    }

    /// 最小宽度（窄文案不被压缩换行）。
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = Some(width);
        self
    }

    /// 最大高度（超出滚动；后续弹窗使用）。
    #[allow(dead_code)]
    pub fn max_height(mut self, height: f32) -> Self {
        self.max_height = Some(height);
        self
    }

    /// 默认宽度。
    pub fn default_width(mut self, width: f32) -> Self {
        self.default_width = Some(width);
        self
    }

    /// 显示弹窗，返回内容闭包的结果（窗口被关闭时 None）。
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<R> {
        let center = ctx.input(|input| input.screen_rect().center());
        // default_pos 指定的是窗口「pivot 点」的位置：默认 pivot=左上角，
        // 只写 default_pos(center) 会把左上角钉在屏幕中心（弹窗偏右下）。
        // 配 pivot=中心 才真正居中；pivot 不同于 anchor，不逐帧钉死，仍可拖动。
        let mut window = egui::Window::new(self.title)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(self.collapsible);
        if let Some(width) = self.min_width {
            window = window.min_width(width);
        }
        if let Some(height) = self.max_height {
            window = window.max_height(height);
        }
        if let Some(width) = self.default_width {
            window = window.default_width(width);
        }
        window
            .show(ctx, add_contents)
            .and_then(|response| response.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chains() {
        let dialog = dialog("测试")
            .collapsible(false)
            .min_width(320.0)
            .max_height(480.0)
            .default_width(400.0);
        assert_eq!(dialog.title, "测试");
        assert!(!dialog.collapsible);
        assert_eq!(dialog.min_width, Some(320.0));
        assert_eq!(dialog.max_height, Some(480.0));
        assert_eq!(dialog.default_width, Some(400.0));
    }
}
