# KaiCut / 开剪

**让剪辑回到画面本身。** 本地优先的桌面视频剪辑工作台，当前先交付 Windows；macOS 等获得设备后再适配与验收，Linux 继续延后。

当前没有 Mac 设备，开发、截图、性能测试、安装包与发布门槛均以 Windows 为准；Apple 风格仅作为视觉参考，Windows 使用自身窗口、字体与系统交互。保留已有跨平台边界，不把未验证的 macOS 能力视为已支持。

当前已进入基础时间线编辑阶段，使用 Tauri 2 + Rust 桌面宿主，以及 React + TypeScript + Vite + Tailwind CSS v4 + shadcn-macos + Zustand 界面。

## 当前可用

- 顶部工具栏、素材库、静态首帧/图片预览、检查器、底部 2V + 2A 时间线。轨道内容区无网格。
- 原生文件对话框新建、打开、保存单文件 `.kaicut` JSON；后续保存覆盖当前项目文件。
- 导入 mp4 / mov / mp3 / wav / png / jpg / jpeg，探测媒体信息、按需生成缩略图，支持离线检测/重新链接。
- 素材搜索、列表/网格切换、选中素材查看元数据。
- 添加、单选、分割、删除、创建副本、移动/跨兼容轨道、裁切、吸附、缩放、锁轨、撤销/重做；检查器提供项目帧数输入。
- 严格 JSON/schema 校验，拒绝未来版本、无效引用、重叠和越源片段；取消和失败保留原项目，同目录临时写入后替换。
- 最近项目、独立恢复快照；合法 v1 空轨项目和旧恢复点可读入升级为 v2。
- 新建、打开、关闭时提醒未保存更改。
- 固定深色，应用内独立透明度与减少动态效果开关，偏好本地保存；旧浅色/系统主题设置会被忽略。
- FFmpeg sidecar 状态接口及可选打包配置，缺失时如实显示不可用。
- 素材预览与时间线预览分离；时间线画面/音轨按播放头解析，支持黑场、图片和音频尾段，规则与导出共用测试夹具。
- 可选本地 MCP：使用 `kaicut.exe --mcp` 启用，20 个结构化检查、播放、编辑、项目文件与导出工具。可指定路径打开、导入媒体、另存新项目，并分步预检、准备快照、启动和取消导出；打开拒绝丢弃未保存更改，另存与导出禁止覆盖已有文件。配置和分轮验收记录见 [MCP.md](docs/MCP.md)。

基础连续播放、定位、运行时波形、多选批量移动、按轨道波纹删除、主轨边界吸附和音频轨混音已接入；逐帧精度与长时同步仍待实机验证。Windows 导出支持选择位置、预检、提交快照、后台 H.264/AAC 编码、进度、取消及同次运行重试。首版限 64 个片段、1080p、双音轨混音，已有输出必须换名；音画自动关联、变速/关键帧和完整任务队列仍待实现。原始媒体只引用，不复制进项目。见 [导出实现与验收](docs/M3-EXPORT-PREFLIGHT.md) 和 [剪辑操作限制](docs/EDITING-OPERATIONS.md)。

## 环境

- Node.js 24，pnpm 11.25.0（由 `packageManager` 固定）。依赖精确版本与 `pnpm-lock.yaml` 已锁定。
- Rust stable / MSVC 工具链，最低 Rust 1.90；开发使用 Rust 1.97.1。
- Windows：Microsoft C++ Build Tools 与 WebView2 Runtime。
- macOS（延期）：获得设备后再准备 Xcode 工具链、构建、签名和公证；不阻塞当前 Windows 交付。
- 平台前置条件见 [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)。

## 启动与验证

```sh
pnpm install --frozen-lockfile
pnpm tauri:dev
```

仅预览网页：`pnpm dev`，地址 `http://127.0.0.1:5173`。浏览器模式只能下载项目副本，不能确认磁盘写入成功，也不能取得素材持久路径。桌面版使用 Rust 项目状态和原生文件读写。

