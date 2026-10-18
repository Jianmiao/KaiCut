# 导入、剪辑、保存重开与导出回归

验证日期：2026-10-16。平台：Windows x64。本批次运行后台自动测试，没有控制桌面、打开原生窗口或操作用户素材。

## 验证范围

使用本地生成的真实 MP4 / WAV / PNG 文件，通过生产代码完成 `import_media → source_frames → edit_session → step_history → save_project → read_project → write_snapshot_to_dir → render_plan → FFmpeg → validate_output_metadata`。不是直接拼一个时间线 JSON 送给渲染器，也不是原生窗口端到端验收。

测试入口：`src-tauri/src/timeline.rs` 中的 `workflow::real_import_edit_save_reopen_snapshot_and_export`。默认 ignored，仅 Windows x64 且配有仓库 LGPL sidecar、可运行 `h264_mf` 的环境显式执行。编码失败会让测试失败，没有其他编码器降级或静默跳过。

每次在 `src-tauri/target/workflow-validation/<UUID>/` 创建独立目录，保留源媒体、`.kaicut`、快照、成片、探测 JSON 和解码样本。不会使用或覆盖上一次输出，也不删除测试后的可复查产物。目录在构建产物范围，不进入 Git；磁盘空间随显式测试次数增加，可在不再需要手工复查时清理整次运行目录。

## 真实媒体工作流

源素材：4 秒视频，前 1 秒红色、后 3 秒蓝色；4 秒单声道 WAV，前 1 秒静音、后 3 秒 440 Hz 正弦波，振幅 0.4；黄色 PNG。文件名包含中文、空格、分号、方括号与字面 `%03d`。重复导入同路径会跳过。

1. Rust 实际探测视频 / 音频为 120 项目帧、图片默认 150 项目帧，时间基为 30/1。
2. 视频分割、删除、撤销、重做，再裁切与移动；另一视频轨保留 15 帧红色视频。
3. 音频先设置淡入淡出，在淡入内分割并撤销，再裁切；独立轨音量设为 0.5，淡入淡出各 30 帧。第二音频轨加入相同素材并静音。
4. 保存 `.kaicut v2` 后重新读取，比较完整项目，核对源入点、源长度、淡入淡出、轨音量与静音，并用 `audio_gain` 检查帧边界增益。
5. 从重开的项目写入不可变快照并反序列化核对。快照后把编辑会话音量调到 0，验证输出仍按快照中的 0.5 渲染。
6. 使用生产渲染计划实际编码，检查 `progress=end`，并用生产输出校验器核对 H.264 / AAC、1920×1080、30 fps、120 帧、4 秒、48 kHz 双声道。
7. 将成片解码为 RGB 与 PCM，以像素和音频能量验证剪辑结果；对源媒体重新计算 SHA-256、检查修改时间，确认没有改变源文件。重新读取保存项目，确认之后的会话修改没有污染磁盘文件。

最终画面与音频：

| 时间 | 画面 | 音频 |
| --- | --- | --- |
| 0–0.5 秒 | 第二视频轨的红色片段 | 静音 |
| 0.5–1 秒 | 黑色空隙 | 静音 |
| 1–2 秒 | 第一视频轨的蓝色片段，取源素材 1–2 秒 | 淡入 |
| 2–3 秒 | 黑色空隙 | 稳态，轨音量 0.5 |
| 3–4 秒 | 黄色 PNG | 淡出 |

RGB 检查红 / 蓝 / 黄色区间，以及第 15、25、60、75、89 帧为空隙黑帧；全部解码样本长度为 120×8×8×3 字节。AAC 的能量测试使用有损编码容差，不要求逐样本相等。单声道转双声道后的稳态单声道 RMS 目标约 0.1；以同一输出的稳态能量作比例基准，验证淡入和按项目帧计算的淡出。

## 本批发现与修复

### 带淡入淡出的音频无法正常分割或缩短

原实现把两端 fade 原样复制到分割后的两个片段，裁切也保留原 fade 总长。120 帧片段设置淡入 30 + 淡出 30 后在第 15 帧分割，以及把淡入 80 + 淡出 20 的片段裁到 40 帧，都会错误返回“片段时长无效或超过 24 小时限制”。修复前新增的两个单元测试均实际复现失败。

修复后的规则：

- 分割仅保留原始外缘：左片段保留淡入、清除淡出；右片段保留淡出、清除淡入。外缘 fade 最多等于所在子片段时长。
- 在 fade 内分割会把外缘 fade 重新锚定并缩短；不宣称完整保留分割前的自动化包络。当前模型只有边界 fade 时长，不能表示任意起始增益。
- 裁切后，如果原 fade 总长仍可容纳，保持其时长；否则按原长度比例缩短。淡入取 `floor(newDuration × oldFadeIn / oldFadeTotal)`，淡出使用剩余帧，乘法先扩展为 `u128`。再次延长片段不会自动放大 fade。
- 无效源范围、零时长和锁轨仍拒绝整次操作，不改变项目、历史或会话状态。成功编辑可撤销 / 重做，并可保存重开。

三组确定性回归覆盖在淡入内、中段、淡出内分割；不等长 fade、整数取整、单侧 fade；延长后不扩张；锁轨、越界、失败原子性与源偏移。真实媒体工作流也执行了“fade → split → undo”。

### 非方形像素视频导出变形

同批渲染回归 `export_render::tests::real_sidecar_preserves_anamorphic_display_aspect_ratio` 检出缩放仅按编码宽高、再直接改为方形像素的问题。修复为按显示比例计算输出尺寸，仍使用现有 FFmpeg 二进制。

