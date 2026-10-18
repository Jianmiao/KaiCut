# MCP 导出闭环验收

日期：2026-10-16。环境：Windows x64、真实 Tauri 调试程序、WebView2、仓库内已有 LGPL FFmpeg/ffprobe。接口通过官方 MCP SDK stdio 客户端调用，没有使用 computer-use 或模拟鼠标键盘。

## 本轮交付

MCP 从 16 个增加到 20 个工具：

| 工具 | 参数 | 实际作用 |
| --- | --- | --- |
| `export_preflight` | `projectId / revision / outputPath` | 检查当前版本和新 MP4 路径，只读返回 ready 与检查项。 |
| `export_prepare` | 同上 | Rust 内部重新预检并创建固定项目版本的 queued 任务，不启动编码。 |
| `export_job_start` | `jobId` | 启动当前进程持有的可信快照；不受后续编辑影响。 |
| `export_job_cancel` | `jobId` | 取消指定任务，运行中任务需查询到终态。 |

复用现有预检、快照、编码 worker、并发锁和无覆盖发布，不开放自定义编码命令、客户端快照或覆盖参数。项目 schema 和 FFmpeg 二进制未升级。预检与准备截止时间为 60 秒；启动/取消为 5 秒。取消 MCP 请求或请求超时不等于取消导出，必须先查任务列表/状态。

## 实际运行结果

`pnpm mcp:export-check` 先完成原有 11 组真实媒体工作流检查，再执行 8 组导出检查，全部通过。

| 验收项 | 结果 |
| --- | --- |
| 隐藏实例 | 主窗口初始及最终 `visible=false / focused=false`，页面检查均要求原生主窗口隐藏。独立 app-data 配置，没有使用用户项目或恢复点。 |
| 原有闭环 | MP4/WAV/PNG 导入与探测，裁切/分割/移动、图片、音量和淡化、真实播放/暂停、另存和同 ID 重开通过。 |
| 只读预检 | 不创建输出文件或任务，不修改编辑状态；错误 revision 的 prepare 被拒，任务数量不变。 |
| 快照独立性 | revision 15、音轨音量 0.5 时准备任务；随后修改到 revision 16、音量 0.25。启动旧任务后成片仍使用冻结的 0.5，编辑状态不被导出改写。 |
| 编码与发布 | 完成任务到 100%；真实输出为 H.264 + AAC、1920×1080、30fps、120 帧、约 4 秒；AAC 为 48kHz 双声道。 |
| 解码画面 | 成片第 15/45/75/105 帧分别为蓝/黑/蓝/黄，覆盖源入点、空隙、移动片段及图片。8×8 RGB 解码样本每个像素均通过阈值检查。 |
| 解码音频 | 开头静音 RMS=0；稳定段 RMS=0.099956，符合冻结音量 0.5；淡出段 RMS=0.053454，符合逐帧淡出位置。 |
| 完成后保护 | 再次启动及迟到取消被拒，成片 SHA-256 不变。 |
| 目标抢占保护 | prepare 后人为创建同名哨兵文件；后续预检不 ready、prepare/start 被拒，哨兵文件字节不变。 |
| 排队取消 | 重复取消安全，未生成输出。 |
| 运行中取消 | 33 秒测试任务实际报告 8.5859% 编码进度后取消；先返回 running/cancelRequested=true，再到 cancelled/false；目标 MP4 未生成，任务临时 MP4 与滤镜脚本均清除。重复启动被拒。 |
| 项目与素材 | 测试修改已撤销，最终另存到新项目，revision 20、dirty=false。原始及复制素材的哈希、大小、修改时间保持不变。 |

结束前再次查询：4 个任务中 1 个 completed、3 个 cancelled，没有 queued/running 任务；无任务错误。SDK smoke 确认 20 个工具及 Rust/UI revision 一致。测试进程已停止，所属连接描述文件已清理；项目、成片及报告保留。

## 文件占用清理修复

审查发现旧实现依赖 `TempPath` 的静默析构删除临时文件，同时提前清除任务路径记录。Windows 外部句柄拒绝删除时，文件可能残留且失去恢复清理信息。

