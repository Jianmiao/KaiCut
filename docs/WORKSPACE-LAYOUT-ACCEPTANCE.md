# 工作台布局与品牌图标验收

日期：2026-10-06

本轮按用户提供的剪映顶部栏、工作台分隔线和预览控制条参考调整展示层。项目状态、媒体导入、`.kaicut` schema、剪辑数据和导出逻辑未改动。

## 结果

- 品牌标识和 Tauri 应用图标使用用户提供的影片、播放与剪刀组合图形；外侧白色画布已转换为透明角，内部白色细节保留。
- Windows 将标题栏合并进同一条顶部栏：左侧品牌/项目菜单/保存状态，中间项目名，右侧快捷键/布局/导出和系统风格窗口按钮。保留 Windows 标题栏语义，不使用 macOS 红黄绿按钮；macOS 与其他平台继续使用原生标题栏。不加入会员入口、一级业务导航或运营内容。
- 素材区与检查器各有一条可见垂直分隔器。素材区范围180–360px，检查器范围220–380px，默认值240/280px；支持拖拽和左右方向键微调。
- 预览底栏改为左侧当前/总时长，中间播放按钮，右侧原画、缩放、比例、音量和全屏控制。当前空项目显示零时长。
- 时间线保持上一轮的 96px 轨道头、视频缩略条、音频波形、白色1px播放头和中性蓝灰类型色。

## 验证

- `pnpm lint`：通过
- `pnpm test`：40 项通过
- `pnpm build`：通过
- `pnpm tauri build --no-bundle --debug`：通过
- [Windows Tauri 合并标题栏截图](./screenshots/integrated-titlebar/native-default.jpg)：真实窗口1283×851，系统标题栏不再单独占一行。
- Windows Tauri 真实窗口：单行标题栏、Windows 风格控制按钮、最大化后按钮变为“还原”、空白区域拖动和顶部项目菜单已实测。
- 自动化关闭测试确认按钮调用 Tauri `close()` 而非 `destroy()`，保留 Rust `CloseRequested` 未保存项目防护。
- 本次 Computer Use 被物理 Escape 中断，因此未完成关闭按钮触发原生未保存提示框的实窗验证。
- 拖拽素材区：240px → 约319px，成功
- 拖拽检查器：280px → 约348px，成功
- macOS 实机：待设备

本轮标题栏截图保存在 `docs/screenshots/integrated-titlebar/native-default.jpg`；工作区基准图位于 `docs/screenshots/workspace-layout/native-default.jpg`。当前阶段只展示空项目，未伪造真实媒体片段。
