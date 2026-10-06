# 顶部栏 Tauri 实机截图与自检

日期：2026-10-06。模式：`apple-web-design audit`，本轮不修改应用源码，不继续制作素材区。

## 截图来源

运行程序：`src-tauri/target/debug/kaicut.exe`，构建时间 2026-10-06 12:10。Windows 原生窗口标题为“ KaiCut / 开剪”，内容地址由 UI Automation 确认为 `http://tauri.localhost/`，并非 Vite 页面或浏览器模拟。

四张截图均由 Windows 窗口捕获取得，1283×851，直接保存返回的原始 JPEG 字节；没有裁剪、拼接、缩放或重绘。保留原生标题栏、最小化/最大化/关闭按钮、左右/底部窗口边框和工具栏下边线。鼠标指针与周围蓝色光圈是捕获时已有的桌面指针效果，不是 KaiCut 按钮样式，也没有后期去除。

测试项目名称“顶部栏验收”通过真实重命名操作设置，用于触发“未保存”；未写入用户项目文件。

1. [浅色模式](./screenshots/tauri-toolbar/01-light.jpg)
2. [深色模式](./screenshots/tauri-toolbar/02-dark.jpg)
3. [导出按钮悬停（深色）](./screenshots/tauri-toolbar/03-export-hover.jpg)：实际鼠标移入，未点击导出、未强制 CSS 伪状态。
4. [菜单按钮键盘焦点（深色）](./screenshots/tauri-toolbar/04-menu-keyboard-focus.jpg)：通过 Tab / Shift+Tab 导航形成可见焦点环，鼠标已移开；菜单保持关闭。

## 自检问题列表

| 结论 | 检查项 | 实际结果与依据 | 建议 |
| --- | --- | --- | --- |
| 参数通过，视觉效果有限 | 毛玻璃 | 真实 Tauri WebView 的“已计算”面板显示 `backdrop-filter: blur(14px) saturate(1.6)`，背景 `color(srgb 0.188235 0.188235 0.203922 / 0.9)`，即深色 `#303034` / 90% 不透明度。高度为 52px。与 DESIGN-TOKENS 一致。工具栏处在独立网格行，后方是实色，几乎没有可供模糊的纹理；不等于桌面壁纸透过窗口，也不是原生 Liquid Glass。 | 保持当前预算；不要通过加大 blur 制造效果。如要明显透过内容，需要另行定义真实的背景/叠层关系。 |
| 有分隔线，缩放需复核 | 底部 1px | 源码 `toolbar.css:56` 是 `1px solid var(--kc-color-divider)`；实机已计算颜色 `rgb(58,58,60)`、solid，宽度却是 `0.666667px`。截图有细线，说明运行时缩放/像素取整与源码字面值存在差异。 | 若要求严格 1 CSS px，后续检查 WebView 缩放和显示 DPI。当前不能把源码 1px 直接当作各设备实测 1px。 |
| 不符本次检查要求 | 未保存颜色 | `toolbar.css:90` 使用 `--kc-color-text-secondary`，不是 tertiary。深色实测 `rgb(199,199,204)` / `#C7C7CC`；tertiary 目标为 `#B0B0B8`。 | 改用 `--kc-color-text-tertiary`；此次保留现状以呈现真实问题。 |
| 功能清楚，尺寸有偏差 | 右侧两个图标 | `toolbar.tsx:63–64`：Keyboard 打开 N/O/S 快捷键说明；PanelsTopLeft 打开素材库/检查器显隐及恢复布局菜单。两者 `size=20`、`strokeWidth=1.6`。描边一致，但常规图标 token 是16px。 | 是否作为工具栏20px特例需明确；否则收敛至16px。静态图标不能用于推断剪映对应按钮的真实功能。 |
| 部分通过 | 导出按钮 | 实机已计算：高度32px，四角6px，内边距上下0/左右12px，column-gap/row-gap均6px。背景 `#0066CC`、白字；hover 源码为 `#005CBA`，实际鼠标悬停截图已取得。导出图标16px，stroke1.8。 | 圆角、高度、左右内边距符合规范；图标/文字间距应由6px改为8px。与右侧图标的1.6描边有轻微重量差，可统一。 |
| 视觉建议 | K logo | 系统标题栏左上角是小蓝色方块应用图标，尺寸克制；应用工具栏内是22×22 SVG实心 K 加“开剪”，没有方块底板。实心笔画比右侧1.6线性图标重，左端视觉重量略高，但没有压住项目名或导出。 | 保留系统图标；工具栏字标可缩至18–20px或减轻形状填充，不新增背景方块/阴影。 |
| 额外发现 | 系统标题栏与主题 | Windows 原生标题栏在应用切为浅色后仍保持深色，四张图均保留这一真实表现。 | 后续决定是否将 Tauri window theme 与应用主题同步；此项不是本轮通过调色消除的问题。 |

另外，菜单左右内边距11px、品牌图标与文字间距5px仍未收敛至4px网格；本轮不顺手改写。

## 证据与限制

运行时样式来自 Tauri 自身 WebView2 的开发者工具 Elements → Computed 面板，通过只读 DOM 元素选择读取，不修改样式或应用状态。文本证据保存在同目录：

- [工具栏计算样式](./screenshots/tauri-toolbar/runtime-toolbar-computed.txt)
- [导出按钮计算样式](./screenshots/tauri-toolbar/runtime-export-computed.txt)
- [未保存状态计算样式](./screenshots/tauri-toolbar/runtime-status-computed.txt)

原生窗口内容已确认使用生产资产 `index-BgiUhRVu.css`。悬停颜色不进行基于鼠标光圈覆盖区域的精确像素取样；计算值和源码值在报告中分别标明。未进行 macOS 实机或其他显示器 DPI 验证。

审查结论：**Refine**。结构已认可；建议先修正状态文字、按钮间距与图标规格，再决定 logo 微调和系统标题栏主题。素材区仍未进入实现阶段。