```sh
pnpm lint
pnpm test
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
pnpm tauri build --no-bundle --debug
```

Windows 调试程序输出到 `src-tauri/target/debug/kaicut.exe`。这不是经过签名的发布安装包。macOS 构建和公证必须在对应平台另行验证。

MCP 验证使用 `pnpm test:mcp`、`pnpm mcp:smoke` 和 `pnpm mcp:live-check`。真实媒体闭环另用 `pnpm mcp:workflow-check --output-dir <src-tauri/target 下的目录>`，仅连接干净的临时项目并使用生成的测试素材；它会导入、剪辑、检查真实媒体元素、另存与重开项目，保留报告及源文件完整性记录。运行条件、连接参数和素材生成步骤见 [MCP 验证说明](docs/MCP.md#真实媒体工作流验证)，不以结构化状态代替截图或人工体验验收。

完整导出验收使用 `pnpm mcp:export-check --connection-file <隔离连接文件> --output-dir <src-tauri/target 下的目录>`，等价于工作流检查追加 `--export`。它先执行上述编辑/预览/保存闭环，再检查真实 H.264/AAC 成片、快照独立性、取消与临时文件清理；仅使用合成测试素材和隔离隐藏实例。具体结果单独记录于 [MCP 导出验收](docs/MCP-EXPORT-VALIDATION.md)。MCP 请求取消或超时不会自动取消导出任务，遇到不确定结果先查询任务列表/状态，再决定下一步。

后台验收使用 `--mcp-hidden`，同时关闭窗口初始显示与焦点请求。工作流逐次检查 `ui_*` 返回的 Rust 原生主窗口 `nativeWindow.visible=false`，并记录 `focused/minimized`；页面 `viewport.visibility/hidden` 属于 WebView 文档状态，即使文档报告 visible，也不能据此认定原生窗口显示。检查失败时脚本停止，不代替用户操纵窗口。

预览压力回归：`pnpm mcp:preview-check --connection-file <隔离连接文件> --output-dir <target下目录>` 检查多片段与视口裁剪，历史结果见 [预览性能基线](docs/PREVIEW-PERFORMANCE-VALIDATION.md)。新音频主时钟专项 `pnpm mcp:playback-check` 默认真实运行600秒；本机合成素材连续稳定段相对时钟差P95为9.91ms，开播/跳源仍有瞬时偏差及边界等待，完整M2尚未关闭，见 [音频主时钟验收](docs/AUDIO-CLOCK-VALIDATION.md)。两种验证均使用隐藏无焦点实例，不操作桌面。竞品实现线索与 REA 可用性见 [研究记录](docs/EDITOR-IMPLEMENTATION-RESEARCH.md)。

启动/定位专项使用 `pnpm mcp:startup-check`。统一准备和主音频实际起跑确认已接入，重复起播的一帧同步目标仍未全部通过；现有功能回归通过，后续将验证原生共享呈现时钟。完整数据与未通过项见 [启动验证](docs/PLAYBACK-STARTUP-VALIDATION.md)。

原生时钟探针独立运行：`pnpm native:clock-probe --output-dir <src-tauri/target内绝对目录> --seconds 20`。它只创建共享模式静音输出流，采样WASAPI位置并测试暂停、恢复和重置；不会开启摄像头/麦克风或替换主播放器。视频侧目前仅为合成PTS调度，不代表已完成原生视频呈现。范围、官方API依据和实测记录见 [原生时钟探针](docs/NATIVE-CLOCK-PROBE.md)。

真实媒体原生探针：`pnpm native:media-probe --output-dir <src-tauri/target内绝对目录>`。它用已审计sidecar解码4秒测试音视频，将PCM真正写入仅自身静音的WASAPI会话，并按设备时钟选择实际解码视频PTS；暂停、源定位和取消可重复验证。当前没有D3D呈现或主应用接入，实测及限制见 [原生媒体探针](docs/NATIVE-MEDIA-PROBE.md)。

D3D离屏验证：`pnpm native:d3d-probe --output-dir <src-tauri/target内绝对目录>`。使用真实硬件D3D11着色器绘制到纹理，并逐像素读回核对解码帧；音频仍经自有静音WASAPI会话。当前没有窗口、交换链或Present，主应用未接入，详见 [D3D离屏探针](docs/NATIVE-D3D-PROBE.md)。

异步队列验证：`pnpm native:queue-probe --output-dir <src-tauri/target内绝对目录> --render-delay-ms 120`。D3D渲染在独立线程执行，音频线程非阻塞提交帧；最多2项待渲染、1项在途和1项最新完成结果。普通与120ms人工慢渲染均已在本机验证音频供给、队列淘汰、epoch隔离及关闭回收。压力模式会主动丢弃旧视频任务，不代表无丢帧播放；主应用仍使用WebView播放器。范围和结果见 [异步帧队列探针](docs/NATIVE-QUEUE-PROBE.md)。

流式验证入口：`pnpm native:stream-probe --output-dir <src-tauri/target内绝对目录> --long-fixture-dir <已生成20秒素材目录>`。独立解码进程通过管道逐块供给PCM和带真实时间戳的视频帧，固定缓冲容量并显式管理准备、暂停、定位、EOF与取消。当前仅验收受控MP4/WAV、48kHz双声道设备及离屏渲染；定位会重启解码并顺序跳过源数据。输入准备、结果与边界见 [流式预览探针](docs/NATIVE-STREAM-PROBE.md)。

音频调度对照在同一入口追加`--audio-scheduling poll|event|event-mmcss`，默认仍为poll。事件模式按WASAPI要求请求系统默认缓冲，实际容量与轮询模式分别记录；`event-mmcss`仅在播放阶段为当前线程注册MMCSS Audio任务并在退出时恢复。它不更改系统计时器、全局音量或主应用后端。对照证据与限制见 [事件驱动音频探针](docs/NATIVE-EVENT-AUDIO-PROBE.md)。

图形复用与完整20秒验证追加`--renderer-lifetime scoped|shared --long-playback partial|full`，默认仍为scoped/partial。shared使两段验证会话借用同一图形worker，会话结束清理旧代次并确认空闲，整个探针退出才释放一次GPU资源；full将20秒源完整播放到末帧，再执行定位和取消。它避免重复创建/销毁，最终设备释放仍需独立验收。见 [图形复用探针](docs/NATIVE-REUSE-PROBE.md)。

1080p验证使用优化构建：`cargo run --release --locked --manifest-path src-tauri/Cargo.toml --features native-stream-probe --example native_stream_probe -- --output-dir <target内绝对目录> --hd-fixture-dir <已生成1080p/60秒素材目录> --renderer-lifetime shared --audio-scheduling event-mmcss`。独立参考记录逐帧真实PTS与内容哈希，输入、解码输出和GPU纹理均为1920×1080。该路径仍逐帧读回校验，未接入屏幕呈现；本机完整60秒功能通过，视频任务丢弃与顺序定位开销单独保留。见 [1080p探针](docs/NATIVE-HD-PROBE.md)。

定位对照追加`--seek-mode sequential|indexed-cfr`，默认保留sequential；`--seek-only true`仅在HD测试中缩短前段播放，再定位到30秒。indexed-cfr使用输入定位并按真实帧PTS过滤关键帧前缀，音频按整秒定位后精确裁去剩余采样；它显式限制零起点30fps和48kHz双声道PCM16 WAV，不是任意VFR/压缩音频的通用定位器。完整耗时包含停止旧会话、重开解码器和准备首帧。见 [定位探针](docs/NATIVE-SEEK-PROBE.md)。

素材兼容性使用独立`native_compat_probe`：`pnpm native:compat-probe --output-dir <target内绝对目录> --fixture-dir <受控兼容性素材目录>`。新的`IndexedPts`按完整真实帧时间戳选取画面，可验证VFR、非零视频起点与B帧；AAC从头解码后按PCM采样位置裁取。24项解码矩阵已通过，新增素材的声卡/GPU播放仍需单独验收。见 [兼容性探针](docs/NATIVE-COMPAT-PROBE.md)。

音轨偏移使用独立`native_audio_timing_probe`：`pnpm native:audio-timing-probe --output-dir <target内绝对目录> --fixture-dir <非周期音频素材目录>`。按实际首个解码音视频PTS计算相对起点，把固定窗口分为前置静音、连续源PCM和尾部静音；整数采样边界及非周期左右声道内容分别核验。它验证解码和时间映射，尚未接入连续音频输出。见 [音频时间映射](docs/NATIVE-AUDIO-TIMING.md)。

连续传输使用`pnpm native:audio-transport-probe --output-dir <target内绝对目录> --fixture-dir <非周期音频素材目录> --device false|true`。默认离线验证6组连续采样及定位，显式`true`另测一个自有静音WASAPI会话。游标只按实际接受量推进，保留部分提交的包尾，拒绝旧代次和重复确认；每个连续片段维持同一decoder。它仍是独立诊断，主播放器未切换。见 [连续音频传输](docs/NATIVE-AUDIO-TRANSPORT.md)。

设备会话控制使用`pnpm native:audio-session-probe --output-dir <target内绝对目录> --fixture-dir <非周期音频素材目录>`。`AudioPreviewAdapter`统一准备、播放、暂停、定位和取消，准备必须同时具备已填满的设备缓冲和下一包PCM；定位/取消在设备仍有排队数据时清空并隔离旧代次。独立静音设备验证及限制见 [音频预览适配器](docs/NATIVE-AUDIO-ADAPTER.md)。

音画协同使用`pnpm native:av-session-probe --output-dir <target内绝对目录> --fixture-dir <非周期音频素材目录>`。同一解码会话保留当前及下一视频帧，按真实WASAPI时间选择到期画面；音频和首个离屏视频结果都准备好才起播，暂停/定位清理旧渲染代次。该路径仍无屏幕Present，不能据此认定物理音画同步。见 [音画会话验证](docs/NATIVE-AV-SESSION.md)。

呈现接口使用独立`pnpm native:present-probe --output-dir <target内绝对目录> --fixture <160×90、120帧RGB夹具路径>`。硬件D3D11绘制到DXGI composition交换链并实际调用非等待Present，普通帧不再读回CPU；尺寸切换、零尺寸暂停和旧代次拒绝可验证。两轮本机测试每轮47次Present返回S_OK，6次抽样背缓冲像素核验一致。当前不创建窗口、不绑定visual，也不启动音频，因此这是提交接口验证，尚非可见预览或音画同步验收；主应用仍用WebView。夹具来源、构建及完整证据见 [呈现接口探针](docs/NATIVE-PRESENT-PROBE.md)。

音画呈现队列使用`pnpm native:av-present-probe --output-dir <target内绝对目录> --fixture-dir <非周期音频素材目录> --render-delay-ms 0`。现有WASAPI时钟驱动真实VFR选择，独立线程通过composition交换链提交画面；准备帧核验像素后才启动音频，普通帧无GPU读回。可追加`--render-delay-ms 120`验证有界队列在人工延迟下淘汰旧任务，定位/取消等待旧调用退役。本机正常两轮和压力轮通过，但交换链尚无可见窗口绑定，不能视为屏幕同步验收。见 [音画呈现队列](docs/NATIVE-AV-PRESENT.md)。

窗口绑定验证使用`pnpm native:surface-probe --output-dir <target内绝对目录>`。独立程序创建始终隐藏且不获取焦点的自有Win32窗口，将真实DirectComposition视觉树连接到既有交换链；两轮各28次Present、7次图案读回、尺寸调整、解绑/重绑和关闭验证通过。这里只验证窗口连接层，最小化与DPI变化仍是状态输入，尚未替换应用预览，也没有屏幕显示或音画同步结论。见 [原生预览宿主验证](docs/NATIVE-SURFACE.md)。

应用内原生预览连接已提供开发开关：构建`native-preview-host`并显式使用`--native-preview-host`启动，才在真实Tauri预览内容区创建原生子表面。单独使用这个运行开关时绘制校准图，用于验证画面范围、播放控制条避让、弹层暂停、窗口缩放与关闭。默认运行保持现有WebView播放。MCP的界面检查会返回该表面的实际状态，完整范围与隐藏实例验收见 [Tauri预览接入](docs/NATIVE-TAURI-PREVIEW.md)。

真实媒体接入已在隐藏Tauri验证入口打通：后台线程以WASAPI时钟选择FFmpeg解码帧，送入应用内原生子表面，并由GPU保持宽高比缩放和补黑边。受控素材播放、暂停、定位、遮挡恢复和取消证据见 [Tauri真实媒体验证](docs/NATIVE-TAURI-MEDIA.md)。

源素材控件现已接入该后端：额外传入`--native-source-preview`，素材库选择视频后可使用既有播放/暂停及秒数定位输入；支持自然片尾、精确片尾定位和重播。首批仅准入单视频流与48kHz双声道AAC的MP4/MOV，最长60秒、最大512MiB、最高1920×1080、方形像素且无旋转。失败时先确认原生会话关闭，再回退WebView；关闭失败保持错误态。两轮真实隐藏实例和旧媒体路径回归通过，源路径不保留PCM或做GPU诊断读回，默认启动和时间线仍用原播放器。可见画面、真实输入、听感和物理音画同步尚未验收。见 [原生素材预览](docs/NATIVE-SOURCE-PREVIEW.md)。

原生源预览的生命周期后续已通过三场景各两轮隐藏实例：快速A→B→A、准备中退出、播放中退出。打开登记与退出封门使用同一门闩，正常退出等待媒体线程与解码器收尾；这不意味着默认后端已切换或可见音画同步通过。证据与限制见 [源预览稳定性](docs/NATIVE-SOURCE-STABILITY.md)。

故障处理后续也已完成：暂停时解码器退出能被检测，原始解码错误与清理错误分别保留；资源未确认释放时阻止再开播放器。定位中切源、暂停解码故障、设备打开前错误注入各两轮隐藏实例与基础回归通过。真实设备拔插、可见输出和物理同步仍未验收。见 [源预览故障处理](docs/NATIVE-SOURCE-FAILURES.md)。

素材准备现支持按请求取消：离开或切换素材时停止对应探测，等待其进程与读取线程结束；清理失败保留停止状态。准备取消与关闭保护的三类隐藏场景各两轮、基础播放回归均通过，详见 [取消与关闭保护](docs/NATIVE-SOURCE-CANCELLATION.md)。后续已补齐设备实际打开后、首次准备循环中的取消及退出，各两轮隐藏实例通过，并回归源控件与旧媒体路径；检查发生在系统调用边界，不能强行打断卡住的驱动调用。见 [设备与首帧准备取消](docs/NATIVE-SOURCE-STAGE-CANCELLATION.md)，真实驱动故障和可见输出仍待独立验收。

原生画面提交的在途退役也已完成四轮隐藏验证：Present 返回后，关闭会让迟到结果进入 `Closed`，定位会让旧 epoch 结果进入 `Stale`；控制命令保持未完成直到受控停留释放，之后资源清理和新素材恢复播放通过。该批只证明队列退役与晚 ACK 隔离，不能中断已进入驱动的 Present，也不证明屏幕显示或物理音画同步。见 [在途提交退役](docs/NATIVE-SOURCE-INFLIGHT-RETIREMENT.md)。

清理失败处理进一步修正：Cancel/Finish不能把清理错误回复成成功，已取消的设备不再继续投喂；解绑失败保留原关联，已有窗口释放错误也会阻止兼容回退。两轮隐藏实例验证了suspend前受控错误、重复关闭拒绝和B被阻止，正常源控件与旧媒体回归通过；真实设备拔插/驱动故障仍待独立验收。见 [清理失败处理](docs/NATIVE-SOURCE-CLEANUP-ERRORS.md)。

原生绘制前已增加代次与关闭状态复查：已经过期的出队帧不会进入平台绘制，也不生成播放确认。关闭/定位各两轮隐藏验证通过，旧媒体与提交后退役回归通过。正常控件首次回归记录一次音频缓冲耗尽，同程序复测通过，原因仍待定位；保留失败记录，不视为稳定无断音。见 [提交前过期帧拦截](docs/NATIVE-SOURCE-PRE-SUBMISSION.md)。

音频欠载已补充有界发生点记录，区分启动调用和返回后的首次供音。固定三次正常控件运行未复现欠载，两次明确延迟对照均捕获真实缓冲耗尽；没有改变播放参数或降低零欠载标准。上批偶发问题根因仍未确定，后续继续用记录定位。见 [音频缓冲耗尽诊断](docs/NATIVE-SOURCE-UNDERRUN-DIAGNOSTICS.md)。

后续固定四实例、每实例六次近片尾启动，共24次未记录欠载，定位代次和解码器回收证据完整；正常控件回归也通过。这仅覆盖本机短素材，旧偶发问题仍未定位。见 [近片尾启动压力验证](docs/NATIVE-SOURCE-TAIL-STRESS.md)。

跨度压力验证随后使用同一份20秒固定夹具，按真实DOM流程覆盖约5秒暂停/恢复、10秒前跳、1秒回跳和片尾前200ms至EOF；两轮隐藏静音实例均完成，独立报告累计自然欠载为0。结果只适用于本机隐藏验证，不代表可见画面、听感、物理音画同步、长素材或跨设备资格；此前自然欠载记录仍保留，根因尚未确定。见 [原生源预览跨度压力验证](docs/NATIVE-SOURCE-SPAN-STRESS.md)。

59秒跨度验证已进一步完成：中点暂停/恢复到自然EOF、7.5秒与30秒定位及片尾播放，最终构建两轮累计自然欠载为0。验证入口改为确认真实素材卡片出现后继续，保留启动失败与旧二进制批次。该结果仍仅覆盖本机320×180固定素材，默认播放器不变。见 [59秒跨度压力验证](docs/NATIVE-SOURCE-LONG-SPAN-STRESS.md)。

## 本机字体

界面使用系统字体，时间码保持等宽。macOS 使用 `-apple-system`；Windows 使用 `"Segoe UI", "Microsoft YaHei UI", system-ui`。应用已移除用户字体别名和加载声明；`PingFangSC/` 不参与源码或构建，未卸载用户已有系统字体。

当前 Windows 桌面材质使用 Windows 11 Acrylic，只有工具栏与素材侧边栏透出背景，主要编辑区保持实色。macOS Sidebar vibrancy 分支保留但待设备后验证。开关复用现有透明度设置；系统减少透明度、高对比度和不支持设备采用实色。接口和平台边界见 [APPEARANCE.md](src-tauri/APPEARANCE.md)。

## 项目格式与状态

`.kaicut` 为 UTF-8 单文件 JSON v2，保存项目、外部媒体引用、轨道与片段（源入点/源长度/项目起点/片段长度均按项目时间基计帧）。最大10MiB、10000片段、24小时时间范围；不保存媒体字节、缩略图或撤销历史。合法 v1 空轨项目仅在内存升级，用户保存时才写v2；旧应用不能打开v2，请保留需要的v1副本。

桌面运行时 Rust 是项目的权威状态；Zustand 接收会话投影、控制操作繁忙状态与选择，另存 UI 偏好。浏览器模式提供独立演示适配，不参与桌面文件写入。接口见 [CONTRACT.md](src-tauri/CONTRACT.md)。

## FFmpeg sidecar

仓库包含 Windows x64 **LGPL FFmpeg/ffprobe sidecar 的构建配方、许可文本和验证记录**，不提交预编译可执行文件。配置保持 LGPL-2.1-or-later、关闭 GPL/nonfree/version3 与网络，不包含 libx264/libx265。本轮补齐导出拼接/混音滤镜和本地进度管道。媒体探测、缩略图、波形和导出使用 sidecar，基础预览播放使用 WebView2 媒体元素。编码器列表不保证每台设备的 Media Foundation 运行能力，失败会显示原因。macOS/Linux sidecar 尚未提供。构建配方、哈希和原始记录见 [交付说明](src-tauri/binaries/README.md)；本地构建前需准备对应 FFmpeg 源码和 Windows 工具链。

从 Git 源码构建时，预编译 sidecar 不会自动出现。需要在 MSYS2 UCRT64 环境准备 FFmpeg `n7.1` 对应源码和一个 48kHz 双声道 PCM WAV，然后设置 `FFMPEG_SOURCE_DIR`、`FFMPEG_INSTALL_PREFIX`、`FFMPEG_RECORD_DIR`、`FFMPEG_TEST_AUDIO` 并依次运行 `src-tauri/binaries/build-scripts/` 中的 configure、build、probe 脚本。验证后将 install 下的 `ffmpeg.exe` / `ffprobe.exe` 复制到 `src-tauri/binaries/` 的 target-triple 文件名，再使用 `pnpm tauri build --config src-tauri/tauri.sidecar.conf.json` 打包。二进制被 `.gitignore` 排除；不要把未经核验的其他构建冒充此记录中的版本。

H.264 编码与软件解码是两种能力。没有可用的已审查 H.264 编码器时，不能靠启用 GPL 编码器实现“软件回退”；应提示该导出预设不可用，或由用户选择经过验证的替代格式。

## 目录

```text
src/
  components/editor/   工作台、素材库、预览与时间线占位
  components/ui/       shadcn-macos 生成并补齐可访问性的组件
  hooks/               主题同步、弹窗焦点
  stores/              项目会话投影与偏好
  lib/                 JSON 校验与浏览器适配工具
  types/               项目/媒体结构
  styles/              KaiCut tokens、主题、上游样式及适配
src-tauri/
  src/                 Rust 项目、文件命令、sidecar 状态
  capabilities/        桌面能力约束
  binaries/            Windows FFmpeg/ffprobe 构建脚本、记录与许可证
public/kaicut-icon.png  KaiCut 用户提供的影片剪辑图标（透明角）
docs/                  PRD、架构、任务、UI 设计
```

`apple-web-design-skill/`、`macos-for-shadcn/` 是原有独立参考目录；不加入应用构建、包管理工作区或测试范围。

## 许可证

KaiCut 自有源码使用 **Apache-2.0**，全文见 [LICENSE](LICENSE)，版权与来源提示见 [NOTICE](NOTICE)。FFmpeg 单独使用 LGPL，二者不是对同一代码的双重授权。第三方组件保持各自许可，详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。`shadcn-macos@1.0.5` 的 npm 元数据未声明许可证，当前不能声称已完成对外发布许可审查。

## 规划

[产品](docs/PRD.md) · [架构](docs/ARCHITECTURE.md) · [里程碑](docs/TASKS.md) · [设计](docs/DESIGN.md) · [Tokens](docs/DESIGN-TOKENS.md)
