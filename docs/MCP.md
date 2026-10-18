# KaiCut 本地 MCP 接口

KaiCut 提供基于官方 `@modelcontextprotocol/sdk` 的标准 MCP stdio 服务。它连接已经运行的桌面应用，读取 Rust 权威项目状态和真实页面的结构化状态，并通过现有编辑命令操作时间线。无需模拟鼠标、键盘或调用 computer-use。

**结构化检查不能代替像素验收。** `ui_inspect` 能确认控件可见性、禁用状态、位置、计算后的字体/颜色样式、播放头、选择，以及媒体元素的 currentTime/paused/volume/readyState；还能读取预览图片的加载状态和页面可见性。它不返回截图，不能单独证明实际像素、音质或播放流畅性。

## 启动

需要当前源码、已安装的 pnpm 依赖及 Node.js。MCP 服务属于开发工具，当前不打入桌面安装包，也不会自行启动 KaiCut。

在 PowerShell 中启动带桥接的桌面程序：

```powershell
Set-Location 'E:\KaiCut（开剪）'
& '.\src-tauri\target\debug\kaicut.exe' --mcp
```

开发模式通过 Tauri 参数转发：

```powershell
pnpm tauri dev -- -- --mcp
```

不带 `--mcp` 时，应用不监听 MCP 桥接端口。退出启用桥接的应用并正常重新启动，即可关闭此功能。

应用启动后会在以下 Windows 默认位置生成连接描述文件：

```text
%LOCALAPPDATA%\com.kaicut.desktop\mcp\connection.json
```

其中包含当前进程的本地端口、PID、协议版本和随机访问令牌。**不要把这个文件提交到 Git、复制进聊天或公开共享。** 正常退出清理当前进程的描述文件；过期描述文件不会让 MCP 服务启动新应用。

隔离验收时，可在启动 KaiCut 前设置绝对路径：

```powershell
$env:KAICUT_MCP_CONNECTION_FILE = 'E:\KaiCut（开剪）\src-tauri\target\mcp-validation\connection.json'
& '.\src-tauri\target\debug\kaicut.exe' --mcp
```

MCP 服务也支持相同的环境变量，或显式参数：

```powershell
node .\mcp\server.mjs --connection-file 'E:\KaiCut（开剪）\src-tauri\target\mcp-validation\connection.json'
```

此进程通过 stdin/stdout 交换 MCP JSON-RPC，启动后等待客户端输入是正常现象。stdout 不输出普通日志。

## 配置 MCP 客户端

将客户端的 stdio 启动命令指向 Node 和仓库内的脚本。建议使用 Node 的绝对路径，避免图形客户端与终端的 PATH 不同。

通用配置示例：

```json
{
  "mcpServers": {
    "kaicut": {
      "command": "C:\\Program Files\\nodejs\\node.exe",
      "args": ["E:\\KaiCut（开剪）\\mcp\\server.mjs"]
    }
  }
}
```

Node 实际位置可用 `(Get-Command node).Source` 查询；不同客户端的配置文件位置和字段可能不同。此示例不包含访问令牌，也不自动改写任何客户端配置。

Codex TOML 示例：

```toml
[mcp_servers.kaicut]
command = 'C:\Program Files\nodejs\node.exe'
args = ['E:\KaiCut（开剪）\mcp\server.mjs']
```

使用非默认连接文件时，将 `--connection-file` 和完整路径追加到 `args`。连接路径必须与已启动的 KaiCut 指向同一文件。

## 工具

当前提供 20 个工具；下方合并展示撤销/重做及播放/暂停两组。历史 13/16 个工具阶段的记录保留在各自验收段落。

