# KaiCut Design Tokens v0.2

状态：2026-10-06 高密度修订后的唯一数值规范，与 [DESIGN.md](./DESIGN.md) 配套；单页速查见 [UI-CHEATSHEET.md](./UI-CHEATSHEET.md)。本轮收紧工具栏、轨道和文字尺寸，明确四区边界与浮动播放控制条；历史初值不构成第二套规范。

本轮仅保留深色，取消先前的浅色规范。K默认主文字色、hover蓝；系统图标改中性底色。只有导出为蓝色实心操作。

## 使用原则

- 所有 CSS token 使用 `--kc-` 前缀，优先映射到现有 shadcn-macos 组件，不建立平行设计系统。
- 预览画面周围、时间线、波形、检查器、表单和对话框实色；工具栏与素材侧边栏可用原生毛玻璃，菜单和短小弹出层可用 CSS 毛玻璃。预览底部控制条仅使用半透明背景，不添加模糊。
- 颜色不能作为唯一状态线索。固定深色必须验证实际合成对比度；系统高对比度模式优先使用系统颜色。
- 下表即当前规范，不再保留待确认的另一套品牌色、圆角或布局尺寸。

## 颜色

**仅导出使用 `#0066CC` Accent + `#FFFFFF` 按钮文字**，导入与弹窗确认均为次要样式。悬停和按下是这套强调色的状态值，文字仍为白色。默认、悬停、按下的纯色文字对比度分别约为 5.57:1、6.48:1、8.04:1；最终仍需检查实际合成背景。

| Token | 深色唯一值 | 用途 |
| --- | --- | --- |
| `--kc-color-canvas` | `#1C1C1E` | 应用背景 |
| `--kc-color-content` | `#2C2C2E` | 主要内容实色表面 |
| `--kc-color-content-muted` | `#3A3A3C` | 次级内容表面 |
| `--kc-color-sidebar` | `#2A2A2C` | 侧栏实色基值；native下会随背景合成 |
| `--kc-color-inspector` | `#2A2A2C` | 检查器实色背景 |
| `--kc-color-viewer` | `#1A1A1C` | 查看器容器，区别于画面周围 |
| `--kc-color-toolbar-fallback` | `#3A3A3C` | 工具栏及毛玻璃实色回退 |
| `--kc-color-text-primary` | `#F5F5F7` | 主文字 |
| `--kc-color-text-secondary` | `#C7C7CC` | 次要文字 |
| `--kc-color-text-tertiary` | `#B0B0B8` | 辅助文字，保持可读性 |
| `--kc-color-text-on-accent` | `#FFFFFF` | 所有 accent 按钮文字 |
| `--kc-color-border` | `#48484A` | 表面装饰性边界 |
| `--kc-color-divider` | `rgba(255, 255, 255, 0.10)` | 四区1px分隔线 |
| `--kc-color-focus` | `#70A7FF` | 键盘焦点轮廓，不是另一套 Accent |
| `--kc-color-accent` | `#0066CC` | 主操作填充 |
| `--kc-color-accent-hover` | `#005CBA` | Accent 悬停 |
| `--kc-color-accent-pressed` | `#004F9E` | Accent 按下 |
| `--kc-color-selection` | `#414146` | 列表/片段选择背景，搭配主文字 |
| `--kc-color-selection-strong` | `#505058` | 当前选择强调，搭配主文字 |
| `--kc-color-success` | `#71CC89` | 已保存、完成等状态前景 |
| `--kc-color-warning` | `#F0C067` | 注意状态前景 |
| `--kc-color-danger` | `#FF8A91` | 错误/危险状态前景 |
| `--kc-color-window-close-hover` | `#C42B1C` | Windows 关闭按钮悬停；仅限窗口关闭操作 |
| `--kc-color-info` | `#70A7FF` | 提示状态前景 |
| `--kc-color-disabled` | `#8E8E93` | 真正禁用控件的辅助显示 |
| `--kc-color-timeline-canvas` | `#202022` | 时间线固定深色实底，空轨道同色 |
| `--kc-color-timeline-bar` | `#2C2C2E` | 时间线工具/状态栏 |
| `--kc-color-preview-surround` | `#0E0E10` | 画面周围近黑实底 |
| `--kc-color-preview-controls` | `rgb(26 26 28 / 82%)` | 预览底部浮动控制条，无模糊、不占独立行；减少透明度时用 viewer 实色 |
| `--kc-color-waveform` | `#70A7FF` | 波形 |
| `--kc-color-playhead` | `#FFFFFF` | 当前时间位置的1px细线；不使用三角或红色填充 |
| `--kc-color-timeline-grid` | `rgba(255, 255, 255, 0.06)` | 历史token名称，仅用于顶部标尺刻度；轨道画布不绘制网格 |
| `--kc-color-track-header` | `#202022` | 与空轨道、时间线实底同色 |
| `--kc-color-clip-video` | `#3D72B8` | 视频片段填充 |
| `--kc-color-clip-audio` | `#2F5E70` | 音频片段填充；低饱和蓝灰，与视频保持同一色系 |
| `--kc-color-clip-selected` | `#70A7FF` | 片段选择边界，不能仅靠颜色 |
| `--kc-color-clip-offline` | `#D89B32` | 离线片段标记 |
| `--kc-color-clip-muted` | `#636366` | 静音片段标记 |
| `--kc-color-clip-locked` | `#48484A` | 锁定片段标记 |

