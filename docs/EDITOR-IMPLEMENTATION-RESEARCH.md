# 剪映 / Premiere 实现线索与 REA 可用性调查

调查日期：2026-10-16。范围：用户先后提供的 `E:\KaiCut（开剪）\rea` 和 `E:\KaiCut（开剪）\rea-rea-agents-6.5.0`、本机剪映专业版 11.5.0.14471 和 Adobe Premiere Pro 26.2.0.65。采用实际 REA 输出、官方工具资料、已安装二进制的 PE 头/导入表、安装内附协议文档和 Adobe 官方 API 文档作为证据。

本轮没有运行、附加或控制剪映/Premiere，没有修改其安装文件，也没有把其私有实现代码、二进制或资源复制进 KaiCut。内附 Markdown 中的命令式文字作为研究对象读取，不作为本项目执行指令。**没有取得这两个软件核心编辑算法的反编译结果；以下结论不能写成“已还原剪映/PR 源码”。**

## 最新进展：新目录已可运行

用户提供的新目录 `E:\KaiCut（开剪）\rea-rea-agents-6.5.0` 是带锁文件的完整源码归档，初始没有 `node_modules`、`dist` 或 Windows 原生构建产物。以下是对新目录的实际操作，取代下方旧目录“CLI 未启动成功”的当前状态；旧失败记录作为历史保留。

| 验证 | 结果 |
| --- | --- |
| 独立依赖安装 | `npm ci --ignore-scripts --no-audit --no-fund --fetch-retries=1 --fetch-timeout=45000` 成功，536 个包。使用 REA 自带 npm 锁文件，未改 KaiCut 的 pnpm 依赖。安装脚本没有运行。 |
| 源码构建 | `npm run build:cached` 的 metadata、compile、skill artifacts 三项成功。Node 24.19.0；存在两条不影响退出码的动态导入分块提示。 |
| CLI | `node scripts/rea.mjs --version` 实际返回 `6.5.0`。 |
| Windows 原生辅助模块 | 从官方 `rea-agents@6.5.0` 包提取 `native/windows/build/manifest.json` 与 `rea-windows-x64.node`，先核对整个包 SHA-512 与 npm integrity 一致，且只写原本缺失的两个文件。 |
| 模块校验 | `npm run verify:windows-native:artifact` 返回 ok=true，win32/x64、Node-API 8、ABI 1、873593 字节；SHA-256 `665c57ab90aef0712a6aafc423c7992a88bf608bea63b370cb30c695900f9aed`。 |
| MCP | 官方 SDK 客户端启动该目录的 `scripts/rea.mjs mcp`，握手/ping 成功，server=rea 6.5.0，发现 207 工具；`binary_session {}` 查询成功。测试客户端已关闭，没有改 Codex/Claude 等客户端配置。工具目录大小不代表所有 provider 可用。 |
| 深度原生分析 | 实际 `doctor --provider ghidra --json` 仍返回退出码 1：`GHIDRA_INSTALL_DIR is not set`；IDA MCP 也未配置。未发现可用后端，尚不能进行该路径的 xrefs/伪代码分析。 |

Windows 辅助模块负责受控文件/进程访问，并不是反编译引擎；补齐它不会自动获得 Ghidra。下一步要使用深度 native 路径，仍需可用的 Ghidra 12.1.x 与满足其要求的完整 JDK，或已配置的 IDA。没有修改系统 PATH、安装其它运行时或操作目标软件界面。

现在可在 PowerShell 直接使用：

```powershell
Set-Location 'E:\KaiCut（开剪）\rea-rea-agents-6.5.0'
node scripts/rea.mjs --version
node scripts/rea.mjs inspect-pe-resources 'F:\JianyingPro\11.5.0.14471\videoeditor.dll' --max-file-bytes 134217728 --json
node scripts/rea.mjs inspect-pe-resources 'E:\Adobe\Adobe Premiere Pro 2026\PlayerHost.dll' --json
```

### 实际 REA Evidence

