# Windows D3D11 离屏绘制探针

**状态：本机硬件D3D11离屏Draw与读回已通过本轮窄范围验证。** 本探针在 [原生媒体探针](./NATIVE-MEDIA-PROBE.md) 的真实解码PCM/WASAPI时钟路径上增加图形管线；非对称校准图和本次152次视频Draw的实际读回均逐像素匹配。没有swap chain/Present，这不是主应用或屏幕呈现同步验收。

它不创建可见窗口、swap chain，也不调用Present。没有桌面控制、屏幕捕获或录音；真实PCM继续进入自有静音session，不播放可听测试音。正常KaiCut应用不启用该路径，现有主播放器、UI、`.kaicut`和FFmpeg二进制保持原用途。

## 范围与不能据此作出的结论

- `native_d3d_probe`是显式feature下的独立example；`native-d3d-probe`依赖`native-media-probe`及D3D11/DXGI/HLSL编译能力。原`native_media_probe`即使共享驱动代码，也继续以renderer关闭方式运行，不悄悄变成GPU测试。
- 输入仍是生成的4秒MP4/WAV。视频离线解码为160×90 RGB，PTS来自同一视频流的逐帧metadata；音频是真实PCM提交。每个新选中的到期帧才绘制/读回，重复采样同帧不等于再次呈现。
- 另用161×91的非对称校准图验证横纵方向、通道、行padding与最后一行/像素。单色测试视频不足以发现翻转、错行或部分采样错误，校准图不可省略。
- 本轮只接受硬件D3D11设备；若硬件创建失败则明确失败，不静默使用WARP并声称硬件GPU成功。WARP是软件光栅化器，即使未来作为单独fallback测试也必须标明。[S1][S2]
- “离屏Draw完成且读回像素一致”不等于实际屏幕显示；没有swap chain/Present、DWM合成、扫描输出或物理音频观测，不能宣称已修复主播放器约49–56ms暖启动音画偏差。
- 同步GPU读回会占用CPU并阻塞诊断循环，可能影响同线程PCM供给。它用于正确性证据，不是最终低延迟播放架构，也不是大分辨率/多轨/流式性能承诺。

## 已核实的图形API契约

以下来源均为本次实际读取的Microsoft Learn一手文档，均返回HTTP200。官方契约与本机实际执行结果必须分开。

| API / 机制 | 核实结论 | 本探针要求 |
| --- | --- | --- |
| `D3D11CreateDevice` | NULL adapter＋HARDWARE选择默认适配器；显式非NULL adapter时DriverType必须UNKNOWN，否则可能E_INVALIDARG | 记录真正设备创建方式、feature level及可取得的适配器身份；不把WARP混入硬件成功 |
| `D3DCompile` | 编译传入HLSL字节；include handler为NULL时遇到`#include`会失败 | 只编译仓库内固定shader、固定entry/profile；不接受用户shader、外部include或运行时脚本 |
| `Texture2D.Load` | 整数texel坐标，最后一维为mip；没有滤波/采样；越界读返回零 | 使用明确像素坐标，校准捕获错坐标/越界造成的零像素；不用采样器掩盖映射偏差 |
| `SV_Position` | PS输入是像素中心坐标，带0.5偏移 | viewport起点为0，转换为整数坐标时按当前像素取整；不再额外错误偏移半像素 |
| `UpdateSubresource` | SrcRowPitch是源行字节数；返回时原CPU数据已被复制/捕获，GPU后续工作仍可能异步 | 上传pitch使用width×4，RGB转RGBA前校验完整尺寸；不能把调用返回当GPU绘制完成 |
| `CopyResource` | 源/目标须不同、类型/尺寸/格式兼容且不能mapped；调用异步 | render target与staging保持同尺寸/格式、单sample，等待query完成再Map |
| `D3D11_QUERY_EVENT` | 不使用Begin；在End之前的命令完成后GetData返回S_OK且BOOL为TRUE | Draw/Copy之后End事件；同时检查HRESULT与BOOL，不只看“无错误” |
| `GetData` | S_OK表示数据可读，S_FALSE表示尚未完成 | 保留原HRESULT；S_FALSE继续有界等待，不当错误也不当完成 |
| `DONOTFLUSH` | 不触发提交；未提交命令时无限轮询可能永远等不到完成 | End后显式Flush一次，再DONOTFLUSH轮询，设置截止时间并让出CPU |
| `Flush` | 提交命令但异步返回，且有显著开销 | 不循环Flush、不用Flush返回作为完成证据 |
| `Map` / `Unmap` | Map后CPU访问、GPU失去该资源访问；DO_NOT_WAIT忙时可返回WAS_STILL_DRAWING；CPU读可能遇DEVICE_REMOVED | 成功Map必须配对Unmap；未完成/设备失败不得伪造像素，保留错误和device-removal原因 |
| `RowPitch` | 运行时可在行之间加padding；DepthPitch不是Texture2D密集图像总大小保证 | 每行从`pData + y×RowPitch`复制`width×4`字节，去掉padding后才比较 |