| 工具 | 参数 | 行为 |
| --- | --- | --- |
| `get_state` | `{}` | 读取当前项目、轨道、媒体引用、revision、dirty、撤销/重做状态。 |
| `project_open_path` | `{ "projectId": "…", "revision": 3, "path": "绝对路径.kaicut" }` | 完整校验并打开已有项目；当前项目 dirty 时拒绝，没有丢弃更改参数。同 ID 重开也会清空编辑视图和历史。 |
| `project_import_paths` | `{ "projectId": "…", "revision": 3, "paths": ["绝对媒体路径"] }` | 导入 1–100 个 mp4/mov/mp3/wav/png/jpg/jpeg 文件，整批校验后提交；已导入的文件跳过，不修改源文件。 |
| `project_save_as` | `{ "projectId": "…", "revision": 3, "path": "新绝对路径.kaicut" }` | 原子另存为新文件，成功后设置项目路径并清除 dirty。拒绝覆盖任何已有目标，不自动创建目录。 |
| `ui_inspect` | `{}` | 从真实页面读取播放头、选中片段、预览模式/播放状态、当前画面和音频片段 ID、可见控件及其坐标和状态、警告和视口尺寸。 |
| `media_check_links` | `{}` | 检查当前项目媒体引用是否在线。 |
| `media_probe` | `{}` | 调用现有媒体探测流程，读取当前项目所有媒体元数据。 |
| `export_job_list` | `{}` | 读取已有导出任务。 |
| `export_job_status` | `{ "jobId": "…" }` | 读取一个已有导出任务。 |
| `export_preflight` | `{ "projectId": "…", "revision": 3, "outputPath": "新绝对路径.mp4" }` | 只读导出预检，返回原生 `ready/checks`；不创建任务或成片。 |
| `export_prepare` | `{ "projectId": "…", "revision": 3, "outputPath": "新绝对路径.mp4" }` | 内部预检并由现有快照命令复核，创建 queued 不可变任务；不启动 FFmpeg。 |
| `export_job_start` | `{ "jobId": "…" }` | 启动已有冻结任务，使用同一原生 worker、单任务限制和文件保护。 |
| `export_job_cancel` | `{ "jobId": "…" }` | 明确取消已有任务；运行中任务需继续查询状态，直至取消完成。 |
| `timeline_edit` | `{ "projectId": "…", "revision": 3, "edit": {…} }` | 通过 Rust 原子编辑命令修改时间线，保留现有锁轨、边界、类型、重叠检查和撤销语义。 |
| `timeline_undo` / `timeline_redo` | `{ "projectId": "…", "revision": 3 }` | 撤销或重做一次时间线操作。 |
| `ui_seek` | `{ "projectId": "…", "revision": 3, "frame": 48 }` | 移动预览播放头，不修改项目内容。 |
| `ui_select` | `{ "projectId": "…", "revision": 3, "clipIds": ["…"] }` | 选择片段；空数组清空选择。 |
| `ui_play` / `ui_pause` | `{ "projectId": "…", "revision": 3 }` | 控制真实预览播放器。 |

`timeline_edit.edit` 支持当前已有的 `add / split / delete / rippleDelete / duplicate / move / moveMany / trim / trackState / trackVolume / clipFade`。完整字段由 `tools/list` 的 `inputSchema` 返回，并在 MCP 和 Rust 两端校验；不接收任意脚本或命令。时间线删除只删除项目中的片段，不删除媒体源文件。

三个项目文件工具都要求从 `get_state` 获取的当前 `projectId + revision`，只接受绝对 Unicode 文件路径；Rust 限制路径为 32768 UTF-8 字节并拒绝控制字符、Windows 设备路径/保留设备名和文件替代数据流。MCP 导入数组不能含重复字符串；此前已导入的文件仍会按源路径去重。另存目标必须位于已经存在的目录，现有普通文件、目录、硬链接和符号链接均不覆盖；即使目标在检查后才出现，最终无覆盖发布也会失败并保留目标。

当前不暴露覆盖项目/成片、任意文件读写或通用脚本执行工具。MCP `project_save_as` 与界面“保存”不同：前者每次都需要新的目标文件名，后者仍可保存到当前项目文件。

`ui_inspect.imageNodes` 最多返回两个预览图片的 `complete / naturalWidth / naturalHeight`。`complete=true` 也可能对应加载失败，确认成功加载需要同时检查自然宽高大于 0。音视频仍使用 `mediaNodes` 检查 `readyState`、`paused`、`currentTime`、`seeking` 和 `errorCode`。