新增回归用真实 Windows 文件句柄允许读写但拒绝 DELETE 共享，先复现删除失败，再验证修复：失败/取消后显式检查清理结果；清理失败时保留路径到 `job.json`，并在任务 `error` 给出警告。重试必须先处理旧残留，启动恢复仍被占用时继续保留路径。解除占用后能够清除，已有成片不受影响。

这是文件锁回归的验证范围；正常运行中取消及无残留则另外由上面的真实 MCP 流程验证。正在被外部进程占用的文件不会被强制解锁。

## 可复查产物

目录：`src-tauri/target/mcp-export-validation/run-5df26d0c-98c3-4854-a863-10e192f7f11b/`。

- `MCP 导出成片;[literal].mp4`：102102 字节，H.264/AAC 成片。
- `MCP 导出验收.kaicut`：最终项目，媒体仍为外部引用。
- `validation.json`：完整真实接口和媒体状态证据。
- `export-probe.json`、`export-decoded.rgb`、`export-decoded.wav`：独立探测和解码结果。
- 三份合成媒体副本、前置流程另存的项目、`禁止覆盖.mp4` 哨兵文件。哨兵文件是刻意创建的测试文本，不是可播放视频。

成片 SHA-256：`c0db1b03142784b3201867af4fafc859be330db93653241d01b826b449380936`。

验收程序 `src-tauri/target/debug/kaicut.exe` SHA-256：`9935556993569320099E9AEAA77DC813BEAB99E91482917102851245856076A0`。应用快照与任务历史在独立 `com.kaicut.mcp-validation-36452` app-data 目录，报告记录具体路径；连接令牌不进入报告。

## 运行命令和检查

| 命令 | 本轮结果 |
| --- | --- |
| `pnpm lint` | 通过。 |
| `pnpm test` | 198 项通过，18 个文件。 |
| `pnpm test:mcp` | 20 项通过，含 SDK 握手、参数拒绝、导出转发和超时语义。 |
| `cargo test --locked`（src-tauri） | 100 项通过，6 项已有设备条件用例默认忽略。 |
| `cargo fmt --check`（src-tauri） | 通过。 |
| `pnpm tauri:build --debug --config src-tauri/tauri.sidecar.conf.json` | 通过；其中 beforeBuildCommand 执行 `pnpm build`，TypeScript/Vite 构建通过。 |
| `pnpm mcp:export-check --connection-file <独立连接文件> --output-dir <target下目录>` | 通过：11 组前置工作流、8 组导出检查。 |
| `pnpm mcp:smoke --connection-file <同一连接文件>` | 通过：20 工具发现，Rust/UI 状态读取。 |
| `git diff --check` | 通过。 |

Windows 调试包已生成：`src-tauri/target/debug/bundle/msi/KaiCut_0.1.0_x64_en-US.msi`（11030528 字节）、`src-tauri/target/debug/bundle/nsis/KaiCut_0.1.0_x64-setup.exe`（6270597 字节）。本轮未执行安装。Windows 链接器的“正在创建库/对象”信息仍被 Rust 标为 linker_messages warning，编译与测试退出码为 0。

## 修改文件

- 原生：`src-tauri/src/mcp_bridge.rs`、`src-tauri/src/export_job.rs`。
- MCP：`mcp/tools.mjs`、`mcp/bridge.mjs`、`mcp/server.test.mjs`、`mcp/bridge.test.mjs`、`mcp/workflow-check.mjs`、新增 `mcp/export-check.mjs`。
- 入口与文档：`package.json`、`README.md`、`src-tauri/CONTRACT.md`、`docs/MCP.md`、本文。

## 限制和下一步

这次验收覆盖当前 Windows 设备上的固定导出预设与合成短片，不代表所有 H.264 设备、长项目、复杂/VFR 素材或音画长期同步均已验证。没有重新执行 6 项默认忽略的设备用例；真实 MCP 编码及取消通过独立脚本执行。未新增 UI、编码器、云服务或项目格式。

下一步优先验证较长素材、多片段时间线的预览性能与音画同步，并把发现的瓶颈纳入可重复回归。