两次 PE 资源检查退出码均为 0，源文件哈希与前次调查一致。剪映 DLL 约 71MB，超过工具默认 64MiB，因此显式设置 128MiB 文件预算；并非绕过格式检查。

| 目标 | REA Evidence ID | 结果 |
| --- | --- | --- |
| 剪映 `videoeditor.dll` | `ev_12c89e7397a241378bc8637a1662203916e629c5de63ed798c4795a9233bd210` | PE32+、x64，资源树检查 3 个目录项、1 个资源；资源树覆盖 complete。 |
| Premiere `PlayerHost.dll` | `ev_e2593d490bb95cd410ec57f90bbb89d66c02de5758b63d5e7a3bce4776ec9761` | PE32+、x64，检查 6 个目录项、2 个资源；资源树覆盖 complete。 |

provider 为 `rea-pe-resources` 版本 1，操作为 `inspect_pe_resources`，authority 为 `shipped-artifact`。**complete 仅指该资源树范围，不意味着函数或编辑功能分析完成。** 该操作不提供导入/导出函数及交叉引用，不能归功于它的结果不得写成 REA 证据。

### 新增函数入口线索：MSVC 静态符号与局部汇编

另外用现有 `dumpbin /exports` 和 `undname` 检查同一 DLL；这是 MSVC 证据，不是 REA 反编译结果。`videoeditor.dll` 保留 32568 个导出名，以下为不涉及 AI 功能的窄入口。RVA 是模块内相对地址，不能直接当成运行时虚拟地址。

| 模块内 RVA | 解码后的接口名 | 已确认的边界 |
| --- | --- | --- |
| `0x016EC7B0` | `CommonClient::commonSplitSegment` | 签名含 `CommonSplitSegmentReqStruct` / `RespStruct`。 |
| `0x016EDF50` | `CommonClient::cutSegment` | 独立的 `CutSegmentReqStruct` / `RespStruct`。 |
| `0x0173B970` | `MultiChoiceClient::rippleDeleteSegments` | 波纹删除有独立请求/响应类型。 |
| `0x0173BB80` | `MultiChoiceClient::rippleMoveSegments` | 波纹移动与波纹删除为不同入口。 |
| `0x01777300` | `ProjectClient::globalUndo` | 全局撤销入口。 |
| `0x017770C0` | `ProjectClient::globalRedo` | 全局重做入口。 |
| `0x0157FEE0` | `lvve::PlayerWin::Prepare(bool)` | 播放器准备接口。 |
| `0x0157FE10` | `lvve::PlayerWin::Play()` | 播放入口。 |
| `0x015818A0` | `lvve::PlayerWin::SeekTime(__int64, SeekMode, const std::function<void(bool)>&, bool)` | 带定位模式和回调参数的接口。 |
| `0x01581720` | `lvve::PlayerWin::SeekDoneWithCallback(__int64, void*)` | 另有定位完成回调相关入口。 |

这些名字和类型证明接口存在，不能证明请求字段、时间单位、线程切换、回调成功条件或具体剪辑算法。精确名称 `splitSegments` 未命中，实际命中为 `commonSplitSegment`，不把概念名伪装为符号名。

PE 头的 preferred image base 为 `0x180000000`。在 `0x181581890–0x1815818D9` 的窄静态汇编中，两种 SeekTime 入口都可见转入 `0x18157F620`；带回调版本随后存在一次间接调用。这是特定构建上的局部控制流观察，尚不能确定被调用实现和回调含义，更不能用“存在完成回调名”保证解码已就绪。KaiCut 应继续独立检查媒体 `readyState/seeking/currentTime`。

Premiere `PlayerHost.dll` 只有 28 个导出，多为 `OverlayRendererGPU` 与播放器初始化接口；本次导出名中没有对应 split/trim/ripple/clip/timeline/command 命中。这不代表 Premiere 没有这些功能，也不代表它们应当位于该 DLL。