所有 `ui_*` 工具成功返回时，Rust 会额外从实际 `main` 窗口读回 `nativeWindow.visible / focused / minimized`，分别表示原生窗口可见、焦点及最小化状态。`viewport.visibility` 与 `viewport.hidden` 则来自页面的 `document.visibilityState / document.hidden`，描述 WebView 文档状态；两者不能互相代替。原生主窗口已经隐藏时，WebView 文档仍可能报告 `visible`。检查是否隐藏必须使用 `nativeWindow.visible`，不能只看启动参数、DOM 字段或未经窗口类名/标题核对的进程 `MainWindowHandle`。后台隐藏窗口的解码状态仍不能当作可见窗口的视觉验收。

### 预览时钟与检查开销

`ui_inspect` 的现有响应增加以下只读诊断字段，不新增 MCP 工具，也不写入项目文件。所有值在每次检查时采样，没有后台监听或全局 FPS 统计循环。

| 字段 | 含义 |
| --- | --- |
| `mediaNodes[].expectedSourceTime` | 时间线模式下，按节点的片段 ID 查找项目片段，以 `(sourceInFrame + playheadFrame - startFrame) × fpsDenominator / fpsNumerator` 计算预期源时间，单位秒。素材模式、片段不存在或结果非有限数时返回 `null`。 |
| `mediaNodes[].driftSeconds` | `currentTime - expectedSourceTime`，正数表示媒体元素时钟领先项目播放头，负数表示落后。任一输入不可用时返回 `null`。这是媒体时钟诊断，不是物理音频延迟或音画同步证明。 |
| `mediaNodes[].totalVideoFrames / droppedVideoFrames` | 视频元素通过可选 `getVideoPlaybackQuality()` 提供的累计计数。音频节点、API 不可用/抛错或计数非有限数时返回 `null`。节点或资源重建可能重置计数；跨片段比较必须先确认节点身份。 |
| `performance.inspectedAtMonotonicMs` | 本次检查开始时的 WebView `performance.now()`，单位毫秒。只适用于该页面自身的单调时钟，不能直接和外部 MCP 客户端时钟相减。 |
| `performance.inspectionDurationMs` | 页面采集本次状态、控件、媒体和 DOM 数量所用的时间，单位毫秒。不含整个 MCP 请求往返、Rust 通道等待、响应序列化或客户端处理时间。 |
| `performance.domNodeCount` | 当前文档中 DOM **元素**的数量；不包含文本和注释节点。 |
| `performance.timelineClipElementCount` | 当前挂载的 `.timeline-clip` 元素数量；不是整个项目的片段总数，也不能单独证明它们都处于视口内。 |
| `preview.clockMode / masterKind / masterClipId / buffering` | 当前时间来源、主节点及缓冲状态；clockMode 为 audio/video/monotonic/source，缺失返回 null。存在主媒体但未就绪时冻结，只有真实无媒体区间用单调时钟。 |
| `preview.phase` | 从预览协调器读取 preparing/starting/playing/buffering/paused/source；旧版本或未知值返回null，不从playing推断已完成准备。 |
| `mediaNodes[].playbackRate / nodeId / hardSeekCount` | 真实播放速率、元素生命周期身份及累计硬定位次数。相邻连续切段可保持同一 nodeId；不能只根据 clipId 变化推断重建。字段缺失返回 null。 |

分析时先检查数值是否有限；`null` 表示不可用，不能转成零计入平均值或 P95。媒体时钟漂移的 P95 与 MCP 往返耗时的 P95 是不同指标，报告应分别列出。隐藏窗口中的累计视频计数不能用来宣称可见窗口达到某个帧率或没有卡顿；可见画面呈现、音频设备延迟和高分辨率真实素材负载需要独立验收。

## 编辑顺序与一致性

1. 调用 `get_state`，取得真实 `project.projectId` 和 `revision`。
2. 调用 `ui_inspect` 检查页面是否已显示同一项目和 revision。
3. 用这个上下文提交一次编辑，例如：

   ```json
   {
     "projectId": "从 get_state 读取",
     "revision": 3,
     "edit": { "type": "split", "clipId": "从当前轨道读取", "frame": 48 }
   }
   ```

