# 剪映操作模型审计

日期：2026-10-11。对象：本机安装的剪映专业版 11.5.0.14471，路径 `F:\JianyingPro\11.5.0.14471`。目的：提取可独立复现的操作模型和布局线索，帮助 KaiCut 对齐使用习惯。本文不复制剪映代码、二进制、图标或私有资源，也不把静态字符串当成完整算法。

## 已核实的实现线索

剪映使用 Qt 6 / QML 和原生 DLL。`VECreator.dll` 中可以读到以下 UI 命令与文案标识：

| 线索 | 证据 | 对 KaiCut 的含义 |
| --- | --- | --- |
| 播放切换 | `VEShortcut.togglePlay`；QML `Shortcut` 的 `autoRepeat: false`，触发 `viewModel.togglePlay(true)` | 播放/暂停应走统一命令，不因按键重复触发多次 |
| 导入媒体 | `VEShortcut.importMedia`；启用条件包含窗口可见、遮罩关闭和 OOBE 关闭 | 导入快捷键必须受模态层和输入状态保护 |
| 快捷键分发 | `CommandMgr.editorShortCutList`、`CommandMgr.onShortCut`、`VEShortcut.editShortcutEnabled` | 快捷键来自命令列表和可见/启用状态；不能只在组件里监听全局 keydown |
| 分割 | `pc_mouse_split_mode` | 分割是编辑模式/工具命令，不只是一个图标点击 |
| 删除 | `pc_delete_the_selected_material` | 删除以当前选择为目标，不按素材名批量删除 |
| 左右裁切 | `current_position_not_support_cut_left`、`current_position_not_support_cut_right` | 裁切必须检查播放头是否落在合法片段范围，端点操作要有明确错误状态 |
| 自动吸附 | `turn_off_automatic_snapping`、`pc_main_rail_magnetic_attraction` | “普通吸附”和“主轨磁吸”是两个能力，不能合并成一个开关 |
| 波纹编辑 | `ripple_delete`、`ripple_move` | 波纹删除/移动会改变后续片段位置，不能用普通删除替代 |

这些字符串位于安装二进制资源区，只能证明命令和文案标识存在。未找到可直接核实的默认 Ctrl/Command 快捷键表；`CommandMgr.editorShortCutList` 是运行时模型，不能从命令名推断组合键。

## 时间线模型线索

随安装发布的 XML 参考文件：

`F:\JianyingPro\11.5.0.14471\Resources\canvas_agent\skills\studio.lyra.xml_timeline_builder\references\xml\core-track-and-fields.md`

其中可核实：

- `main` 是顺序播放的 video/image 主轨；主轨片段排列顺序决定播放顺序。
- 副轨内容放在 `multi`，需要用绝对开始时间或相对对齐关系唯一确定位置。
- 源入点使用 `material_start`，时间线长度使用 `duration`，速度使用 `speed`；副轨可用 `start` 或对齐字段。
- 时间值统一使用无单位毫秒。
- 图片没有可裁剪的源时间范围，也不能写 `material_start` 或 `speed`；图片只保存时间线 `duration`。
- 修改已有片段应按片段 ID 定位，不能按同一个素材 ref 批量修改。

KaiCut 当前采用项目时间基帧存储：`startFrame`、`durationFrames`、`sourceInFrame`、`sourceDurationFrames`。这与剪映 XML 的毫秒表达不同，但表达的是同一个必要关系：时间线位置、源入点和使用时长必须分开保存。

## KaiCut 已复现与未复现

已独立实现：

- 片段添加、选择、分割、删除、创建副本。
- 片段移动、跨兼容轨道移动、左右裁切。
- 播放头定位、帧输入、边缘吸附、缩放、锁轨。
- Rust 权威撤销/重做、修订号和过期命令拒绝。
- v2 项目格式、v1 空轨迁移、源范围校验和重链过短保护。
- 轨道画布移除纵横格线，只保留标尺、轨道边界和播放头。

尚未复现：

- 剪映同版本默认快捷键的完整列表。
- 主轨磁吸与普通吸附的差异化行为。
- 波纹删除、波纹移动和后续片段自动重排。
- 多选、成组、复制粘贴、音画关联/分离。
- 连续播放、VFR 精确定位、音频主时钟和真实波形。
- 变速曲线、关键帧、转场和导出任务。

## 接下来按这个顺序实现

1. 在真实播放稳定后，补齐播放头与片段范围的自动滚动和跨片段播放。
2. 把普通吸附和主轨磁吸拆成两个独立状态与测试集。
3. 实现波纹删除/移动，要求一次命令原子修改后续片段位置，并能完整撤销。
4. 实现多选/成组/复制粘贴，再接音画关联和分离。
5. 最后接变速、关键帧、转场和导出，避免先做按钮再补底层时序。

KaiCut 采用的是基于公开可观察行为和自有数据模型的独立实现，不对剪映内部算法、默认快捷键或私有实现作未经验证的结论。