Accent 是按钮填充色，不能在深色底上直接当正文文字色。文字链接用主文字色并加下划线。状态色默认是前景，不代表可直接用于带白字的实心按钮。片段填充与文字须逐组验证；上表不是所有组合已达标的声明。

普通文字 ≥ 4.5:1，大字与识别控件所必需的非文本信息 ≥ 3:1。低对比分隔线仅用于装饰；必要控件边界、选择和焦点使用足够对比的轮廓或形状。disabled 色不能用于仍可操作的控件或关键说明。

## 字体

| Token | 唯一值 | 用途 |
| --- | --- | --- |
| `--kc-font-sans-macos` | `-apple-system, BlinkMacSystemFont, system-ui` | 系统为 macOS 中文自动选择苹方 |
| `--kc-font-sans-windows` | `"Segoe UI", "Microsoft YaHei UI", system-ui` | 拉丁字符 Segoe UI，中文微软雅黑 UI |
| `--kc-font-sans` | 按平台引用上面对应 token；未知平台 `system-ui, sans-serif` | 所有界面共用字体入口 |
| `--kc-font-mono` | `ui-monospace, "Cascadia Mono", Consolas, monospace` | 时间码、标尺和数值 |
| `--kc-font-size-title` | `24px` | 项目中心标题，与内容对齐 |
| `--kc-font-size-panel` | `12px` | 面板标题 |
| `--kc-font-size-body` | `13px` | 常规操作、表单和列表 |
| `--kc-font-size-caption` | `12px` | 次要信息 |
| `--kc-font-size-timecode` | `12px` | 时间码 |
| `--kc-line-height-title` | `32px` | 项目中心标题 |
| `--kc-line-height-panel` | `20px` | 面板标题 |
| `--kc-line-height-body` | `20px` | 常规文本 |
| `--kc-line-height-caption` | `18px` | 次要信息 |
| `--kc-line-height-timecode` | `20px` | 时间码 |
| `--kc-font-weight-regular` | `400` | 正文 |
| `--kc-font-weight-medium` | `500` | 操作标签 |
| `--kc-font-weight-semibold` | `600` | 标题与关键标签 |

不显式优先指定 Apple 字族，不引入用户字体包、字体别名或自定义字体加载规则。Windows 不请求 Apple 字体。Tailwind 的 `font-sans` / `font-mono` 映射到以上项目 token，`text-sm` 映射到正文13px / 20px，防止组件保留旧正文尺寸。时间码使用 `tabular-nums`。在系统缩放与大字号下检查截断和重排。

## 间距与尺寸