4. 以返回状态的新 revision 继续，必要时再次检查 UI。不要猜测 ID、revision 或片段边界。

Rust 是编辑权威状态，UI 收到更新后采用同一项目结果。过期 revision 会拒绝修改。UI 动作也检查项目/revision，并返回页面确认；不能把“发出了事件”当成动作已完成。

打开项目的返回值和 `kaicut:mcp-session` 事件额外含 `resetEditingView:true`。它只属于运行时消息，不写入 `.kaicut` 或撤销历史；即使重开同一 projectId，也会清空片段/素材选择、复位播放头并进入时间线模式。项目更换、重开或媒体引用变化后，UI 按“先检查媒体链接，再探测媒体信息”顺序刷新，并在两步之间确认项目和请求代次仍有效，避免探测开始时让尚未返回的链接检查失效。

网络超时或客户端取消并不保证编辑尚未发生。遇到这类错误应先重新读取状态，再决定下一步；适配器不会自动重试编辑。撤销会保守地将项目标为未保存，即使轨道内容恢复到了操作前。

## 导出顺序与取消语义

1. 用 `get_state` 读取当前 `projectId/revision`，为成片指定已经存在目录中的新绝对 `.mp4` 路径。
2. 调用 `export_preflight`，检查 `ready` 和每项 `checks`。工具调用成功但 `ready=false` 仍表示预检未通过；编码器被列出也不代表设备已成功编码。
3. 调用 `export_prepare`。服务端自行执行预检，并调用现有 `export_snapshot_create` 再次检查结果与项目修订；返回 queued 任务和 jobId，不启动编码、不创建目标 MP4。客户端不能传入 `preflight`、快照、`overwrite`、编码器或 FFmpeg 参数。
4. 以这个 jobId 调用 `export_job_start`，再通过 `export_job_status` 检查进度与最终状态。任务使用准备时的冻结快照，后续编辑和 revision 变化不会改变成片输入；start/cancel 不接受当前项目上下文或新的输出路径。
5. 需要停止时调用 `export_job_cancel`。排队任务直接取消；运行中任务先提出取消请求，必须继续查询，不能把收到取消确认当作 worker 已退出或临时文件已清理。

`retryAvailable` 表示当前进程仍保有可用的可信快照；还必须结合任务状态判断，只有 queued/failed/cancelled 可尝试启动，running/completed 不可重启。文件占用可能阻止临时文件删除；这时任务会保留路径并在 `error` 报告清理警告，解除占用后重试或下次启动时继续清理。成功成片不参与这项清理。

`export_preflight/export_prepare` 的 MCP 桥接截止时间为 60 秒，start/cancel/list/status 为 5 秒；这些是请求等待期限，不是导出作业执行时限。**取消 MCP 请求不等于取消导出任务。** 断开、失败或超时后，先查询 `export_job_list`/`export_job_status`，确认是否已生成任务或已启动，再决定下一步；不要盲目重复 prepare/start。实际编码沿用原来的 Windows H.264/AAC 导出 worker、锁、快照和无覆盖发布保护，不存在另一套 MCP 编码器。

## 验证

```powershell
pnpm test:mcp
pnpm mcp:smoke
pnpm mcp:live-check
```

- `test:mcp` 使用官方 SDK Client 和 StdioClientTransport 启动真实 MCP 子进程，测试协议发现、鉴权桥接、参数校验、旧 revision 错误传递、超时、取消、大小限制和令牌不外泄。测试 TCP 对端是独立受控夹具，不等同于桌面实机验收。
- `mcp:smoke` 连接正在运行的 KaiCut，只执行握手、工具发现、`get_state` 和 `ui_inspect`；打印计数及 UI 状态，不打印媒体路径或令牌。
- `mcp:live-check` 额外检查媒体链接、元数据和已有导出任务，默认仍为只读。

以上两个实时验收命令支持 `--connection-file <绝对路径>`。

仅在临时空项目上验证编辑闭环：