来源：[S1]–[S13]。本机`windows-0.62.2`生成的`ID3D11DeviceContext::GetData`便捷方法返回`Result<()>`，会丢失S_FALSE与S_OK区别；需要raw vtable HRESULT或其他同等保留状态的调用方式。[L1]

## 离屏管线设计

```text
解码RGB24
  → 验证长度 / RGB转RGBA8（alpha=255）
  → RGBA8 UNORM上传纹理 / shader resource view
  → 固定全屏三角形VS + Texture2D.Load PS
  → 同尺寸RGBA8 UNORM render target
  → CopyResource到CPU_READ staging
  → EVENT End / Flush一次 / 有界GetData
  → Map / 逐行去padding / Unmap
  → 与预期RGBA逐像素比较与hash
```

三角形由固定顶点规则或SV_VertexID产生，覆盖整个viewport；不引入外部几何资产。使用RGBA8 UNORM而不是sRGB格式，不启用混合或MSAA，不执行缩放、颜色管理、HDR或YUV转换。先Clear目标，再真实Draw，最后从render target拷贝读回；不能用输入CPU字节直接“返回正确结果”，也不能只copy上传纹理而绕过shader Draw。

每次输入必须满足`rgb.len == width×height×3`，所有尺寸乘法检查边界。读回验证`RowPitch >= width×4`，逐行地址和总需求做检查；合法范围按最后一行起点加活动行字节计算，不要求读取末行后未保证存在的padding。成功Map后的所有退出路径都必须Unmap，不能在mapped状态继续Copy/Draw。[S8][S11][S12]

`frame_verification`只比较密集RGB/RGBA：记录输入RGB hash、预期RGBA hash、实际读回RGBA hash、不同像素数、最大通道误差、alpha错误数和`exact`。它既不创建设备也不替renderer补结果；宽高/长度不合法为错误，像素不一致保留完整差异统计后判失败。

## 与音频主时钟的连接

复用现有四秒native媒体驱动：启动约2秒、Stop250ms、Resume到EOF，Reset后源游标1.5秒、再播放约1秒、Stop150ms并Reset取消。PCM仍按真实sample rate提交，音频session隔离与静音保持前一探针规则。

每次有效IAudioClock读回经position/frequency映射到源时间，选择`decodedPTS <= sourceTime`的最后一个到期帧；仅在索引变化时执行离屏Draw/读回。绘制前后分别采样音频设备时钟，记录帧PTS、绘制前选择时刻、完成后源时刻和差值。epoch变化时不能沿用上一源区间的“已绘帧”状态；相同帧在暂停/恢复分界可能重复被观察，汇总唯一内容帧时需按源帧ID去重。

