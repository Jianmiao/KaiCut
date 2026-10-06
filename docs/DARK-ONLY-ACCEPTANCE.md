# 深色视觉收敛验收 · 2026-10-06

本轮仅执行指定视觉调整和深色主题策略。项目新开存、导入、媒体引擎、剪辑与导出行为不变。macOS 实机验收在获得设备后单独进行。

## 结果

| 要求 | 本轮结果 |
| --- | --- |
| 蓝色重点收敛 | K 字标使用 `--kc-color-text-primary`，仅hover蓝；导入与普通弹窗确认均为secondary，只有导出为蓝色实心主操作。系统应用图标也改为中性底色。焦点环与复选框选中仍保留辅助状态色。 |
| 区域颜色 | 侧栏#242426、查看器#1C1C1E、检查器#242426、时间线#252527。原生透明侧栏以该基色85%叠加，实际像素可能受背后内容影响。 |
| 分隔线 | 面板与轨道边界均1px `rgba(255,255,255,0.06)`。原生截图可辨认查看器/侧栏/检查器/时间线边界，不靠重阴影分割。 |
| 时间线 | 轨道头128px；所有轨道42px行高；空轨道与头部同为#252527；刻度改1px白色8%细线，保留等宽时间码。 |
| 播放头 | 头部改为向下三角，红色加白色1px描边，与红色竖线对齐。 |
| 查看器元信息 | “查看器”及1080p/适合窗口为11px tertiary、常规字重；不增加控件或功能。 |
| 深色单主题 | 移除主题选择和系统主题监听；HTML与Tauri启动均为深色。旧偏好中的light/system被丢弃，但保留透明度和减少动态效果设置。 |

## 真实截图与对比

- [KaiCut 深色原生窗口原图](./screenshots/dark-only-refinement/native-dark.jpg)：直接保存 Windows Tauri 窗口捕获 JPEG，1283×851，包含系统边框，没有裁剪/重绘。蓝色鼠标光圈是桌面指针效果。
- [与 Final Cut Pro Mac 官方图并排](./screenshots/dark-only-refinement/comparison-final-cut-pro.png)：左为上述完整原图，右为 Apple 官方指南截图，分别保持原始比例/像素，无虚构素材加入 KaiCut。

右图来源：[Final Cut Pro Mac 用户指南](https://support.apple.com/guide/final-cut-pro/welcome/mac)，页面图像说明为 “The Final Cut Pro window showing the browser, viewer, and timeline”。[原图URL](https://help.apple.com/assets/6A29F5537442EFE69C0CD4F3/6A29F558FEEBDB01270C8EF2/en_US/15436cc290789a42506a7394ad5362fc.png)，1304×738；本地参考位于 `docs/references/final-cut-pro-mac-official.png`。该 Apple 参考图仅作设计比较，不纳入应用发布资产，不能用项目 Apache 许可证重新授权。

对比结论：KaiCut 的重点已收敛为导出，灰阶分区与时间线密度更接近专业工具；仍是空壳状态，而参考图包含真实媒体与密集剪辑内容，所以不能宣称整体信息密度或功能体验已一致。原有“剪映专业版.png”为Windows截图，本轮不将它标注成Mac版。最终macOS窗口、字体渲染与原生材质须另验收。

## 文件清单

- `src/styles/tokens.css`、`globals.css`、`toolbar.css`：单深色、颜色层次、重点与间距。
- `src/components/editor/media-bin.tsx`、`timeline.tsx`、`src/components/ui/dialog.tsx`：次要按钮、三角标记与中性弹窗确认。
- `src/App.tsx`、`src/hooks/use-preferences.ts`、`src/stores/preferences-store.ts`、`index.html`、`src-tauri/tauri.conf.json`：移除主题选择，固定深色及兼容旧偏好。
- `src/hooks/use-preferences.test.tsx`、新增 `src/stores/preferences-store.test.ts`：深色请求与旧浅色偏好迁移验证。
- `public/kaicut.svg`、`src-tauri/icons/`：中性系统图标源与生成图标。
- `README.md`、`docs/DESIGN.md`、`DESIGN-TOKENS.md`、`UI-CHEATSHEET.md`、本记录与截图/参考图。

## 检查结果

- `pnpm lint` 通过。
- `pnpm test` 26项通过，包含旧light偏好被丢弃且辅助偏好保留。
- `pnpm build` 通过。
- `pnpm tauri build --no-bundle --debug` Windows构建通过。
- 原生窗口启动、深色外观、设置面板仅保留辅助偏好、四分区和蓝色重点已目视检查；Acrylic日志仍显示Native，原生材质实现未改动。
- macOS实机验收：待设备，不列为已通过。

事实说明：本项目选择仅深色，不基于“所有专业软件只有深色”的笼统判断。此前对 Resolve 的例子不准确：其官方21.1手册第110页描述的是蓝灰/中性灰界面选项，不是完整浅/深主题切换。[官方手册](https://documents.blackmagicdesign.com/UserManuals/DaVinci_Resolve_21.1_Reference_Manual.pdf?_v=1788850811000#page=110)。
