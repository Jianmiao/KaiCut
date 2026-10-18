# Windows 原生音频预览适配器验证

本轮承接 [连续音频传输验证](NATIVE-AUDIO-TRANSPORT.md)，将 PCM 传输与 WASAPI 控制收拢到同线程音频预览适配器。目标是统一准备、播放、暂停、定位、取消与失败清理语义，让之后的预览会话有明确的调用边界。

本轮仍是独立、显式运行的诊断程序，不接入主应用、UI、MCP 或项目格式，不创建 GPU 或可见窗口。真实设备验证只使用自有静音 WASAPI 会话；不控制桌面、不录音、不改变系统音量或默认设备。模拟后端验证错误路径，真实设备不主动制造系统故障。

## 状态与控制规则

状态为 `Preparing`、`Ready`、`Playing`、`Paused`、`Cancelled`、`Failed`。准备阶段可解码和预填设备缓冲，不能自动 Start；设备缓冲填满且下一包 PCM 已准备好才进入 `Ready`，随后仍需显式开始播放。下一包复用单个待提交槽，预取本身不推进接受游标。新包准备长度不超过设备容量；只有设备实际接受 PCM 前缀，传输游标才能提交相同帧数。

暂停执行 Stop，保留设备 padding 与尚未提交的 PCM 包；恢复从同一序列继续，不执行 Reset。播放中定位必须先 Stop、Reset 并确认旧设备排队数据清空，再让传输增加代次、丢弃旧包、关闭旧解码器并建立新源位置。定位回到 `Preparing`，完成预填也不自动恢复播放；旧代次确认必须被拒绝。

取消在有排队数据时仍执行 Stop、Reset、关闭自有 decoder，进入不可恢复终态；重复取消应安全。后端控制或提交步骤失败进入 `Failed`，尝试 Stop、Reset、关闭 decoder，即使前一步清理失败也继续后续清理。保留原始失败与各清理错误，不能用清理错误覆盖根因。主动取消允许将 `Failed` 转为 `Cancelled`，但原始 `failure` 仍保留，不能恢复播放；取消清理失败会返回错误，重复取消不能伪报清理成功。错误阶段调用与无效请求直接拒绝，不等同于后端运行失败。

特别是设备写入成功、传输 commit 随后失败时，PCM 已实际进入端点队列；必须终止并清空设备，不能只返回错误后继续旧状态。失败或取消后的迟到控制不得复活会话。

设备可能在读取 padding 与完成解码准备之间继续消费数据。适配器将本次提供的 PCM 限制为首次观察到的可用容量，并保留完整传输 ticket 做部分提交，避免将设备随后合法接受的较长前缀误判为越界。模拟后端覆盖该消费时序。

## 验收与证据解释

模拟设备/传输验证顺序、零与部分接受、暂停保留、定位清空、旧 ticket 拒绝、取消幂等，以及 prepare/write/commit/start/stop/reset/seek/close 等失败后的完整清理尝试。成功与失败都核对状态和调用序列，不能仅验证返回错误。

真实设备使用既有非周期偏移 AAC 与零起点视频，检查准备不播放、源 PCM 阶段暂停/恢复、播放中多次定位及仍有排队数据时取消。每段保存实际提交 PCM 摘要或有界原始捕获、源锚点、代次、设备位置、padding、调度状态和解码进程回收结果。定位目标与最终数量以实际报告为准，不预先宣布运行通过。

捕获的是设备接受的数据，包含可能仍在 padding 中、随后被定位或取消清掉的帧；它不是扬声器录音，也不代表每个接受帧已播放。每次 Reset 的前后 padding、时钟及丢弃量应分开报告。MMCSS 只在实际启用后报告，恢复和资源退出需要真实证据。

## 失败证据与修正

初轮 [run-c3bb51b6](../src-tauri/target/native-audio-session-validation/batch-bf930796-dde7-4ad3-9a1d-c9ca23a8d913/run-c3bb51b6-c511-4a33-ba62-497d7f740f77/native-audio-session-validation.json) 在第二个 7.123 秒源目标的播放阶段观察到设备队列为空，验收失败并停止，不能报告整条设备流程通过。第一次定位已清空旧设备缓冲并正确换代；失败后 Stop/Reset、MMCSS 恢复和 decoder 回收均有记录。原始失败报告与已完成第一段捕获保留，第二段未保存完整捕获的限制也不隐去。

提交记录显示，适配器要求准备完整 4800 帧包，解码队列最多只有 8×480=3840 帧，而设备缓冲为 1056 帧；每跨过 4800 帧边界可能出现连续零提交，等待包组装完毕才继续供音。先将新传输包准备长度限定为设备容量与请求长度的较小者，同时保留按已观察可用容量裁切本次写入的规则。新报告也先保存失败时的供给统计，再判定失败。