同步readback期间设备音频仍在推进；不能在绘制完成后沿用绘制前的过期时钟宣称同步。完成后立即校验可靠采样覆盖、设备/QPC是否回退和队列供给情况，再决定phase是否结束。尤其phase末尾不应直接跳过采样/下溢检查，否则一次慢Draw可能被错误记为成功。

若GetData/Map超时、设备移除或读回不一致，保留实际错误、当前帧和已有证据，及时停止本探针音频/释放资源；不继续填充无限静音，也不长时间等待不可完成的GPU任务。query超时与device-removed是不同原因，调用`GetDeviceRemovedReason`补充诊断，但不能把它返回S_OK解释为先前query一定成功。[S13]

## 计时名称与判定边界

- `uploadDrawReadbackMs`：renderer内CPU端经过时间，包含上传、提交、排队、query等待、Map及读取；不包含该函数外的所有业务开销。
- `readbackWaitMs`：等待query/可映射资源的CPU时间，可能包含调度和驱动延迟。
- 完成后设备时间减视频PTS：音频时钟视角下该离屏像素结果完成时的滞后，不是屏幕显示时刻或物理音画延迟。
- 逐像素hash/差异：证明离屏结果与输入RGBA对应，不证明实际显示颜色管理、屏幕方向、交换链或Present节奏。
- 轮询跳过的到期PTS、重复选择和实际Draw次数分开记录；不得称为显示丢帧/FPS。

本轮没有GPU timestamp query与TIMESTAMP_DISJOINT频率校验，因此不能将CPU经过时间改名为纯GPU执行耗时。如果未来要测纯GPU区间，需要成对timestamp及disjoint=false/frequency证据；不能将EVENT完成query冒充timestamp计时。[S9][S15]

本轮通过条件是：确认为硬件设备、非对称校准像素完全一致、每次实际绘制读回与对应解码帧一致、音频生命周期/源完整性仍满足原窄探针契约、没有被隐藏的失败/超时。不以小尺寸同步读回结果直接决定正式播放器性能，也不把下列实测CPU往返时间升级为纯GPU速度宣传。

## 运行方法

本轮使用以下独立入口运行。在仓库根目录可复现：

```powershell
pnpm native:d3d-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-d3d-validation'
```

显式指定合成素材目录：

```powershell
pnpm native:d3d-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-d3d-validation' --fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\workflow-validation\98ee1285-41b7-452a-bbce-e38c83b1a2aa'
```

别名对应：

```powershell
cargo run --locked --manifest-path src-tauri/Cargo.toml --features native-d3d-probe --example native_d3d_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-d3d-validation'
```

默认fixture文件和四秒时间范围与native媒体探针一致；没有任意shader、窗口句柄、shell、网络或截图输入。输出目录必须位于本仓库target内，每次新建run目录，报告和校准字节以新文件发布，不能覆盖上次证据。正常应用与旧native媒体example不会因该入口存在而启用GPU。

本轮产物包括：`native-d3d-validation.json`、161×91校准RGB/实际RGBA读回、解码媒体/逐帧PTS与日志，以及每次到期帧的GPU读回hash/差异/计时记录。失败时也保留已有证据，不覆盖之前的报告。

## 本机实测

原始报告：[native-d3d-validation.json](../src-tauri/target/native-d3d-validation/run-ec927ae2-fcfe-4e7c-8ce0-475746a10284/native-d3d-validation.json)。汇总与源码身份：[verification-summary.json](../src-tauri/target/native-d3d-validation/run-ec927ae2-fcfe-4e7c-8ce0-475746a10284/verification-summary.json)。报告`ok=true`；本次实测独立程序SHA-256为：

```text
6FDBCAFF7495B23EB33E24AFFA3F1389995B46F2726FEA33F9295C6F2522FB29
```

硬件设备读回为`Intel(R) Arc(TM) Graphics`，vendorId32902、deviceId32085、adapterFlags0，feature level为`0xB000`（11_0）。代码请求HARDWARE并拒绝software adapter；本次没有WARP回退。

