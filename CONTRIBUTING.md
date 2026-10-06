# 贡献指南

使用 pnpm，安装时保留锁文件。应用代码位于 `src/` 与 `src-tauri/`；原有两个参考目录独立维护。

## 修改前

- 功能范围参考 `docs/PRD.md` 与 `docs/TASKS.md`，本阶段保持基础工作台范围。
- UI 依照 `docs/DESIGN.md` 和 `docs/DESIGN-TOKENS.md`，优先复用 shadcn-macos。生成组件若补齐行为或样式，应记录原因。
- 持久化字段变更同步 Rust 与 TypeScript 校验、`src-tauri/CONTRACT.md` 和往返测试；拒绝未知数据，避免静默损失。
- Rust 管理桌面项目状态和磁盘 IO，Zustand 持有 UI 投影。取消或失败不能清除未保存状态。

## 验证

运行 `pnpm lint`、`pnpm test`、`pnpm build`，以及 `cargo test --manifest-path src-tauri/Cargo.toml` 和 `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`。涉及宿主时运行 `pnpm tauri build --no-bundle --debug`。

当前只以 Windows 为开发、交付和实机验收平台，macOS 等获得设备后单独适配。UI 修改验证固定深色、键盘焦点、减少动态效果、关闭透明度和窄窗口；平台窗口控件、字体和快捷键遵循 Windows 习惯。PR 说明区分已运行检查与未验证的平台，不把占位功能写成已实现。

## 文件与发布

使用合成或已授权测试媒体；用户项目、原始媒体、签名凭证、`target/` 和构建结果不进入源码库。KaiCut 自有代码使用 Apache-2.0；贡献应可依此分发，保留版权、适用 NOTICE 和第三方原许可证。

FFmpeg 仅以独立 LGPL sidecar 交付，默认 LGPL-2.1-or-later；禁止 GPL/nonfree 构建与 libx264/libx265。版本、构建选项、来源、校验和、许可证、对应源码和补丁须可追溯。使用独立进程不免除 FFmpeg 自身的分发义务；版本探测不是许可证审计。H.264 编码器不可用时明确报告，不静默换编码器或格式。
