# 原生视觉修订验收 · 2026-10-06

范围：用户列出的18项视觉要求。没有改变项目文件格式、新开存/导入逻辑、时间线编辑能力、FFmpeg接口或导出功能。原生外观命令只连接既有主题/透明度设置，不增加业务功能。

## 原生材质诊断

修复前的可重复检查结果：窗口 `transparent=false`、没有 `windowEffects`、Rust没有原生材质设置，HTML根与app-shell不透明。上一轮实机CSS blur确实生效，但只模糊均匀实底，所以不是窗口背板效果。

修复链路：透明 Tauri窗口 → 透明 WebView底色 → 原生系统材质 → 原生API结果确认 → 仅清空控制区祖先背景 → 工具栏/侧栏薄染色；预览/检查器/时间线继续实色。

- Windows11 build22621+：直接使用 window-vibrancy0.8.1 Acrylic，DWM属性读回确认为 transient backdrop；不用Mica替代真实下层采样。
- macOS：Sidebar NSVisualEffectView + BehindWindow，随窗口激活状态变化；读取系统减少透明度。此平台未实机验证。
- 关闭透明度/系统高对比度/系统禁用效果/不支持平台：返回solid，恢复实色窗口与网页背景。
- 同主题/有效透明状态缓存，焦点恢复时检查系统偏好但不重复重建材质。失败不缓存为成功。
- 原生模糊由系统控制，没有把CSS14px误称为系统vibrancy参数；菜单仍用原有CSS预算。

实测将临时蓝/白/橙色块图放在独立画图窗口，KaiCut置于前面后侧栏出现对应透色，主要编辑区不受影响。诊断截图 [diagnostic-native-backdrop.jpg](./screenshots/native-visual-fix/diagnostic-native-backdrop.jpg) 是原生窗口捕获，色块没有植入应用。背板随后关闭，四张交付图在普通桌面环境下重新截取。

## 逐项自检

| 项目 | 结果 |
| --- | --- |
| 浅色背景/面板 | canvas #F5F5F7、content #FFFFFF；原生玻璃区域是明确例外，会随背后真实内容改变。 |
| 深色三档 | canvas #1C1C1E、content #2C2C2E、content-muted #3A3A3C。 |
| 区域层次 | 素材侧栏原生材质；检查器面板实色；预览中性深底；时间线独立深色。布局默认240/280/52按已认可规范同步。 |
| 预览去卡片 | canvas边框0、外padding0、阴影none、圆角0；保持16:9约束避免未来拉伸画面。 |
| 预览包围 | 使用 preview-surround #202124 / #101012；没有装饰性背景或染色。 |
| 预览空态 | 较小24px线性图标、12/11px中灰文案；无大标题/外框。 |
| 时间线底色 | 两种主题均使用 timeline-canvas #1C1C1E。 |
| 轨道分隔 | 轨道头#2C2C2E，时间线#1C1C1E；横向与纵向边界1px #48484A。 |
| 刻度与时间码 | 网格#535356、刻度文字#C7C7CC；时间码、标尺、序列时长用系统等宽。 |
| 播放头 | 红色竖线两侧白边1px；头部红色SVG轮廓配白色stroke1。它是时间标记，不是操作图标。 |
| 工具栏/侧栏透明 | Windows实际原生材质已验证，背板测试确认透色；关闭后实色恢复已验证。 |
| 排查顺序 | 先发现不透明窗口/缺少原生材质/祖先实色，再接通链路；随后用实机结果调整可读性染色，没有盲目增加blur。 |
| 未保存 | 使用 text-tertiary；浅色原生区域局部值#505058提高最暗背板下可读性，深色#B0B0B8。 |
| 右侧图标 | 快捷键/布局均16px，stroke1.6；行为与原有版本一致。 |
| 导出间距 | gap引用 space-2=8px，左右padding引用space-3=12px，按钮圆角6px、高32px。 |
| K标记 | 从22px实心字形改18px单线字形，stroke1.6，无新增底板。系统标题栏应用图标不改。 |
| 状态栏 | 12px主文字色#1D1D1F/#F5F5F7，对比度高于原版辅助文字。 |
| 图标一致性 | 应用lucide统一线条/stroke1.6，播放从实心改线框；选中标记保留可访问状态。 |

## 四张真实截图

均来自最终 `src-tauri/target/debug/kaicut.exe` 原生窗口，1283×851，保留系统边框/标题栏/底部分隔线。直接保存捕获字节，不拼图、不裁切、不重绘。蓝色鼠标光圈来自桌面指针效果，不是应用材质。

1. [浅色](./screenshots/native-visual-fix/01-light.jpg)
2. [深色](./screenshots/native-visual-fix/02-dark.jpg)
3. [导出悬停](./screenshots/native-visual-fix/03-export-hover.jpg)
4. [菜单键盘焦点](./screenshots/native-visual-fix/04-menu-focus.jpg)

审美方向按专业桌面编辑器的分区、密度、原生材质和内容优先执行。未提供 Final Cut Pro/剪映Mac版实机同平台对照，因此不声称“已达到像素一致”或“macOS验收已通过”。

## 修改文件

- 样式：`src/styles/tokens.css`、`globals.css`、`toolbar.css`。
- 视觉标记：`src/components/editor/toolbar.tsx`、`preview-area.tsx`、`timeline.tsx`。
- 原生外观桥：`src/hooks/use-preferences.ts`、新增 `use-preferences.test.tsx`。
- 原生材质：新增 `src-tauri/src/appearance.rs`，修改 `src-tauri/src/lib.rs`、`Cargo.toml`、`Cargo.lock`、`tauri.conf.json`，新增 `APPEARANCE.md`。
- 说明同步：`README.md`、`docs/DESIGN.md`、`DESIGN-TOKENS.md`、`UI-CHEATSHEET.md`、本验收记录和截图。

文件哈希核对确认 `project-store.ts`、`kaicut-format.ts`、Rust `project.rs`、`commands.rs`、`media_engine.rs` 未修改。没有增加AI、云服务、素材业务或剪辑功能。

## 检查结果

- `pnpm lint` 通过。
- `pnpm test` 25项通过；新增3项覆盖原生确认前不透明、失败降级、忽略过时响应。
- `pnpm build` 通过。
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` 通过。
- `cargo test --manifest-path src-tauri/Cargo.toml --locked` 5项通过。
- `pnpm tauri build --no-bundle --debug` Windows构建通过。
- Windows实际主题切换、native→solid→native透明度切换、导出hover、Tab/Shift+Tab菜单焦点通过。
- 日志确认 `theme=Light/Dark, material=Native`，关闭开关后 `material=Solid`；同状态聚焦不再重复材质重建日志。

macOS编译/签名和视觉实机仍未验收。旧Windows与Linux采用实色降级，不宣称原生Acrylic可用。系统可能在省电/远程会话中自行减弱效果；不修改用户系统设置。
