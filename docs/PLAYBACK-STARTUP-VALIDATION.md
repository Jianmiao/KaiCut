# 起播与定位衔接验证

日期：2026-10-16。继续方案二的渐进媒体内核路线。本批只处理现有WebView预览的准备、起播和定位时序，保持Rust项目权威状态、项目格式、导出及界面设计不变。此前连续稳定段的十分钟结果见 [AUDIO-CLOCK-VALIDATION.md](./AUDIO-CLOCK-VALIDATION.md)，它不能替代本批起播质量验证。

## 测量方法

新增 `pnpm mcp:startup-check --connection-file <独立连接文件> --output-dir <target内目录>`。使用前次生成的610秒合成视频与音频，只读复用源文件。仅连接干净、未保存、空白的隐藏无焦点隔离Tauri实例，不操作用户桌面。

脚本先导入并保存素材引用，再打开14秒测试时间线，分别执行：5次起播、完整连续/跳源/空隙/换主序列，以及3次播放中的正反向定位。每次结束都检查暂停320ms稳定性。每次起播和定位采样约1.9秒，每次请求后最多3秒必须观察到真实视频与音频源时间均前进，不能仅凭playing或播放头判定成功。

每次读取保存真实nodeId、currentTime、readyState、seeking、paused、phase、主钟身份、速率和硬seek计数；期望片段/源坐标由项目独立解析。MCP有自身往返与约32ms页面等待，按约65ms请求节奏采样也不等于65ms精确事件采集。

功能与质量分别报告。质量统计包含每一个就绪且未暂停的音视频对，包括首批样本和边界前后，不再排除开头5秒；P95和最大相对媒体时钟差都需小于30fps的一帧。暂停/准备中的节点保留原始记录，并另记主节点已播、从节点未播的观测窗口与实际源时间首次推进，避免通过推迟从节点而隐去等待成本。

## 复现记录

| 版本 | 功能 | 同步质量 | 可核实结论 |
| --- | --- | --- | --- |
| 上一批音频主时钟基线 | 9个场景通过 | 9个场景均未达一帧门槛 | 首次起播最大相对差176.0ms，其后4次58.7–73.9ms；3次播放中定位54.5–64.6ms。 |
| 统一准备后同轮放行 | 9个场景通过 | 9个场景仍未达一帧门槛 | 首次起播最大158.1ms，其后57.1–64.9ms；定位55.1–59.1ms。单纯ready屏障不能保证两个播放请求同时生效。 |
| 准备屏障＋主音频实际起跑确认（本轮最终实现） | 9个场景通过 | 首次起播通过，其余8个场景仍未达一帧门槛 | 首次起播最大30.9ms，其后4次49.3–56.5ms；定位43.7–50.5ms，边界序列最大54.2ms。 |

这两组各一次本机运行，仅支持该负载的比较，不是跨设备性能承诺。第一次起播到3帧且两个媒体实际推进的观察时间约768ms→393ms，其余用例未统一变快。

实际中间版本trace：请求后约90/148ms，视频源时间已为10.033/10.091秒，音频仍为10秒；两个节点均报告readyState=4、paused=false。约232ms音频才到10.015秒，视频已到10.173秒。由此可确认“已就绪/未暂停”不能等同“媒体时间已开始推进”，这也是下一次改动的具体依据。

## 当前实现与最终结果

统一准备屏障已实现：定位到同一项目位置并等待所有活跃节点元数据/seek/缓冲就绪，媒体子节点就绪事件不能自行播放；连续同源剪切保持原节点，非连续区间重新准备；暂停或更新定位撤销旧放行，准备超时返回明确错误。

最终实现先启动主节点，确认当前代的 `play()` Promise成功、真实currentTime已前进、节点仍就绪，才放行从节点。从节点仍从原准备位置开始，没有通过跳过片段开头或重写测量值制造同步。准备/启动各受同一个10秒截止时间约束；暂停、项目/revision变化和新seek撤销旧请求。source模式保持独立原始素材播放。MCP新增真实 `preview.phase` 读回，不根据playing猜测准备已完成。

本机首轮冷启动：首次双节点实际源时间推进在请求后约231ms被观察到，推进到3帧且两流前进约396ms；采到了2个“主节点未暂停、从节点仍暂停”的样本，估计窗口约141ms。这不等同141ms可听声音独奏，因为HTML paused=false也不证明设备已经输出；该成本完整记录，未从报告删除。

