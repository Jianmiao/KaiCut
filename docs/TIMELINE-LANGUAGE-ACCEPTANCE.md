# 时间线视觉语言修订验收

日期：2026-10-06

本轮只修改时间线展示层，没有写入 `tracks[].clips`，没有改变 `.kaicut` schema、媒体状态、项目保存或导出行为。

## 截图

- [Windows Tauri 原生窗口](./screenshots/timeline-language/native-timeline.jpg)：包含真实窗口边框和底部状态栏。
- [剪映 / KaiCut / Final Cut Pro 三方并排](./screenshots/timeline-language/comparison-jianying-fcp.png)：三张原图完整保留，比较画布只添加标签和说明。
- [剪映参考图](../references/剪映专业版.png)：用户提供的项目参考。
- [Final Cut Pro 官方参考图](../references/final-cut-pro-mac-official.png)：Apple 用户指南截图。

KaiCut 空项目中的缩略条和波形是 `aria-hidden` 的视觉占位，仅用于验收时间线视觉语言，不代表已经创建媒体片段，也不参与选择、播放、保存或导出。

## 自检

| 项目 | 结果 |
| --- | --- |
| 轨道头96px，仅类型图标、锁定、隐藏 | 通过 |
| 独立时间线工具行36px | 通过 |
| 撤销、重做、分割、删除、复制、音量、速度图标 | 通过；均为 Lucide 16px / stroke 1.6 的禁用占位 |
| 工具提示 | 已实现；工具按钮外层提供悬停提示，避免 disabled button 吞掉 hover。当前 Windows 捕获过程中窗口焦点切换，未把悬停状态作为截图证据 |
| 视频缩略图条 | 通过；低饱和蓝灰的视觉占位 |
| 音频波形 | 通过；蓝灰类型色填充的视觉占位 |
| 单一蓝色强调 | 通过；没有绿色或多彩运营色，保留导出按钮蓝色主操作 |
| 播放头 | 通过；标尺和轨道共享1px白线，不再显示三角 |
| 标尺密度 | 通过；1秒次刻度、5秒主刻度，标尺行24px |
| 一级导航、会员入口、运营内容 | 未添加 |
| 业务逻辑 | 未改；只触碰时间线 JSX、视觉 token 与 CSS |

## 验证

- `pnpm lint`：通过
- `pnpm test`：26 项通过
- `pnpm build`：通过
- `pnpm tauri build --no-bundle --debug`：通过
- Windows 原生窗口已重新启动并截图；macOS 实机验收待设备
