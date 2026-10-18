# 时间线视觉与交互修订验收

日期：2026-10-11

本轮在既有视觉语言上补充运行时多选、同轨范围选择、批量删除和按轨道波纹删除；选择集合不写入 `tracks[].clips`，不改变 `.kaicut` schema。

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
| 撤销、重做、分割、删除、复制、音频混音、速度图标 | 通过；均为 Lucide 16px / stroke 1.6，音频混音可编辑轨道音量与片段淡入淡出，速度仍为禁用占位 |
| 工具提示 | 已实现；工具按钮外层提供悬停提示，避免 disabled button 吞掉 hover。当前 Windows 捕获过程中窗口焦点切换，未把悬停状态作为截图证据 |
| 视频缩略图条 | 通过；低饱和蓝灰的视觉占位 |
| 音频波形 | 通过；蓝灰类型色填充的视觉占位 |
| 单一蓝色强调 | 通过；没有绿色或多彩运营色，保留导出按钮蓝色主操作 |
| 播放头 | 通过；标尺和轨道共享1px白线，不再显示三角 |
| 标尺密度 | 通过；1秒次刻度、5秒主刻度，标尺行24px |
| 主轨磁吸 | 通过；固定 `video-1` 为运行时参考，视频片段拖动时可吸附其边界，不写入项目且不联动其他轨道 |
| 成组拖动 | 通过；同类型多选片段保持相对位置，通过一次 `moveMany` 提交；混合音视频多选不拖动 |
| 一级导航、会员入口、运营内容 | 未添加 |
| 业务逻辑 | 已补充运行时选择与批量编辑；Rust 对锁轨、重叠、边界和历史保持原子校验 |

## 验证

- `pnpm lint`：通过
- `pnpm test`：120 项通过
- `cargo test --locked`：38 项通过，1 项 sidecar 条件测试忽略
- `pnpm build`：通过
- `pnpm tauri build --no-bundle --debug`：通过
- Windows 原生窗口已重新启动并截图；macOS 实机验收待设备