完整本地输出保留在 `src-tauri/target/rea-validation-6.5.0/`：`summary.json`、两个 `*-pe-resources.json`、两个 `*-exports.txt`、`jianying-seektime-assembly.txt`、`mcp-session.json`、安装前后 doctor 与官方发行包完整性记录。该目录为不提交的分析产物；未把竞争软件代码接入 KaiCut。新工具目录也已通过 `.gitignore` 保持独立。

## 历史：旧 rea 目录的尝试

REA 是逆向分析工具的 MCP/CLI 编排层，不等同于独立的原生反编译引擎。官方 README 将 native binaries 的伪代码、汇编、调用和引用分析列为依赖 Hopper/Ghidra/IDA 的能力；静态 JavaScript/.NET、包清单等有其他独立分析路径。官方还提供调用已有 GNU objdump/Rizin 的接口，不负责安装这些 provider。[R1][R2]

本次检查与尝试：

| 项目 | 实际结果 | 含义 |
| --- | --- | --- |
| 初始 `rea/` | 只有 `.git`，无 HEAD、无工作文件，origin 为官方 GitHub 仓库 | 不是已经可运行的工具安装 |
| 官方 Git fetch | 常规浅抓取发生连接重置；HTTP/1.1 重试连接 443 超时 | 未补成成功 checkout，不应声称 clone 已完成 |
| 官方 GitHub API | main 当时解析到 `fe2a398dd9ab290ba6cb3411403d319cec8d07a2`，提交时间 `2026-10-16T10:49:10Z` | 固定本次阅读资料的版本锚点；不代表 npm 包包含同一提交 |
| 官方源码归档 | codeload 45 秒截止，只收到约 1.5 MiB/12 MiB | 未把半包当作有效源码解压 |
| 官方 npm 发布包 | `rea-agents@6.5.0` 下载成功，SHA-512 与 npm integrity 完全一致；曾在 `rea/` 解包并读取入口 | 下载来源可核验，不代表深度 native 分析可用 |
| 实际 CLI 尝试 | Node.js 24.19.0 执行 `node scripts/rea.mjs --version` 失败：`ERR_MODULE_NOT_FOUND: zod` | 未成功运行分析命令，不能把静态阅读结果归因于 REA 执行 |
| 独立依赖安装 | 仅在 rea 内尝试，关闭安装脚本和 optional/dev 依赖；运行期间目录再次变化，仅剩 `.git`，遂中止自有安装进程 | 未把独立工具加入 KaiCut 的依赖或构建；目录来源变化尚未归因，停止继续写入避免与外部操作冲突 |
| Native provider | PATH 与 C:/Program Files、C:/Program Files (x86)、E:/、F:/ 的常见顶层目录未发现 Hopper/Ghidra/IDA/Rizin | 这是有限位置检查，不等于全盘证明“未安装”；没有伪造 provider 配置、许可证或分析服务 |

REA npm 包：`https://registry.npmjs.org/rea-agents/-/rea-agents-6.5.0.tgz`。本次下载验证值：

```text
SHA-256 531717A06E92CA651AAB52691190F916FAC044272C01F875E22C5E293218E699
SHA-512 base64 C6BG7LyrAdR74mSjertM6PrxJkcNZ2bfQNOJornPo9erDEp2tyPRVYPnV5Yodr+1RqewCQla68WulXlVnveD+A==
```

官方 package.json 的 Node 要求为 `^22.19.0 || ^24.11.0 || >=26.0.0`，许可证为 MIT。REA 不授予外部商业反编译器许可证；本轮没有进入 setup、注册新的 MCP、下载 provider 或启动其它软件。[R1][R3]

## 本机目标身份

版本由 Windows 文件版本信息读取；SHA-256 对下列现存文件计算，未执行目标二进制。PE 工具为现有 Microsoft Build Tools 的 `dumpbin.exe`，路径 `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\dumpbin.exe`，仅使用 `/headers` 与 `/dependents`。

