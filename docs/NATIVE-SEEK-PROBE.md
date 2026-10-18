# Windows 原生受控随机定位探针

本轮在独立1080p探针中增加显式`indexed-cfr`定位，保留默认`sequential`对照。同一release程序的30秒定位完整准备耗时从4744.62ms降到166.04ms，完整60秒回归后的定位为145.50ms；三组功能与本次供给预算检查通过，总体实时资格仍未关闭。主应用、UI、项目格式和MCP不变；没有可见窗口、Present或录音，音频仍为自有静音会话。

## 范围与精度

- 快速路径只接受零起点、30/1帧率且帧时间可精确表示的视频，以及48kHz双声道PCM16 WAV；不支持的输入明确报错，不静默改回顺序解码。
- 根据源PTS向下选取目标所属帧。帧901的纳秒表示为30033333333，不能再简单乘30取整成900；使用检查溢出的整数运算并核对实际时间戳。
- FFmpeg输入`-ss`默认执行精确定位，ffprobe的`-read_intervals`可能先返回更早的关键帧。只过滤多余PTS记录，然后与RGB逐帧配对；检查首帧、连续性和结束计数。
- 音频先定位到目标之前的整秒，再精确丢弃剩余采样。源文件位置与本次实际解码读取数分别报告。
- 每次定位仍重开解码进程，尚非常驻解码器。帧编号、纳秒边界及非块对齐采样均由单元测试和真实解码参考核验。

## 对照方式

同一release程序和60秒真实1080p素材运行：`--seek-mode sequential --seek-only true`，再`--seek-mode indexed-cfr --seek-only true`，最后`--seek-mode indexed-cfr --seek-only false`完整回归。其余参数为`--hd-fixture-dir`、`--renderer-lifetime shared`、`--audio-scheduling event-mmcss`。

seek-only只播放前约2秒，再定位30秒播放约1秒后取消，不能报告为完整60秒。完整回归须实际提交2,880,000音频帧、到达60秒设备时间并验证末帧1799。定位首个准备画面和首个GPU验证结果必须都是帧900。

比较`totalSeekToPreparedMs`（停止旧会话到准备完成）、`decoderStartMs`和`preparation.elapsedMs`，不将之前约5.16秒的单独准备阶段与新路径总耗时混算。功能、音频供给预算与总体实时资格分开记录，实际消费帧不等于屏幕已显示帧。

## 初步证据及失败保留

固定sidecar命令验证见 [seek-evidence.json](../src-tauri/target/native-seek-validation/evidence-bda901ed-8cf2-4a3d-82b0-c765431f969c/seek-evidence.json)：0、1.5、30、30.033333333、30.020、59秒的连续三帧与参考一致；非对齐PCM源区间也一致。这只是独立解码命令证据，不代表整条管线已通过。

首轮同程序整机对照中，顺序路径通过，而indexed路径在启动后报告RGB/PTS数量不一致并停止。失败记录为 [run-3fbfc201](../src-tauri/target/native-seek-validation/batch-5718dc56-5ceb-49f6-bf63-eb04607bc5bb/run-3fbfc201-0ca4-4894-8aa9-2532c2a3eec9/native-stream-validation.json)。旧程序与源码摘要保存在同目录的`native_stream_probe-before-fix.exe`、`before-fix-context.json`。失败时音频停止、MMCSS恢复、decoder回收及最终GPU退出均有报告；不会删除失败后只展示成功运行。

修复根因后已用新的固定程序重新跑三组对照，原失败记录与程序副本保留。本轮不改变FFmpeg来源或授权，不下载或重编译sidecar。

### ffprobe开放区间的失败原因

独立于Rust、声卡和GPU的命令重复实验也出现`59%`返回0或2行、exit0且stderr为空。固定单线程没有解决（40次对照中默认8次、单线程13次短输出）；显式终点`59%60`在后续20次中均返回完整30帧，显式`-show_frames`仍可能短输出，说明线程数/是否显示帧并非修复。

核对本机对应的FFmpeg n7.1源码`E:\ffmpeg-build\ffmpeg-n7.1\fftools\ffprobe.c`：`parse_read_intervals`使用未清零的`av_malloc_array`，空终点分支只设置`has_end=0`；随后读取循环仍检查未在此分支赋值的`end_is_offset`等字段。这与偶发提前终止的证据一致。集成修复为传入经过元数据校验的**显式绝对终点**，不会修改FFmpeg二进制、伪造PTS或静默重试；过滤后实际PTS和完整结束计数仍必须严格匹配。绝对终点向上舍入到微秒并留1微秒边界，避免最后一帧被排除。