实际生成并编码 160×120 的两个源视频：SAR 4:3 时显示比例为 16:9，导出应满宽；SAR 1:2 时显示比例为 2:3，导出应保留较窄画面及左右黑边。测试先检查输入 SAR，再实际编码、验证元数据、解码 RGB 检查画面边界，已完成修复前失败 / 修复后通过。此专用测试使用临时目录，样本不会长期保留；上面的工作流样片是方形像素样本。

## 运行记录与复现

在 `src-tauri` 目录运行：

```powershell
cargo test --locked --lib timeline::tests
cargo test --locked --lib timeline::tests::workflow::real_import_edit_save_reopen_snapshot_and_export -- --ignored --exact --nocapture
cargo test --locked --lib export_render::tests::real_sidecar_preserves_anamorphic_display_aspect_ratio -- --ignored --exact --nocapture
```

时间线定向验证：21 项通过、1 项实际媒体测试默认忽略；随后显式运行实际媒体工作流，1 项通过，执行阶段 2.75 秒。其他模块、全量测试、lint 和构建结果由本批总验收记录汇总，不以这里的定向结果代替。

本轮保留目录：`src-tauri/target/workflow-validation/98ee1285-41b7-452a-bbce-e38c83b1a2aa/`。

- `剪辑验收.kaicut`：可手动打开的项目，引用同目录下保留的真实源媒体。
- `workflow-sample.mp4`：可手动播放的 4 秒成片。
- `workflow-sample.ffprobe.json`：成片探测数据。
- `export-jobs/job-5560f1c4-2969-4247-9fc8-793760442132/snapshot.json`：实际用于渲染的快照。
- `validation.json`：实测结果、源媒体 SHA-256 和产物绝对路径。
- `filters.txt`、`progress.txt`、`decoded.rgb`、`decoded.wav`：渲染与解码复查材料。

实测 RMS：静音 0；稳态 0.0999561；淡入采样区间 1.18–1.22 秒为 0.0200940；淡出采样区间 3.68–3.72 秒为 0.0267915。检查全部通过。每次重跑生成新 UUID，因此本记录路径不会被覆盖；测试日志会报告新的目录。

## 尚未验证

- 没有运行真实窗口导入、拖动、快捷键、播放与导出对话框的人工验收；后台调用生产函数不等同于验证 Tauri IPC、原生文件选择器或 WebView 播放体验。
- 该集成用例使用生产渲染器和输出元数据校验器，但没有通过 Tauri job supervisor 启动；取消、失败清理、原子无覆盖发布由 `export_job` 的独立测试覆盖。
- 尚未用用户长项目、长 GOP / VFR / HDR 或真实相机 MOV 完成整段流程。短合成素材能精确判断源范围、帧数、空隙、音量与画面比例，不能代表所有设备的性能和兼容性。
- `h264_mf` 在本机成功，不保证所有 Windows 设备；macOS 设备验收仍待设备，Linux 延后。本批没有更换 FFmpeg 二进制或更改第三方许可。

## 同批交互修复与整库验收

时间线新增的交互测试先复现 9 项失败后转绿：同轨拖动/裁切时片段预览消失、拖动后点击折叠多选、带修饰键选择误触移动、多轨选区压在同轨、纯垂直拖动未生效、首尾越界、选区成员自吸附、锁轨仍参与拖动。现在时间和轨道位置均整体移动；目标轨不足或锁定时，整组保留原轨并允许水平移动；取消、过期 revision 不提交。多选依然只支持同类型片段，不引入持久编组或音画自动关联。

导出窗口修复了迟到任务历史重新覆盖新建表单、已结束旧项目遮挡新项目、旧预检阻塞当前检查、较早历史让已结束任务重新显示为运行中、轮询恢复后残留错误等问题。运行中的旧项目任务保留可查询/取消入口；结束后释放新项目表单。重试仍使用提交时快照，不因后来编辑 revision 改变输出。

2026-10-16 整库命令与结果：

- `pnpm lint`：通过。
- `pnpm test`：16 个文件、150 项通过。
- `pnpm build`：通过。
- `cargo fmt --check`：通过。
- `cargo test --locked`：75 项通过、6 项设备相关用例默认忽略。
- 设置 `KAICUT_THUMBNAIL_TEST_VIDEO` 为本次工作流样片后，`cargo test --locked --lib -- --ignored`：6 项全部通过（完整后台工作流、3 项导出画面/混音/帧率矩阵、实际 worker 编码/取消、原有缩略图矩阵）。
- `git diff --check`：通过。
- `pnpm tauri:build --debug --config src-tauri/tauri.sidecar.conf.json`：通过，已更新 `src-tauri/target/debug/kaicut.exe`、MSI 与 NSIS 调试安装包。未安装、未启动；这不代表真实窗口流程已人工验收。

本批修改文件：

- `src-tauri/src/timeline.rs`：淡化边界与真实工作流测试。
- `src-tauri/src/export_render.rs`：非方形像素比例修复与实际成片回归。
- `src/components/editor/timeline.tsx`、`timeline.test.tsx`、`src/lib/timeline-view.ts`、`timeline-view.test.ts`：拖动预览、选区、轨道与边界。
- `src/components/editor/export-preflight-dialog.tsx`、`export-preflight-dialog.test.tsx`：导出异步状态隔离。
- `src-tauri/CONTRACT.md`、`docs/EDITING-OPERATIONS.md`、`docs/M3-EXPORT-PREFLIGHT.md`、本文：当前语义与验收记录。

项目格式、FFmpeg 二进制与版本均未变；本批没有运行安装程序或自动启动应用。