首次采用上述限制的 [run-40f3f176](../src-tauri/target/native-audio-session-validation/batch-892ff4ca-18a4-4b84-90af-1c9f31c70622/run-40f3f176-4052-4959-ad6c-8690ba6a9515/native-audio-session-validation.json) 仍在第二段观察到 1 次队列为空，不能判定问题已解决。该段最大供给循环间隔约 10.47ms，虽然小于 22ms 设备缓冲，仍不能证明每次循环获得足够 PCM。失败证据用于区分线程及时被唤醒与实际数据及时供应。

最终再增加 `Ready` 门槛：设备缓冲满且下一包已准备好，才允许 Start。没有扩大 1056 帧设备缓冲、8 块解码队列或待提交槽，也没有放宽下溢标准。两个失败批次与对应构建摘要保留；最终验证使用新的固定构建。

## 最终真实设备结果

最终批次 `batch-f7cd8d2d` 的 [第一次运行](../src-tauri/target/native-audio-session-validation/batch-f7cd8d2d-b229-4892-8210-d1ec4e27d98a/run-3333aaad-42e9-4ec1-a812-da45e8abcec5/native-audio-session-validation.json) 与 [第二次运行](../src-tauri/target/native-audio-session-validation/batch-f7cd8d2d-b229-4892-8210-d1ec4e27d98a/run-ab55ff48-49a1-4c2f-9bc8-57022c12fb50/native-audio-session-validation.json) 均完成全部 4 段、3 次播放中定位及最终带队列取消。

| 源时间目标 | 每轮设备接受帧数 | 内容范围 |
| --- | ---: | --- |
| 2.2 秒 | 50,016 | 非周期源 PCM，包含暂停恢复 |
| 7.123 秒 | 50,016 | 新锚点对应的连续源 PCM |
| 0 秒 | 16,416 | 音频开始前的前置静音 |
| 12 秒 | 16,416 | 224 帧源 PCM，随后 16,192 帧尾部静音 |

每轮合计接受 132,864 音频帧。准备阶段设备时钟保持 0，padding 为 1056，下一完整 1056 帧包已就绪但游标未提前消费。首次播放在源游标 31,872、待提交源 PCM 剩余 960 帧时暂停 200ms；设备位置、padding 和待提交包冻结，恢复不 Reset。

每次定位前都观测到设备中仍有排队数据，定位后 position/padding 均为 0，状态回到停止的 `Preparing`，新代次拒绝旧 ticket，再由显式 Start 播放。最终取消后 position/padding 也保持 0，等待 150ms 无变化；旧 ticket、新准备/播放命令被拒绝，重复取消安全。Reset 前观测的 padding 可能在 Stop 前继续消耗，不能据此声称精确丢弃了 1056 帧。

两轮最大供音循环间隔分别为 10.6868ms 与 10.6455ms，均低于实际 22ms 缓冲，且所有段未观察到队列下溢。全部 MMCSS 注册均显式恢复，自有 decoder 回收，报告记录的 24 条 decoder PID 引用均完成退出。有限两轮结果不覆盖长期运行或其他声卡。

## 核验与限制

[独立验证汇总](../src-tauri/target/native-audio-session-validation/batch-f7cd8d2d-b229-4892-8210-d1ec4e27d98a/verification-summary.json) 从原始起点和完整参考 WAV 重建每段期望 PCM，逐字节核对全部 8 份实际提交捕获，同时复核预填、正数部分提交换 ticket、暂停、代次、Reset 和取消终态，结果通过。[固定程序与源码身份](../src-tauri/target/native-audio-session-validation/batch-f7cd8d2d-b229-4892-8210-d1ec4e27d98a/build-context.json) 绑定最终构建。

[工程检查记录](../src-tauri/target/native-audio-session-validation/batch-f7cd8d2d-b229-4892-8210-d1ec4e27d98a/verification-checks.json)：前端 lint、231 项测试和构建通过；默认 Rust 100 项通过、6 项既有环境测试忽略；session 探针 83 项通过、2 项忽略，其中适配器模拟测试 13 项；未变更的 transport 探针本轮检查为 70 项通过、2 项忽略。共享测试不能相加算独立覆盖，旧 ignored 项未另跑。release 构建、格式及 diff 检查通过。模拟故障覆盖错误清理路径，没有在真实声卡上注入系统故障。

捕获只证明端点接受的输入与映射一致，含随后被定位/取消清掉的缓冲数据；不是物理播放完整性证据。本轮不声明 GPU 显示、物理音画同步、跨设备稳定性或主应用播放器替换完成。