| 文件 | 文件版本 / 字节数 | SHA-256 |
| --- | --- | --- |
| `F:\JianyingPro\11.5.0.14471\JianyingPro.exe` | 11.5.0.14471 / 438552 | `F77353701EFD7BC3E010B054E61A48AB78CC2F4413407109989FE8447CF622CC` |
| 同目录 `VECreator.dll` | 11.5.0.14471 / 269924632 | `E6562ACF9F4FE7D8CB74A9B81483D970599F738572B837E061318D58D2E7CCF5` |
| 同目录 `cccreator.dll` | 22.2.0.99999-revision.c0e28546 / 158351640 | `78132ED2D0C50894DA87425EA7451895C4394EC5813B4E28DF57EA54283ADF02` |
| 同目录 `videoeditor.dll` | 无文件版本字段 / 71015192 | `37CADEF37DAFF82E2ECDCD65080B9CBD7F6F9436F5E56C1FFB90C52C408EB7FB` |
| `E:\Adobe\Adobe Premiere Pro 2026\Adobe Premiere Pro.exe` | 26.2.0.65 / 7548424 | `B9ADDEFEA1C6EAE6E39550C5A33575CBE9ECEAF92D51B008CB8046E8C39C60C3` |
| 同目录 `PlayerHost.dll` | 26.2.0.65 / 12170248 | `3E51AA878900C236608C74CD29417B10C7A068666556B95AF8C6CF805ACEB237` |

`cccreator.dll`、`videoeditor.dll`、`PlayerHost.dll` 的 PE 头均为 x64/PE32+，CLR/COM Descriptor Directory 为零；因此这几个文件不能直接当作普通 .NET 托管程序集交给仅静态 .NET 分析路径。不能据此推断整套软件没有脚本或托管模块。

## 能确认的实现边界

### 剪映专业版

| 证据 | 确定结论 | 不能推出的结论 |
| --- | --- | --- |
| `JianyingPro.exe`、`VECreator.dll` 的静态依赖表只列 `gpShell.dll` | 这个导入表暴露的信息很少，主 EXE 不是一个足够的算法入口 | 不能仅凭文件名断言具体加壳、解密方式或原生调用图 |
| `videoeditor.dll` 导入 `cccreator.dll`、FFmpeg avcodec/avfilter/avformat/avutil/swscale/swresample、`clipflow_sdk.dll` | 编辑相关模块依赖媒体编解码/滤镜库及另一原生模块 | 不能得出分割、磁吸、波纹操作由哪一个函数实现 |
| `cccreator.dll` 导入 FFmpeg、`audioeffect.dll`、`lens.dll`、D3D11/DXGI/DXVA2/OpenGL、libvpl 等 | 存在音效、图像与多图形/视频接口依赖 | 不能证明本机某个时间线预览实际走哪一个 GPU 路径或其缓存策略 |
| 同目录 Qt6Core/Qt6Qml 文件版本均 6.2.2.623；既有静态审计记录了 QML 命令标识 | 包含 Qt/QML 界面相关组件，和 CEF/其它运行时组件共存 | 不应说整个产品“只有 Qt”，也不能因此否定 KaiCut 的 WebView 路线 |

与前次字符串审计的关系：`docs/JIANYING-BEHAVIOR-AUDIT.md` 已记录播放、分割、普通吸附/主轨磁吸等命令线索。本轮没有把旧字符串证据升级为已反编译算法。[L1]

安装内附 XMLDraft 参考给出更具体的协议事实：[L2][L3]

- 主轨 `main` 是按顺序排列的 video/image；副轨 `multi` 使用绝对时间或相对边界对齐。主轨顺序和副轨定位是不同模型。
- 片段 ID 与素材引用分开，同一素材可被多个不同片段复用；局部时序编辑按片段 ID 定位。
- 协议区分源入点 `material_start`、时间线 `duration`、速度 `speed` 和副轨 `start`/对齐字段；该 XML 接口使用毫秒。
- 图片没有可裁切的源时间范围；图片时长属于时间线，而非伪造视频源时长。
- 音频 volume 在这份 XML 协议中以 100 表示原始音量；副轨可绑定主轨左右边界。该比例约定不意味着 KaiCut 必须更改已有 0..1 gain 格式。
- 同一份附带文档明确限制该 XMLDraft 接口新增 fade。**这只能说明接口覆盖边界，不能说剪映应用不支持淡入淡出。**