| Token | 唯一值 | 用途 |
| --- | ---: | --- |
| `--kc-space-1` | `4px` | 最小间距 |
| `--kc-space-2` | `8px` | 图标与文字、同组控件 |
| `--kc-space-3` | `12px` | 紧凑内边距 |
| `--kc-space-4` | `16px` | 面板内边距 |
| `--kc-space-6` | `24px` | 功能组之间 |
| `--kc-space-8` | `32px` | 页面分区 |
| `--kc-space-10` | `40px` | 大区块间距 |
| `--kc-space-12` | `48px` | 页面级分区 |
| `--kc-space-16` | `64px` | 项目中心留白 |
| `--kc-toolbar-height` | `44px` | 唯一默认工具栏高度 |
| `--kc-window-control-width` | `44px` | Windows 一体化标题栏每个窗口按钮宽度，高度随44px顶部栏 |
| `--kc-track-height` | `34px` | 统一轨道行高 |
| `--kc-track-header-width` | `96px` | 统一轨道头宽度；仅保留类型图标、锁定和隐藏 |
| `--kc-timeline-tool-row-height` | `36px` | 时间线独立工具行 |
| `--kc-timeline-ruler-row-height` | `24px` | 紧凑时间标尺行 |
| `--kc-sidebar-width` | `320px` | 剪映式三分区默认素材区宽度 |
| `--kc-inspector-width` | `300px` | 唯一默认检查器宽度 |
| `--kc-sidebar-width-range` | `180–460px` | 素材区拖拽范围；空间不足时为播放器保留300px |
| `--kc-inspector-width-range` | `220–380px` | 检查器拖拽范围 |
| 工作台外边距/区域间距 | `var(--kc-space-2)` / `8px` | 与面板分隔器命中宽度统一 |
| 时间线默认高度 | `300px` | 可调220–520px，代码常量 DEFAULT_WORKSPACE.timeline |
| `--kc-panel-padding` | `var(--kc-space-4)` | 面板内边距 |
| `--kc-control-height-compact` | `32px` | 常规桌面控件高度 |
| `--kc-control-height-comfortable` | `40px` | 大字号/舒适密度控件高度 |
| `--kc-target-size-touch` | `44px` | 触控命中区域 |
| `--kc-target-size-minimum` | `24px` | 指针目标最低验收基线 |
| `--kc-icon-size` | `16px` | 常规图标 |
| `--kc-icon-size-small` | `14px` | 辅助图标 |
| `--kc-hit-area-icon` | `32px` | 桌面图标按钮命中区域 |

4px 基础网格，常用 4 / 8 / 12 / 16 / 24 / 32px；40 / 48 / 64px 仅用于大区块。侧面板可调整/折叠，恢复布局始终回到本表默认值。响应式用折叠和重排处理空间不足，不另立默认宽度；不同控件密度和触控目标不改变工具栏/面板的唯一默认规格。命中区域不得互相遮挡。粗指针触控模式下轨道行最小44px，以容纳触控目标，桌面默认仍为34px；播放控制条内部重排。

空态图标/文字：素材区32px / 13px，查看器18px / 12px，检查器20px / 12px。查看器内边距0，播放控制条浮在底部，不占独立行。播放头为1px白色细线，不使用三角或红色填充。时间线工具行使用16px、stroke1.6的Lucide线性图标，并提供悬停提示；轨道头只保留类型图标、锁定和隐藏。这些是相应展示组件的固定规格，不改变普通操作图标和命中区域。

## 圆角与层级

| Token | 唯一值 | 用途 |
| --- | --- | --- |
| `--kc-radius-control` | `6px` | 按钮、输入框、选择器 |
| `--kc-radius-surface` | `10px` | 面板、卡片、属性分组 |
| `--kc-radius-popover` | `10px` | 菜单、短小弹出层 |
| `--kc-radius-dialog` | `12px` | 对话框 |
| `--kc-radius-thumbnail` | `var(--kc-radius-control)` | 缩略图复用控件圆角 |
| `--kc-border-width` | `1px` | 常规边界 |
| `--kc-focus-ring-width` | `2px` | 键盘焦点轮廓 |
| `--kc-focus-ring-offset` | `2px` | 焦点轮廓间距 |
| `--kc-opacity-disabled` | `0.50` | 真正禁用控件的辅助透明度 |
| `--kc-opacity-hover` | `0.08` | 普通控件悬停叠加层 |
| `--kc-opacity-pressed` | `0.14` | 普通控件按下叠加层 |
| `--kc-opacity-scrim` | `0.35` | 模态背景压暗 |
| `--kc-shadow-popover` | `0 4px 12px rgb(0 0 0 / 10%)` | 菜单轻阴影 |
| `--kc-shadow-dialog` | `0 8px 24px rgb(0 0 0 / 12%)` | 对话框轻阴影 |

只使用上述四档圆角；缩略图是别名，普通按钮不另设胶囊圆角。浮层和工具栏可使用指定轻阴影，内容面板不用装饰阴影，不叠加成重阴影。必要边界不能依靠阴影表示。Accent 状态直接使用颜色表，不叠加 hover/pressed 透明层改变已选配色。

## 材质与模糊

