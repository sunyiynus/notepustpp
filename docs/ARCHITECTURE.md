# 模块边界与接口契约

本规格约束不同开发者和 AI 之间的集成方式。文中的类型与方法名是本项目需要实现的接口设计，不是声称 GPUI-Kit 已提供的 API。先完成契约和适配层验证，再并行开发功能。

## 工作区结构

首版采用以下 Cargo workspace。app 内的 workspace、documents、editor、settings 是模块，暂不为每个界面拆 crate。GUI 和工作进程必须分成两个 binary，后者不能链接 GPUI。

~~~text
crates/
  app/                 # binary；GPUI窗口、文档模型、编辑器适配、界面
  contracts/           # 纯数据与协议；无GPUI/数据库/文件系统实现依赖
  text-codec/          # 编解码、编码检测、换行扫描、偏移映射
  file-io/             # 打开、保存、备份、文件身份和监视编排
  storage/             # 设置、会话、恢复数据库
  search/              # 搜索调度、结果、替换计划、worker supervisor
  large-file/          # reader、索引、缓存、tail；不依赖GPUI
  platform/            # Windows原生接口与未来平台适配
  search-worker/       # binary；标准/高级正则执行，stdin/stdout协议
  xtask/               # fixtures、基准启动、打包编排
assets/
  locales/
  themes/
  icons/
  grammars/
tests/
  fixtures/
  scenarios/
packaging/windows/
docs/
~~~

依赖方向如下：

- contracts 只依赖 std、serde、uuid、thiserror 等通用类型。
- text-codec 依赖 contracts；platform 依赖 contracts 及目标平台库。
- file-io 依赖 contracts、text-codec、platform。
- storage 依赖 contracts、text-codec；文件备份通过 file-io 服务编排，不反向依赖 app。
- search 依赖 contracts、text-codec、file-io 的公开读取接口、platform 的进程控制。
- large-file 依赖 contracts、text-codec、platform；共享 file-io 的文件身份类型与监视事件，避免循环。
- app 组合上述模块，只有 app/editor 持有 GPUI-Kit EditorState。
- search-worker 依赖 contracts、regex、pcre2 和必要的解码模块；通过冻结后的 UTF-8 快照进行普通文档匹配。

必要时把共同的 FileIdentity、FileStamp、ReadAt 接口放在 contracts；不得通过把搜索或日志读取器塞进 app 来解决依赖循环。

## 所有权与线程

| 对象 | 唯一所有者 | 其他模块如何使用 |
| --- | --- | --- |
| 文档文本与撤销历史 | app/editor 的 Kit 适配状态 | 不可变快照、增量编辑事件、版本化 EditBatch |
| 文档路径、编码、BOM、磁盘状态 | app/documents | 文档 ID 和公开查询/命令；后台只得到快照值 |
| 光标、选区、滚动、折叠、焦点 | 对应 ViewId 的视图/适配器 | 通过视图命令；不混入文档保存数据 |
| 原始文件字节快照 | file-io 管理的不可变资源 | 用于原样另存/字节保真判断；不是第二份可编辑文本 |
| 日志文件句柄、解码检查点、索引 | large-file 的 reader actor | 版本化窗口请求与结果 |
| SQLite 连接 | storage 专属写入线程 | 有界消息队列；UI线程不得执行SQL |
| 搜索匹配与结果 | search 的作业对象 | 有界结果批次和可取消句柄 |
| Windows句柄/进程Job | platform的RAII对象 | 稳定服务接口；app不散落unsafe调用 |

GPUI Entity、Window、Context、可变 EditorState 不得跨线程传递。UI 接收后台结果后，按 JobId、DocumentId、DocRevision 或 LogGeneration 校验，再在 GPUI 上下文中提交。

普通编辑中的 CRLF、LF、CR 均保留在权威 UTF-8 文本中。统一 EolScanner 先识别 CRLF，再识别孤立 CR/LF；行列 API、索引和显示层不能各自采用不同规则。Kit release 中 LF 假设的修补属于 M0 的编辑适配工作。EOL 索引可重建，不承担另一套权威文本状态。

## 公共数据契约