```powershell
pnpm mcp:live-check --connection-file 'E:\KaiCut（开剪）\src-tauri\target\mcp-validation\connection.json' --exercise
```

`--exercise` 要求未保存、没有媒体、没有片段、没有编辑历史且未修改的空项目。它验证过期 revision 拒绝、暂停/定位/清空选择，以及锁轨→页面同步→撤销。最终比较轨道内容确实恢复。revision 和历史会正常推进，dirty 会保留为 true；验收结束直接关闭该临时项目，不保存。

### 真实媒体工作流验证

`mcp:workflow-check` 通过官方 SDK 客户端连接实际桌面进程，使用专用测试视频、音频、图片验证导入→剪辑→定位/播放→另存→重开流程。它要求干净、未保存、无素材、无片段、无编辑历史的临时项目；不要连接正在工作的用户项目。使用 `--mcp --mcp-hidden --mcp-validation` 启动调试构建，并显式指定独立连接文件。当前 `--mcp-hidden` 同时设置窗口初始隐藏与 `focus=false`，禁用创建时的原生窗口/WebView 焦点请求；不通过“显示后再隐藏”实现后台验收。

测试素材来自已有的真实媒体工作流生成器：4 秒红/蓝视频、4 秒静音/440 Hz 正弦波 WAV、黄色 PNG。文件名故意包含中文、空格、分号、方括号和字面 `%03d`。如果现有素材目录不存在，在 `src-tauri` 运行：

```powershell
cargo test --locked --lib timeline::tests::workflow::real_import_edit_save_reopen_snapshot_and_export -- --ignored --exact --nocapture
```

该生成器还会运行一次已有 FFmpeg 导出回归，需要已打包的 Windows FFmpeg/ffprobe 和可用的 `h264_mf` 设备能力。它会打印新的 `src-tauri/target/workflow-validation/<UUID>/` 目录；将该目录传给下列命令的 `--fixtures`。MCP 工作流复制其中三个生成素材后使用，不修改原始文件；普通工作流检查不启动导出，显式追加 `--export` 才进入导出验收。

从仓库根目录运行：

```powershell
pnpm mcp:workflow-check --connection-file 'E:\KaiCut（开剪）\src-tauri\target\mcp-validation\connection.json' --fixtures 'E:\KaiCut（开剪）\src-tauri\target\workflow-validation\<生成器报告的UUID>' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\mcp-workflow-validation'
```

- `--output-dir` 必填，必须位于本仓库的 `src-tauri/target` 内，解析目录链接后也不能越界。脚本创建新的 `run-<UUID>/` 子目录，不复用已有验收输出。
- 第一次 `ui_inspect` 及后续每次 `ui_*` 返回都必须满足 `nativeWindow.visible=false`；否则脚本失败，不替用户隐藏或修复窗口。报告同时保留原生窗口状态和 DOM 视口状态，用于区分隐藏、焦点与文档调度行为。
- `--fixtures` 应指向上述生成器的目录。未指定时兼容使用历史目录 `workflow-validation/98ee1285-41b7-452a-bbce-e38c83b1a2aa`；该目录不存在时需先生成素材并显式传入。三个素材均须为小于 32 MiB 的普通文件。
- 脚本覆盖真实导入/探测、裁切/分割/移动、图片、音轨音量/淡化、源时间定位、空隙、图片实际加载、另存拒绝覆盖、脏项目拒绝打开，以及同 ID 重开后的 UI 重置。
- 播放检查要求同一片段的音视频元素 `currentTime` 在未暂停时实际推进，暂停后播放头及媒体时间稳定；只看到 `playing=true` 不算解码播放证据。
- 每次保留两份新 `.kaicut`、复制素材及 `validation.json`。结束时核对原始和复制素材的 SHA-256、大小和修改时间；报告包含状态与产物路径，不含连接令牌。

历史 16 工具阶段的实际执行结果与产物见 [MCP-WORKFLOW-VALIDATION.md](./MCP-WORKFLOW-VALIDATION.md)：当时的 16 工具发现、真实导入/剪辑/解码/播放/暂停/保存/重开均通过。该记录不包含随后新增的四个导出工具。隐藏 WebView 的调度与可见窗口不同，结果不代表长项目性能、音质或逐像素一致性。