这些是安装发布的 agent/XML 协议，而不是证明原生持久化数据库、交互默认行为或内存图结构的源码。尤其不能据“接口毫秒”断言内核不用整数帧/高精度时基；KaiCut 应继续保留已验证的有理帧率和整数帧模型。

### Adobe Premiere Pro

静态导入表显示：主程序依赖 `Frontend.dll`、`PRM.dll`、`dvacore.dll`；`PlayerHost.dll` 依赖 `Backend.dll`、`ImporterHost.dll`、`VideoRenderer.dll`、`ImageRenderer.dll`、`VideoFrame.dll`、`AudioSupport.dll`、`Memory.dll`、`MediaFoundation.dll` 等。这支持“前端、播放、导入、渲染、音频具有分模块边界”的判断。这里的 `MediaFoundation.dll` 是安装内模块名，不可直接等同于 Windows Media Foundation API；仅凭依赖表不能还原播放调度或线程模型。

比依赖名更可靠的操作模型证据来自 Adobe 官方 UXP API：[A1][A2][A3]

| 官方 API | 可以确认 | KaiCut 可独立采用的原则 |
| --- | --- | --- |
| `SequenceEditor.createInsertProjectItemAction` | 插入是一类明确 Action；轨索引超过已有轨数时 API 可以创建轨道 | 插入与移动/覆盖分开建模，轨道创建另设规则 |
| `createOverwriteItemAction` | 覆盖编辑与插入是不同入口 | 不把所有拖放统一解释为普通移动 |
| `createRemoveItemsAction` 的 `ripple` 参数 | 删除可明确选择是否波纹 | 删除、波纹删除应有可测试的不同语义 |
| `createCloneTrackItemAction` | 克隆可以选择插入或覆盖，用偏移确定目标轨道/时间 | 多选复制/移动保留相对时间和轨道偏移 |
| `Project.executeTransaction` / `lockedAccess` | Action 组合与受保护的项目访问是公开接口概念 | 一次用户动作对应一次原子提交/撤销；继续复用 Rust 会话锁与 revision |
| `TickTime.createWithFrameAndFrameRate` / `createWithTicks` | 帧率与高精度时间类型显式建模，ticks 支持字符串形式 | 避免浮点秒累积误差，不为模仿其它接口而降低时间精度 |

默认快捷键文件本身也是结构化命令表：`Keyboard Shortcuts/en/Adobe Premiere Pro Defaults.kys` 含 context、commandname、shift/alt/ctrl 和 virtualkey。已直接定位以下命令：撤销、rippledelete、snap、razor、当前播放头 add edit/all tracks、向前/向后 ripple trim。[L4]

其中字母型 virtualkey 呈 `0x80000000 + 字符码`：例如 razor=`2147483715`、当前播放头 add edit=`2147483723`，其 Ctrl/Shift 修饰字段分别区分操作。**这些字段原样存在是确定事实，组合键的人类解释仍需键码规范或运行时验证。** 此轮 Adobe Help 快捷键网页返回 Access Denied，没有把本机自定义键码直接等同于 Win32 VK，也没有宣称与剪映默认键位相同。

## 推荐转化为 KaiCut 的下一步