原生材质扩展：`--kc-native-glass-tint` 固定 `rgb(42 42 44 / 85%)`，原生半径由系统管理。时间线主/次/辅助文字为 #F5F5F7 / #C7C7CC / #B0B0B8；空轨道与轨道头统一 #202022，头宽96px、行高34px、工具行36px、播放头1px白线。空项目不显示伪片段或假波形；媒体库中的首帧与图片必须来自真实解码。

原生材质 API 失败、系统透明效果关闭、高对比度或应用关闭透明度时，根背景与控制层回到实色；只有确认 native 成功才移除不透明祖先背景。窗口主题同步，重复聚焦不会重复重建相同材质。Windows Acrylic 与 macOS Sidebar 材质各自验证，不把其模糊半径冒称为CSS14px。

| Token | 唯一初值 | 用途 |
| --- | --- | --- |
| `--kc-glass-opacity` | `0.90` | 工具栏、菜单、短小弹出层背景不透明度 |
| `--kc-glass-opacity-strong` | `0.94` | 文字较多的短小弹出层 |
| `--kc-glass-opacity-clear` | `0.88` | 经对比度验证的紧凑控制层 |
| `--kc-glass-blur` | `14px` | 默认模糊 |
| `--kc-glass-blur-strong` | `16px` | 较密实的菜单模糊 |
| `--kc-glass-blur-none` | `0px` | 关闭模糊 |
| `--kc-glass-saturation` | `160%` | 材质饱和度 |
| `--kc-glass-border` | `rgb(255 255 255 / 14%)` | 装饰边缘 |
| `--kc-glass-highlight` | `rgb(255 255 255 / 6%)` | 克制的定向高光 |
| `--kc-glass-shadow` | `0 1px 3px rgb(0 0 0 / 3%)` | 工具栏轻层级 |
| `--kc-glass-fallback` | `var(--kc-color-toolbar-fallback)` | 不支持/关闭透明度时的实色 |

毛玻璃作用于背景层，不降低文字/图标的不透明度。低性能、减少透明度、强制颜色或不支持模糊时使用实色；兼顾 `backdrop-filter` 与 Safari 前缀。禁止全屏模糊、多层玻璃叠加、在视频画面和时间线上制造装饰性染色。

## 动效

| Token | 唯一值 | 用途 |
| --- | --- | --- |
| `--kc-duration-press` | `120ms` | 按下、悬停和状态反馈 |
| `--kc-duration-popover` | `160ms` | 菜单、短小弹出层 |
| `--kc-duration-dialog` | `220ms` | 对话框、检查器展开 |
| `--kc-duration-reduced` | `100ms` | 减少动态效果时颜色/透明度反馈的上限 |
| `--kc-duration-editing` | `0ms` | 剪辑、逐帧、撤销、拖拽与播放头的额外过渡 |
| `--kc-ease-out-ui` | `cubic-bezier(0.23, 1, 0.32, 1)` | 普通 UI 进入/退出 |
| `--kc-ease-in-out-ui` | `cubic-bezier(0.77, 0, 0.175, 1)` | 两个可见位置之间 |
| `--kc-ease-drawer-ui` | `cubic-bezier(0.32, 0.72, 0, 1)` | 抽屉 |
| `--kc-ease-spring-ui` | `cubic-bezier(0.22, 1.08, 0.36, 1)` | 原生感的短促回弹，仅用于低频弹层 |
| `--kc-ease-linear` | `linear` | 恒速显示 |

减少动态效果时移除位移、缩放和弹性，仅保留必要的短反馈。高频编辑不能等待动画；不连续动画化 blur、不使用 `transition: all`。这些是项目默认值，不是 Apple 官方定值。

## 偏好与降级

这些是应用配置项，不另建 CSS 配置 token：主题固定深色；旧浅色/系统主题偏好被忽略，只迁移透明度和减少动态效果设置；透明度可单独关闭；减少动态效果可单独开启。系统或应用任何一方要求减少效果时，采用减少效果的结果。低性能模式关闭玻璃和装饰阴影；系统偏好不可读取时仍能手动控制。

## 验收边界

字体、Accent、四档圆角、工具栏及侧面板默认尺寸已确定。当前用真实 Windows Tauri 窗口验证密度，并与 Final Cut Pro Mac 官方参考图并排对比；macOS 实机验收待设备。后续继续验证系统缩放、固定深色、合成背景对比度、键盘/辅助技术与视频负载性能；本次文档统一不等于源码已完成同步或实机验收。