重复启动仍失败的证据：第二次起播约94ms时视频/音频为10.024174/10.015894秒，差8.28ms；约152ms变为10.081984/10.032365秒，差49.619ms。主音频已有一次进展后仍有短暂时间推进差异，两个节点均ready、未暂停。当前观测不足以可靠承诺共时启动；不能由此断言所有WebView方案必然不行，但继续添加固定等待、更多进展次数或设备相关补偿缺乏稳定依据。

**结论：启动/定位功能和生命周期回归通过，严格起播同步质量未通过。** 本轮保留准备屏障、请求代次隔离和真实起跑确认，不宣称完整修复一帧目标。统一准备、确认和速率校正产生的额外等待不会隐藏。

## 验证结果与后续路线

| 检查 | 本轮结果 |
| --- | --- |
| `pnpm test` | 20文件、231项通过。 |
| `pnpm test:mcp` | 20项通过。 |
| `pnpm lint`、`pnpm build` | 通过；build由最终Tauri打包的beforeBuildCommand执行。 |
| `pnpm tauri:build --debug --config src-tauri/tauri.sidecar.conf.json` | MSI/NSIS调试打包通过。常规链接器创建库消息仍被Rust报告为warning，退出码0。 |
| `pnpm mcp:startup-check ...` | 基线、第一迭代、最终迭代均保存完整报告；最终9个功能场景通过，严格质量8/9失败，脚本正确返回退出码1。 |
| `pnpm mcp:playback-check --seconds 30 ...` | 最终版本30.44秒稳定播放和14秒边界功能通过；稳定相对差P95 18.094ms、最大18.137ms，未采到缓冲，连续段硬seek为0。不是新十分钟验收。 |
| `pnpm mcp:workflow-check ...` | 11组实际导入、剪辑、图片/淡化、保存/重开、播放/暂停及源完整性检查通过。 |
| `git diff --check` | 通过。 |

本轮未改Rust源码、FFmpeg或项目schema，未重新运行Rust测试；前次十分钟结果保持历史版本证据，没有改写为当前版本的新运行。结束前确认测试实例隐藏、未获焦点、未播放、项目干净、无导出任务；自有实例与连接描述文件已清理。没有操作用户桌面或竞品。

下一批改做Windows原生共享呈现时钟探针：先验证WASAPI音频输出位置、解码PCM/视频PTS到同一时间基的映射、暂停/定位代次与取消清理，再评估D3D呈现。验收必须比较实际音频输出位置与视频调度，并保留开播/定位全部样本。仅将代码移到Rust而继续保留两个自由运行时钟不算解决。首批探针独立于现有UI，能力未验证前不替换当前播放器。

## 证据与限制

- 基线：`src-tauri/target/mcp-startup-validation/run-0dd82d26-4100-4cd4-993b-fb8a8eb486e0/startup-validation.json`。
- 仅准备屏障版本：`src-tauri/target/mcp-startup-validation/run-a92d3857-9dfc-4b2e-b5ec-6b6b23aefd59/startup-validation.json`，旁有程序哈希记录。
- 最终启动验证：`src-tauri/target/mcp-startup-validation/run-a146a276-97aa-41a0-96a5-7d13adb93137/startup-validation.json`；`functional=passed`、`quality=failed`。
- 最终稳定播放短测：`src-tauri/target/mcp-startup-validation/run-02822b40-f482-4c6e-9aaf-d1b2d1baee79/playback-validation.json`。
- 最终原有工作流回归：`src-tauri/target/mcp-startup-validation/run-e2d1f34b-d29f-41df-87d0-ecef0326ed1f/validation.json`。
- 源素材：`src-tauri/target/mcp-playback-validation/run-426f5185-9799-4791-8e79-b426aed44bb3/`。

最终打包后的 `src-tauri/target/debug/kaicut.exe` SHA-256：`1DC96047401825A9AF5B9A83DE40F2FD14EEBC70C1954BD15CFB9D1D6C592BC9`。打包前验证程序的hash保存在最终启动报告旁的`build-context.json`；Tauri写入打包类型后hash变化，前端构建内容相同。调试包NSIS为6270930字节、MSI为11034624字节，本轮未安装。

修改文件：`src/components/editor/preview-area.tsx`及测试、`src/hooks/use-mcp-bridge.ts`及测试、新`mcp/startup-check.mjs`、`package.json`、README、MCP/原生命令契约、任务/预览规则与本文。没有增添新编辑功能模块。

上述均是隐藏WebView的媒体presentation clock，不证明物理声卡输出或可见画面呈现。若当前后端仍达不到门槛，应保留失败数据并把原生时钟/解码调度探针列为下一步，不能把目标放宽后宣称问题解决。