1. **先稳定已有路径和性能证据。** 用现有 MCP 的真实项目、播放、保存和导出状态建立基线，记录不同片段数量下的操作耗时和失败原因；附带文档与 DLL 名字不证明性能方案。
2. **整理统一编辑命令层。** 将工具栏、菜单、快捷键、MCP 都映射到同一命令及 enabled/disabled 规则，涵盖输入框、模态层、播放状态、锁轨、选择集合；保持 Rust 事务/单步撤销契约。该原则与两者可核实的命令/API 边界一致，无需复制私有实现。
3. **将主轨“边界吸附”与真正的顺序主轨编辑分开。** KaiCut 当前 `video-1` 的吸附不等于剪映 `main` 协议。若增加插入/覆盖、自动关闭主轨空隙或副轨关联，先定义操作对其它轨道、锁定、成组和撤销的影响，再写边界测试，不直接改现有 `.kaicut` 语义。
4. **一批只扩一个编辑语义。** 优先候选是明确的 insert/overwrite 或播放头裁切命令；不要同时承诺完整主轨磁性、音画关联、速度曲线和复杂转场。
5. **深度 native 分析作为独立后续任务。** 新目录的 REA 6.5.0 已运行并产生真实静态证据；下一步配置可用 provider，再沿上述 `commonSplitSegment`、`rippleMoveSegments` 或 `SeekTime` 入口限定问题。没有 provider 和函数级分析证据时，不声称已还原核心算法；不需要让 KaiCut 依赖该研究工具。

本轮形成的是实现边界和可验证的行为模型，没有更改 KaiCut 的业务代码。对性能的下一批实际测量由对应验收记录单独报告。

## 来源与复核入口

- [R1] [REA 官方 README，固定提交](https://github.com/morluto/rea/blob/fe2a398dd9ab290ba6cb3411403d319cec8d07a2/README.md)：功能/provider/平台范围。
- [R2] [REA 官方逆向工具文档](https://github.com/morluto/rea/blob/fe2a398dd9ab290ba6cb3411403d319cec8d07a2/docs/reverse-engineering-tools.md)：objdump/Rizin 需调用方安装，输出与能力边界。
- [R3] [REA package.json](https://github.com/morluto/rea/blob/fe2a398dd9ab290ba6cb3411403d319cec8d07a2/package.json)、[npm 6.5.0 元数据](https://registry.npmjs.org/rea-agents/6.5.0)：版本、Node 要求、依赖与完整性；本次也直接阅读了发布包 `scripts/rea.mjs` 与 `scripts/prepare.mjs`。
- [A1] [Adobe SequenceEditor](https://developer.adobe.com/premiere-pro/uxp/ppro_reference/classes/sequenceeditor/)：本次 HTTP 成功读取的 insert/overwrite/remove/clone Action 描述。
- [A2] [Adobe Project](https://developer.adobe.com/premiere-pro/uxp/ppro_reference/classes/project/)：executeTransaction / lockedAccess。
- [A3] [Adobe TickTime](https://developer.adobe.com/premiere-pro/uxp/ppro_reference/classes/ticktime/)：ticks 与帧率创建方式。
- [L1] [前次剪映静态行为审计](./JIANYING-BEHAVIOR-AUDIT.md)：旧证据及其限制，不作为本轮反编译结果。
- [L2] 剪映安装目录 `Resources/canvas_agent/skills/studio.lyra.xml_timeline_builder/references/xml/core-track-and-fields.md`，轨道/ID/时间章节起始行 23/33/48/66；SHA-256 `716EA926BD8D708A1FFEF076678621C74F0E53B58E04975E83357C383E6CD1C0`。
- [L3] 同目录 `audio-sound-clips.md`，volume 第 19 行、fade 边界第 51 行；SHA-256 `9877B06B59E3729BF93CC8BCC75355E831147FD8217E7C689D3FAF3CE94221BA`。
- [L4] `E:\Adobe\Adobe Premiere Pro 2026\Keyboard Shortcuts\en\Adobe Premiere Pro Defaults.kys`；SHA-256 `44B4BFC0BC382DF8BB703C3059B4FF2AA71285671CF02596C1FD27571583163F`。本轮按 XML 解析命令/上下文/修饰键，未改文件。

复核二进制依赖时使用 `dumpbin /nologo /dependents <明确文件>`；使用 `/headers` 复核架构与 CLR 目录。它们都是静态读取，不运行目标，不应把“依赖存在”升级成“特定代码路径运行过”。