| 类型 | 必需字段或规则 |
| --- | --- |
| DocumentId | uuid v4；同一打开文档生命周期及恢复记录内稳定 |
| ViewId | uuid v4；视图身份；不能用标签位置代替 |
| JobId / TransactionId | uuid v4；事件去重和任务跟踪 |
| DocRevision | u64，文本或会改变保存结果的编码/BOM设置变更时递增；撤销也递增 |
| Utf8Offset / Utf8Range | u64，原样编辑文本内 UTF-8 字节位置；区间为左闭右开 |
| RawByteOffset | u64，原文件编码字节位置；不得与 Utf8Offset 隐式互转 |
| Utf16Position | UTF-16 code unit 位置，仅用于相应平台/协议接口 |
| LogicalPosition | 从0开始的逻辑行与grapheme列；对外状态栏显示时转成从1开始。平台/LSP的UTF16列单独转换，不能借用此类型 |
| DisplayColumn | 按显示策略计算的列；不能等同字符数、UTF-16长度或字节数 |
| EncodingSpec | 编码ID、映射profile、BOM策略、write_policy；不能仅存“ANSI”。write_policy区分PreserveSourceIfUnchanged与Reencode，显式转码即使字符/编码名相同也可改变保存结果 |
| FileIdentity | Windows volume标识+128位file ID；未来Unix设备号+inode；允许未知状态 |
| FileStamp | identity、长度、修改时间及按需计算的内容摘要；mtime+size不能作为强一致证明 |
| NativePath | WinUtf16(Vec<u16>) 或 UnixBytes(Vec<u8>)；协议禁止有损路径字符串转换 |
| SaveOutcome | savedRevision、savedFormat、提交后FileStamp、备份位置及已知告警 |
| OperationError | Cancelled、Conflict、EncodingLoss、Unsupported、ResourceLimit、Io、InvalidRequest等可匹配变体 |

DocumentId 与 FileIdentity 分开：未命名文档有 DocumentId；原子替换后磁盘 file ID 可能变化，仍然是原文档。同一文件的重复打开通过句柄查询身份处理，路径规范化只能作为辅助。

BlobStore和SaveJournal是contracts定义的服务接口，由storage实现、由app注入file-io，分别提供不可变原始字节块与可恢复保存操作记录。file-io不反向依赖storage crate。每次成功保存后，baseline的原始字节generation必须切换为本次输出，不能在下一次另存时重新吐出更早的原始字节。

## 快照与编辑接口

设计 SnapshotHandle 为可克隆、只读、可发往后台的资源句柄，包含 DocumentId、DocRevision、EncodingSpec、utf8Length、SnapshotId。数据读取支持分块/reader，不要求转成一个 String。

默认实现对 Kit 使用的同版本 Rope 做廉价不可变克隆，具体 Send/Sync 和快照成本在 M0 核实。若不能安全跨线程，适配器生成一次冻结的 UTF-8 临时快照并复用；不得改成每次按键镜像全文。进入正则 worker 时由后台把快照写为仅当前用户可读的临时文件。

EditBatch 的设计字段：

~~~text
document_id
expected_revision
transaction_id
command_id
edits: [{ range_utf8, inserted_utf8 }]
format_change: optional EncodingSpec change
selection_after: optional view selection state
undo_label
~~~

所有 edits 针对同一个 expected_revision，互不重叠，端点位于合法 UTF-8 边界；应用前完整验证，按偏移从后向前提交。范围不能截断 CRLF 的一半，除非是显式字符级操作并有验收定义；普通删除/移动按完整换行单元处理。

适配器需要公开的本项目接缝：

| 接缝 | 行为 |
| --- | --- |
| snapshot(document) | 返回不可变文本与保存格式快照 |
| apply_batch(batch) | 校验版本，完整提交一个撤销事务，返回新revision；版本冲突时不部分应用 |
| undo(view) / redo(view) | 调用同一权威历史并更新文档事件 |
| observe_committed_edits(document) | 产生有序、去重、带事务身份的已提交变化 |
| set_view_selection(view, ranges) | 应用选区；不修改文本revision |
| publish_search_decorations(job, revision, ranges) | 仅对仍有效的文档版本更新可见匹配 |
| apply_replacement_artifact(artifact) | 校验来源revision、UTF8长度、摘要和完整状态后，分块读取准备好的替换结果，以一个可撤销事务提交 |
| query_capabilities(document, view) | 返回普通编辑/日志模式及可用命令 |

Kit 的私有批量编辑/撤销 API 不应被其他模块直接调用。若公开 API 不足，由 app/editor 的最小上游补丁提供接缝并写兼容测试。初始化/重新载入可以用重置 API；用户编辑、批量替换与另一视图同步不能用 set_value。

原生键盘、粘贴、IME 和项目程序化编辑最终都通过一次已提交变更通知更新 revision、恢复日志和修改状态。适配器不得同时在“命令提交”和“Kit事件”中重复记账同一次变化。

EditBatch面向有界的编辑集合；百万匹配的ReplaceAll不得强制转换成百万项Vec再发往UI。ReplacementArtifact包含sourceSnapshotId、来源DocRevision、输出NativePath、UTF8长度、BLAKE3摘要、完整命中数与完成状态。适配器读取artifact，在版本未变化时以一个事务替换；具体Rope导入和undo成本作为100 MB全文替换验收，不能对每个命中执行set_value。

## 修改状态与保存流程

递增 revision 用于拒绝过期结果，不能用 revision==savedRevision 判断撤销后是否恢复到已保存内容。

保存状态比较对象是“文本内容+会影响写入结果的编码与BOM”。优先在适配层使用可靠的历史保存点；没有公开保存点时，用事务状态标记加后台 BLAKE3 指纹确认恢复到 clean，确认前保持保守的 dirty/pending。不得每次输入重新计算全文哈希。

