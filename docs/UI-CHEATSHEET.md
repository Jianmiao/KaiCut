# KaiCut UI 一页速查

**毛玻璃使用范围**

工具栏和素材侧边栏用原生材质，菜单/短小弹出层用 CSS 材质。检查器、表单、对话框、预览画面周围、时间线均实色。播放控制条仅半透明浮于查看器底部，不占独立行、不加模糊，减少透明度时回退 viewer 实色。菜单默认14px blur / 90%背景不透明度；原生层叠加 `rgb(42 42 44 / 85%)` 染色，系统管理模糊；关闭或失败即实色回退。

**颜色 token 列表**

固定深色。下列名称统一加 `--kc-color-` 前缀；完整数值以 DESIGN-TOKENS.md 为准。

| Token | 深色 | Token | 深色 |
| --- | --- | --- | --- |
| `accent` | `#0066CC` | `text-on-accent` | `#FFFFFF` |
| `accent-hover` | `#005CBA` | `accent-pressed` | `#004F9E` |
| `canvas` | `#1C1C1E` | `content` | `#2C2C2E` |
| `content-muted` | `#3A3A3C` | `sidebar` | `#2A2A2C` |
| `inspector` | `#2A2A2C` | `viewer` | `#1A1A1C` |
| `toolbar-fallback` | `#3A3A3C` | `text-primary` | `#F5F5F7` |
| `text-secondary` | `#C7C7CC` | `text-tertiary` | `#B0B0B8` |
| `border` | `#48484A` | `divider` | `rgba(255, 255, 255, 0.10)` |
| `focus` | `#70A7FF` | `info` | `#70A7FF` |
| `selection` | `#414146` | `selection-strong` | `#505058` |
| `success` | `#71CC89` | `warning` | `#F0C067` |
| `danger` | `#FF8A91` | `disabled` | `#8E8E93` |
| `timeline-canvas` | `#202022` | `preview-surround` | `#0E0E10` |
| `timeline-grid` | `rgba(255, 255, 255, 0.06)` | `track-header` | `#202022` |
| `preview-controls` | `rgb(26 26 28 / 82%)` | 时间线文字 | `#F5F5F7 / #C7C7CC` 主/次 |
| `timeline-bar` | `#2C2C2E` |  |  |
| `waveform` | `#70A7FF` | `playhead` | `#FFFFFF` |
| `clip-video` | `#3D72B8` | `clip-audio` | `#2F5E70` |
| `clip-selected` | `#70A7FF` | `clip-offline` | `#D89B32` |
| `clip-muted` | `#636366` | `clip-locked` | `#48484A` |

仅导出蓝底白字，导入与确认用次要按钮，K仅hover蓝；focus / selection 是状态色，不是另一套 Accent。时间线使用低饱和蓝灰类型编码，不引入绿色或多彩运营色。正文 ≥ 4.5:1；状态不可只靠颜色。

**圆角值**

`control 6px` · `surface 10px` · `popover 10px` · `dialog 12px`。缩略图复用 control。

**间距网格**

4px 间距网格；常用 `4 / 8 / 12 / 16 / 24 / 32px`。时间线工具行36px、标尺行24px、轨道头96px；轨道画布不画纵横格线，时间刻度只在顶部标尺。

工作台分隔器：素材区180–460px、检查器220–380px，默认320/300px；时间线220–520px、默认300px。外边距/区域间距8px，面板圆角10px。空轨道不放伪片段与假波形。

**动效时长**

按下/悬停 `120ms` · 菜单/弹出层 `160ms` · 对话框/检查器 `220ms` · 高频编辑 `0ms`。减少动态效果：无空间运动，颜色/透明度反馈 ≤ `100ms`。

**禁止事项**

大圆角、胶囊化普通按钮、重阴影、紫色渐变、居中大标题、毛玻璃滥用；不引入用户字体包，不用动效延迟剪辑操作。