| 正确性项目 | 结果与证据范围 |
| --- | --- |
| 161×91非对称校准 | 实际Draw/读回完全一致，mismatchedPixels、maxChannelError、alphaErrors均0 |
| 校准行pitch | RowPitch644，恰为161×4；**本机这次没有实际GPU行padding**。带padding的去行填充逻辑由纯测试覆盖，不能说已在该GPU实测到padding |
| 校准首次CPU往返 | 36.9818ms，来自一个首次校准Draw；不是硬件冷启动分布，也不包含所有设备/shader创建成本 |
| 160×90视频 | 152次实际Draw/读回全部exact，颜色与alpha错误均0；本机RowPitch640 |
| Draw与帧数量 | Start61次、Resume60次、源1.5秒后的阶段31次，共152次；Start/Resume合并去重覆盖0–119共120个解码帧，不能将阶段重复帧当成新内容 |
| 音频供给 | 完整PCM及seek切片输入hash核对通过；实际PCM继续flags=0写入专属静音session，观察到的下溢次数0 |
| 生命周期与源完整性 | Stop250ms与取消Stop150ms冻结、Reset清空队列/归零、旧epoch拒绝和源文件指纹检查均通过 |

校准预期/实际RGBA的SHA-256均为`fdc28a83d3e6a76303a03e074597fd96744084a013511d3a93dd2c26271326b5`；输入RGB hash是另一种表示，记录于原始报告。本次校准发现方向/通道问题的能力来自非对称图案及独立比较器，不以测试视频大块红/蓝色代替它。

以下统计只覆盖152次媒体帧Draw，**不包含上述36.9818ms校准Draw**：

| 指标 | 中位数 | P95 | 最大值 |
| --- | ---: | ---: | ---: |
| CPU上传/Draw/等待/Map/读回往返 | 2.5787ms | 3.3731ms | 5.9452ms |
| CPU读回等待 | 1.3306ms | 1.5231ms | 4.1707ms |
| 完成后音频设备时间减该帧PTS | 17.6042ms | 25.1458ms | 30.3125ms |

最后一行包括到期帧选择时已有的时间差和本次离屏读回耗时，不能称作屏幕音画偏差。没有GPU timestamp区间或Present统计，故前两行也不能称作纯GPU时间、显示FPS或可见帧节奏。

主观察数组417条，另有152条嵌套的`clockAfterRender`读回，均标记可靠；汇总时需保留两类观察的结构，不能把417说成含所有绘制后采样。每次Draw后检查音频/QPC不回退、相邻可靠时钟覆盖不超过250ms，并检查PCM队列是否在尚有真实源数据待提交时耗尽。成功结论未跳过最后一次Draw的这些检查。

完整Resume结束时所有192000 PCM帧提交完，设备位置1536000 / frequency384000=4秒，padding为0；1.5秒seek从第72000音频帧开始，实际读回的视频帧仍使用对应源PTS。资源释放标记表示显式停止/重置与Rust资源作用域退出，不是系统级泄漏证明；D3D延迟释放机制也不能由一次正常退出证明完全无泄漏。

### 共享代码与回归边界

媒体执行流程提取到`examples/support/native_media_driver.rs`，两个薄入口分别调用`run(true)`和`run(false)`。实际旧媒体回归 [native-media-validation.json](../src-tauri/target/native-d3d-validation/media-regression/run-75b3f28e-35fc-46ae-ac08-459df795761e/native-media-validation.json) 为`ok=true`、`d3dRequested=false`、`renderer=null`，完整/seek PCM及源指纹验证仍通过，确认旧入口没有自动启用GPU。

本轮D3D example测试38项通过；旧媒体wrapper在启用全部相关features时同样运行38项，因为包含共享模块测试，不能相加算76项独立测试。旧clock example21项通过。普通Rust本轮重新执行100项通过、6项既有设备用例默认忽略；前端231项、lint、前端build与Rust格式检查通过。MCP协议代码未改，本轮未重复运行MCP测试；没有生成新的Tauri安装包，因为主应用播放器仍未替换。