## 来源

[FFmpeg -ss与accurate_seek](https://ffmpeg.org/ffmpeg.html)以及[ffprobe read_intervals](https://ffmpeg.org/ffprobe.html)官方说明已在命令实验中读取，相关摘录保存在同一实验目录`official-seek-sources.json`。代码规则仍需实际PTS与内容校验，不能以文档代替设备证据。

## 修复后的真实对照

固定程序的完整批次为`src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/`：

- [顺序定位](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/run-0023b3da-3468-40c3-9243-ad1f60e280ef/native-stream-validation.json)。
- [索引定位](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/run-dc8f9fad-f1e3-4a73-b464-d7e78296fa75/native-stream-validation.json)。
- [索引完整60秒回归](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/run-fcbef67d-ec98-4e54-bbdf-a93786ffbabc/native-stream-validation.json)。
- [独立验证](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/verification-summary.json)、[程序/源码身份](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/build-context.json)。

| 30秒定位实测 | 顺序短程 | 索引短程 | 索引完整回归后 |
| --- | ---: | ---: | ---: |
| 停止旧会话至准备完成 | 4744.6229ms | 166.0418ms | 145.4977ms |
| 新decoder启动 | 52.2172ms | 105.5292ms | 104.2795ms |
| 最后预填/首帧准备 | 4663.6867ms | 43.8617ms | 40.1421ms |
| 准备时实际读取RGB帧 | 902 | 1 | 1 |
| 准备时实际读取PCM帧 | 1,445,760 | 5,760 | 5,760 |
| 首个准备/首个GPU验证源帧 | 900 / 900 | 900 / 900 | 900 / 900 |

短程同条件比较约28.6倍，是本机单次观测，不是跨设备保证或P95。新路径的源视频偏移900、音频偏移1,440,000与实际读取计数分别记录。准备只读到一帧意味着真正跳过源前缀，不是重新标记从头解码的耗时；底层关键帧预滚解码与ffprobe处理仍有内部成本，应用读取计数不等于codec内部全部工作量。

完整回归提交2,880,000音频帧，音频序列hash与WAV参考一致；源视频解码1800帧，末帧1799已完成GPU内容验证，设备时钟达到60秒、padding为0、源子进程正常EOF。播放包含一次200ms暂停。随后30秒定位到约31秒的所有实际消费画面与RGB/RGBA参考一致，音频区间匹配。三个流程的MMCSS每阶段恢复、取消、旧代次隔离、decoder回收、单图形实例最终释放均通过。

完整回归最大供音间隔10.5929ms，小于实际22ms缓冲；无事件超时或观察下溢。完整源初段与恢复段实际消费1772条，另有29条seek结果，总1801条；本次27项pending淘汰，最长约70.1ms没有新的消费完成结果。读回/逐像素校验仍是诊断负载，不能将末帧与内容正确称为屏幕稳定30fps。历史音频间隔和GPU析构波动也未由此关闭。

## 检查与保留证据

前端231项、lint和构建通过；默认Rust100项通过、6项既有环境测试忽略；探针90项通过、2项设备依赖测试默认忽略，其中新增真实索引解码测试已在release中显式执行通过。该测试覆盖6个目标、帧901边界、非对齐PCM，以及从59秒完整读到尾部（30帧/47,999个目标后音频帧）。release构建、格式和diff检查通过。测试、命令实验和真实设备结果不相互替代。

最终[检查记录](../src-tauri/target/native-seek-validation/batch-73afa82e-e3b9-42be-9a4a-d32f2f7753cc/verification-checks.json)保留命令结果和进程核对。三个实际流程记录的18个decoder PID及探针均无残留。原始报告、验证汇总及程序/源码哈希位于本地忽略的target目录；本工作区含既有未提交改动，不能只用Git基准提交标识运行版本。

在最终批次之前有一次误将旧程序运行与新链接重叠，导致Windows LNK1104文件占用。该次基准报告保留在`batch-4cd686a7-afc5-46de-8264-8245a7c93b5d`并有`comparison-exclusion.txt`说明，未用于提速比较。退出后重新构建成功，再从空批次重跑全部三组；最终三组没有并行编译或替换程序。

## 下一步

新路径目前只通过受控零起点30fps视频和原始PCM验证，不等于VFR、重排帧、非零音轨起点或AAC/MP3延迟的通用精确定位。下一步先补这些时间模型和素材矩阵，或收敛正式预览适配边界；逐帧GPU读回开销、屏幕呈现和主应用集成仍需单独处理。默认顺序路径继续保留。