### 真实导出闭环验证

在新的隔离、隐藏且不请求焦点的调试实例上运行：

```powershell
pnpm mcp:export-check --connection-file 'E:\KaiCut（开剪）\src-tauri\target\mcp-validation\connection.json' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\mcp-export-validation'
```

它等价于 `pnpm mcp:workflow-check --export`，也接受 `--fixtures <生成素材目录>`，复用上节的素材、干净空项目及输出路径约束。应用使用 `--mcp --mcp-hidden --mcp-validation`；脚本检查原生隐藏状态，并记录焦点读回，不模拟鼠标键盘或控制桌面。该专用实例还必须没有既有导出任务，避免把用户任务纳入验收。

脚本先执行完整导入、编辑、预览播放、另存和重开流程，再执行以下验收：

- 预检只读，旧 revision 的准备请求不创建任务或目标文件。
- 准备快照后改变编辑会话音轨音量，再启动冻结任务，检查实际成片仍使用准备时的音量。
- 完成后验证 1920×1080、30 fps、120 帧/4 秒 H.264 + AAC、48 kHz 双声道；使用打包 sidecar 解码 RGB/PCM，检查蓝/黑/蓝/黄色区间、静音、增益与淡出。
- 快照准备后再创建同名测试目标，确认预检、准备和启动均拒绝覆盖；已完成成片也须在重复启动/迟到取消后保持原哈希。
- 分别取消排队任务和已经报告实际编码进度的运行任务，等待终态并检查任务临时 MP4/滤镜脚本；检查不通过就报告失败，不能由“取消请求已接受”推断清理成功。
- 撤销测试临时编辑，另存验收项目；再次确认原始及复制素材的哈希、大小和修改时间未改变。

产物位于新的 `run-<UUID>/`，包含项目、成片、探测 JSON、解码样本与 `validation.json`。应用管理的冻结快照保留在隔离 app-data 目录，报告记录对应路径。脚本只尝试取消本次创建的未结束任务，停止或清理失败会保留错误供检查。以上为验收方法；本轮实际成功、失败、修复及复测结果单独见 [MCP-EXPORT-VALIDATION.md](./MCP-EXPORT-VALIDATION.md)，不以脚本存在推断通过。

## 边界与来源

起播专项：`pnpm mcp:startup-check --connection-file <独立连接文件> --output-dir <target内目录>`。在全新隐藏实例中重复5次起播、14秒边界及3次播放中定位，保留首500ms、每边界、节点独立源时间进展和主节点单独运行窗口。功能通过不代表一帧质量通过；质量超标会保留报告并返回退出码1。[本轮实际结果](./PLAYBACK-STARTUP-VALIDATION.md)仍有起播质量失败，不能将工具请求成功当作同步达标。

长时播放专项使用 `pnpm mcp:playback-check --connection-file <隔离连接文件> --output-dir <target内目录>`。默认实际运行至少 600 秒且项目推进到 600 秒；`--seconds 30` 仅为明确标记的短测，不能替代十分钟验收。脚本生成 610 秒合成素材；`--fixtures <此前运行目录>` 可以只读复用素材。后续验证暂停、首中尾定位和 14 秒切段/空隙/换主场景，保存 `playback-validation.json`。稳定样本 P95 与最大相对音视频媒体时钟差分别检查小于一帧，所有缓冲和边界样本另保留；它仍不测量扬声器或显示器实际输出延迟。