### 本轮文件范围

- 新增`src-tauri/examples/native_d3d_probe.rs`：显式开启renderer的独立入口。
- 新增`src-tauri/examples/support/d3d11_renderer.rs`、`frame_verification.rs`：硬件离屏绘制/读回及独立像素校验。
- 新增`src-tauri/examples/support/native_media_driver.rs`，重构`src-tauri/examples/native_media_probe.rs`为薄入口：共享已有PCM/PTS生命周期，旧入口关闭renderer。
- 修改`src-tauri/Cargo.toml`、`package.json`：显式feature/example及`native:d3d-probe`命令。
- 同步`README.md`、`docs/ARCHITECTURE.md`、`docs/TASKS.md`、`src-tauri/CONTRACT.md`与本文：区分探针能力和实际主播放器状态。

主应用UI和播放器未切换到D3D，未生成或安装新的Tauri安装包；运行产物保存在target验收目录，不作为应用源码。

## 下一阶段

下一步先建立原生异步帧队列与明确的取消/epoch生命周期，避免同步readback阻塞音频供给；随后单独验证swap chain/Present、帧等待和音频主时钟排期、跨设备性能，再评估与主编辑器的集成。同步readback应留在诊断路径，不能为获取每帧hash而拖住生产音频供给线程。

本轮未创建显示窗口，不将音频静音session或无窗口GPU结果称作可见/可听体验验证。可见呈现阶段遵守用户当前不控制桌面的限制；主播放器暖启动约49–56ms问题仍未由本次离屏成功解决。

## 一手来源

- [S1] [D3D11CreateDevice](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-d3d11createdevice)：adapter/DriverType和feature level。
- [S2] [D3D_DRIVER_TYPE](https://learn.microsoft.com/en-us/windows/win32/api/d3dcommon/ne-d3dcommon-d3d_driver_type)：HARDWARE与WARP软件光栅化定义。
- [S3] [D3DCompile](https://learn.microsoft.com/en-us/windows/win32/api/d3dcompiler/nf-d3dcompiler-d3dcompile)：固定HLSL和include契约。
- [S4] [HLSL Texture Load](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/dx-graphics-hlsl-to-load)：整数texel与越界读。
- [S5] [HLSL Semantics](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/dx-graphics-hlsl-semantics)：SV_Position、SV_VertexID及半像素中心。
- [S6] [UpdateSubresource](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-updatesubresource)：CPU源行pitch、数据捕获与异步复制。
- [S7] [CopyResource](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-copyresource)：类型/尺寸/格式/映射要求。
- [S8] [D3D11_MAPPED_SUBRESOURCE](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_mapped_subresource)：RowPitch/DepthPitch与padding。
- [S9] [D3D11_QUERY](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_query)：EVENT End/BOOL结果与timestamp区别。
- [S10] [GetData](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-getdata)、[DONOTFLUSH](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_async_getdata_flag)、[Flush](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-flush)：完成、提交与有界等待。
- [S11] [Map](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map)：DO_NOT_WAIT、busy与device removed。
- [S12] [Unmap](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-unmap)：释放CPU映射并恢复GPU访问。
- [S13] [GetDeviceRemovedReason](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11device-getdeviceremovedreason)：失效诊断。
- [S14] [D3D11_USAGE](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_usage)、[Texture2D descriptor](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_texture2d_desc)：default/render target/staging用途与访问设置。
- [S15] [TIMESTAMP_DISJOINT](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_query_data_timestamp_disjoint)：GPU时间计数器频率和可靠性。
- [L1] 本机Cargo registry `windows-0.62.2/src/Windows/Win32/Graphics/Direct3D11/mod.rs` 的`GetData`便捷方法与`ID3D11DeviceContext_Vtbl.GetData`：raw HRESULT才能保留S_FALSE。