应用向 file-io 提交 SaveRequest：快照、目标路径、预期 FileStamp、保存模式和备份策略。IO 结果只标记请求中那一个版本已经保存。保存途中输入的新内容仍然 dirty；保存错误保留缓冲。

“转换换行”修改真实文本并形成一次事务；“新插入行默认换行”是编辑偏好，本身不代表现有文件发生可序列化修改。

已打开文件的文件夹替换只更新缓冲；未打开文件才写磁盘。预览和结果分别标明两种行为，不做跨文件全局原子性的承诺。

## 后台作业和有界传输

默认调度方案：

- GPUI 前台 executor 处理UI编排；后台的本项目 I/O 队列由2个专属 std 线程消费，队列容量32。
- CPU计算使用本项目专属 rayon ThreadPool，线程数 clamp(逻辑CPU数-2, 1, 4)，避免使用不可控的全局池。
- SQLite 使用1个专属写线程；正则进程由 search supervisor 管理。
- 跨线程通道使用 async-channel::bounded；同步worker用阻塞端，UI通过异步等待或try_send提交，UI线程不得send_blocking。
- 取消使用 Arc<AtomicBool> 和作业generation。字面量扫描/索引/解码在块边界检查，关闭文档后拒绝晚到结果。
- 每文档的可改变磁盘状态的保存操作串行。搜索/索引可以并行，但结果必须携带版本。

消息上限同时按数量与字节控制。搜索结果传输最多8个批次，每帧不超过1 MiB；UI结果缓存上限32 MiB。进度可以合并，错误与最终状态必须可靠送达。停止消费、取消、关闭通道都必须解除被反压阻塞的生产者。

[async-channel 有界通道](https://docs.rs/async-channel/latest/async_channel/)、[rayon 独立线程池](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html)是此方案使用的库接缝。

## 正则工作进程协议

默认所有标准与高级正则请求走 search-worker，保持硬取消和统一资源边界；字面量扫描在普通后台作业中执行。两个持久worker槽分别服务当前文档与工作区任务，每个worker同时只运行一个job，任务之间复用进程。

协议采用 stdin/stdout 的 u32 little-endian 长度前缀+UTF-8 serde_json。每帧最大1 MiB，超限或版本不符立即失败并回收worker。stderr 专供诊断，stdout 禁止打印日志。控制帧传路径与长度，不能把100 MB全文塞入JSON。

| 消息 | 必需内容 |
| --- | --- |
| Hello / Ready | protocol_version=1、build_id、支持的engine与flags |
| SearchRequest | job_id、purpose=Find或ReplacePlan、engine、pattern、options、snapshot_id、NativePath、utf8_length、timeout_ms、结果限制；ReplacePlan附统一replacement AST和父进程指定的output_artifact_path |
| MatchBatch | job_id、snapshot_id、匹配UTF-8范围及有限捕获组范围；文档版本由父进程关联 |
| Progress | 扫描进度；不把未知总行数当成精确值 |
| Finished | 结束原因、是否截断、命中计数是否完整、统计数据 |
| ReplacementReady | sourceSnapshotId、输出artifact路径/长度/摘要、完整命中数、完成状态；worker不得写真实目标文件 |
| Failed | 可匹配错误类别和模式位置；不返回未经约束的全文 |

取消或超时：父进程标记job结束，关闭结果通道，终止对应worker，等待回收并排空旧pipe，再补建worker。Windows使用Job Object限制进程资源并确保子进程清理。不能用drop future或UI进度消失作为正则已经停止的证明。

worker的输入只从父进程生成的快照路径读取，按记录长度与摘要/快照身份校验，处理完成释放临时资源。临时快照允许文件级复用，必须有引用计数和启动时残留清理策略。

ReplacePlan允许写入父进程为该job指定的私有输出artifact。Find达到显示上限可停止并标记截断；ReplacePlan的预览达到上限后仍完整扫描，流式构建替换输出，完成后才返回ReplacementReady。取消/错误的未完成artifact不能提交。普通文档搜索快照预算384 MiB，以覆盖100 MB原文件的最坏常见编码膨胀；worker资源上限与峰值一并实测。

## 日志请求契约

LogHandle 表示打开的原始编码文件。LogGeneration 在文件被替换、截断或编码切换时递增；单纯追加只增长 observedLength。

ViewportRequest 指定 LogHandle、generation、目标逻辑行或原始字节锚点、可见行数和横向范围。返回值必须包含原始字节范围、解码文本片段、实际行范围、索引是否完整及generation。未扫描区域不能伪造精确总行数。

冻结搜索使用请求开始时的 observedLength；tail 新增内容不能悄悄改变一次搜索的输入范围。长时间“跟随并持续搜索”是独立作业。

## 并行开发规则

公共类型、保存协议、任务事件和worker协议先由集成人员冻结。各AI仅修改自己负责的模块和测试；公共接口变更必须同时更新本文件、调用方和契约测试。

集成验证顺序为 contracts → text-codec/platform → file-io/storage → editor/search/large-file → app工作区。业务层错误不能退化为“字符串匹配错误信息”，也不能用全局 Mutex 包住所有状态规避生命周期问题。