- 仅监听 `127.0.0.1` 随机端口，每次应用启动使用新令牌；这是本机开发桥接，不是远程控制服务。
- 访问令牌通过连接描述文件提供。拥有当前用户文件读取权限的本地进程属于同一信任边界，不能宣称抵御同用户恶意进程。
- 连接描述最大 16 KiB，请求最大 1 MiB，响应最大 16 MiB；项目打开/导入/另存操作 30 秒截止，媒体探测和导出预检/准备 60 秒截止，其他操作 5 秒截止。超时不保证已开始的文件、编辑或导出操作被取消，必须先重新读项目/任务状态再决定是否重试。只有受限工具可访问桥接。
- `tools/list` 为只读工具设置 `readOnlyHint`，编辑/撤销/重做设置 `destructiveHint`。这些是客户端提示，实际权限和参数验证由服务端执行。
- 当前以 Windows 验证为准；macOS/Linux 的原生运行验收尚未完成。

参考：

- [MCP 官方传输规范：stdio、JSON-RPC 与 stdout 约束](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [MCP 官方 TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk)
- `src-tauri/CONTRACT.md`：KaiCut 编辑命令与会话契约。

## 历史接通记录：首批 13 个工具（2026-10-16）

以下保留首批接通时的结果与计数；它不包含随后新增的三个项目文件工具，也不是本轮真实媒体工作流的验收结果。

- 已按 [Codex 官方 MCP 配置说明](https://developers.openai.com/codex/mcp/) 注册本地 `kaicut` stdio 服务：`F:\Node\node.exe E:\KaiCut（开剪）\mcp\server.mjs`。`codex mcp get kaicut` 确认 enabled。该配置不含令牌；MCP 服务不会自动启动桌面程序。客户端需要重新加载工具时，可在新会话检查工具列表。
- 使用真实 Tauri 调试可执行文件，以 `--mcp --mcp-hidden --mcp-validation` 启动专用后台验证实例。`--mcp-hidden` 不显示窗口；仅调试构建支持的 `--mcp-validation` 将 app-data 标识隔离到该测试 PID，避免读取或改写正常用户恢复点。两个标志都依赖 `--mcp`。
- 官方 SDK Client 经 stdio 完成 MCP 握手、ping、13 个工具发现，再经本地桥接读到 Rust 会话和页面投影，projectId/revision 一致。
- 实测媒体链接/探测、任务列表查询、错误 revision 拒绝、暂停、定位到第 0 帧、清空选择、锁轨、页面 revision 更新以及撤销。最终轨道内容恢复；测试会话的 dirty/history 变化仅存在于隔离配置。
- 本轮测试实例已停止，其过期连接文件已清理。后续请对正常使用的 KaiCut 加 `--mcp` 启动；未启动时工具会明确返回连接不可用，不会偷偷启动或控制其他软件。
- 这项验收证明实际原生进程与真实页面的 MCP 通路可用，不是截图验收，也未通过空项目证明所有媒体在设备上的解码性能。

该历史批次最终验证：前端 193 项、MCP SDK 协议 13 项、Rust 86 项通过（6 项已有设备环境用例默认忽略）；lint、cargo fmt、前端构建、Windows MSI/NSIS 调试打包和 diff 检查通过。原生桥接专项 10 项包含鉴权、边界、请求超时、UI 确认与连接文件清理；实际隔离进程的 smoke/live-check --exercise 均通过。该批次 `.kaicut` 与 FFmpeg 二进制没有升级。

主要文件：`mcp/*.mjs`、`src-tauri/src/mcp_bridge.rs`、`lib.rs`、`src/hooks/use-mcp-bridge.ts` 与测试、`src/stores/project-store.ts`、`src/types/project.ts`、`App.tsx`、`package.json`/锁文件、ESLint 配置，以及预览组件/解析器及其测试。许可来源记录已加入 THIRD_PARTY_NOTICES。预览规则和验证边界见 [PREVIEW-PARITY.md](./PREVIEW-PARITY.md)。

## 预览行为

时间线预览现在独立于片段选择，按播放头解析视频轨顺序、图片、黑场、独立音频与淡化。选中素材时进入素材模式；点击片段、拖动播放头或调用 MCP `ui_seek/ui_play` 会进入时间线模式。`ui_play` 表示启动预览时钟，实际解码状态可从随后 `ui_inspect.mediaNodes` 的 readyState、paused、currentTime 与 errorCode 检查。WebView 播放可能有解码/缓冲延迟，不声称等同 FFmpeg 离线逐帧结果。
