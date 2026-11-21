# 文件、编码、保存与恢复实施规格

状态：设计决策与实施契约；2026-11-21。仅完成官方文档与 API 核实，尚未建立 Rust 工程、解析依赖或编译验证。代码片段是接口草案，不是可以直接复制运行的实现。

本规格覆盖普通文件的完整编辑路径；1 GB 日志使用 `large_file` 按需读取，不把全文或日志追加流写入编辑恢复库。各模块必须使用同一套文档身份、序列化元数据、版本与冲突类型。

公共类型和 crate 依赖以 [ARCHITECTURE.md](ARCHITECTURE.md) 为准；构建、故障注入与发布验收接入 [BUILD_RELEASE_TESTS.md](BUILD_RELEASE_TESTS.md)。本文件的 IO-xx 是模块内分解，不替代总交接清单的任务编号。

## 1. 默认依赖与责任边界

以下版本为调研时官方页面显示的版本，实施时在工作区统一选择兼容版本、锁定 `Cargo.lock`，通过 MSVC 构建后记录实际版本。不能仅根据本表声称已经编译通过。

| 功能 | 单一默认库或方案 | 具体使用方法与官方依据 |
| --- | --- | --- |
| UTF-8、中文及其他受支持编码解码 | `encoding_rs`，调研文档 0.8.44 | 使用 `Encoding` / `Decoder` 的 `*_without_replacement` API；不使用默认替换错误字符的便利函数实现可靠打开。[crate](https://docs.rs/encoding_rs/latest/encoding_rs/)、[Decoder](https://docs.rs/encoding_rs/latest/encoding_rs/struct.Decoder.html) |
| GBK / GB18030 写入 | `encoding_rs::Encoder` | `encode_from_utf8_without_replacement`；`Unmappable` 中止保存，报告字符及文本位置。不能输出 HTML 数字实体代替原字符。[Encoder](https://docs.rs/encoding_rs/latest/encoding_rs/struct.Encoder.html) |
| UTF-16 LE/BE 写入 | Rust 标准库 | `str::encode_utf16()` 逐个单元生成 `to_le_bytes()` / `to_be_bytes()`；BOM 单独处理。`encoding_rs` **不提供 UTF-16 编码器**，请求其 UTF-16 encoder 会得到 UTF-8 输出。[官方限制](https://docs.rs/encoding_rs/latest/encoding_rs/#utf-16le-utf-16be-and-unicode-encoding-schemes)、[encode_utf16](https://doc.rust-lang.org/std/primitive.str.html#method.encode_utf16) |
| 无 BOM 的编码候选 | `chardetng`，调研文档 1.0.0 | 使用增量 `feed` + `guess`，不将猜测当作保证。1.0 API 的 `new` / `guess` 使用检测枚举，不能照搬旧版 bool 示例。[EncodingDetector](https://docs.rs/chardetng/latest/chardetng/struct.EncodingDetector.html) |
| 同目录临时文件 | `tempfile`，调研文档 3.27.0 | `NamedTempFile::new_in(target_parent)`，检查写入、flush、`File::sync_all` 错误；不以系统临时目录文件跨卷替换目标。[crate](https://docs.rs/tempfile/latest/tempfile/)、[NamedTempFile](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html) |
| Windows 提交与文件身份 | `windows`，官方文档 0.62.2 | Windows 适配层封装 `CreateFileW`、`GetFileInformationByHandleEx`、`ReplaceFileW`、`MoveFileExW`。候选 features 为 `Win32_Foundation`、`Win32_Storage_FileSystem`、`Win32_Security`，以编译结果补齐，不向其他模块暴露裸 HANDLE。[官方 Rust bindings](https://microsoft.github.io/windows-docs-rs/doc/windows/) |
| 外部变化通知 | `notify`，调研文档 9.0.0 | `recommended_watcher` / `RecommendedWatcher`，按父目录监听与文件身份复查；通知仅是提示，不是内容真相。[官方文档与限制](https://docs.rs/notify/latest/notify/) |
| 配置和恢复路径 | `directories`，调研文档 6.0.0 | `ProjectDirs` 生成 `config_dir` / `data_local_dir` / `cache_dir`；恢复库使用本机本地数据目录。[ProjectDirs](https://docs.rs/directories/latest/directories/struct.ProjectDirs.html) |
| 配置及协议序列化 | `serde` + `serde_json` + `toml` | `serde` derive；用户配置 `settings.toml`；布局、历史及恢复操作参数用有版本的 JSON；全文、插入文本、原始字节使用 BLOB，不经 JSON 数组放大。[serde](https://docs.rs/serde/latest/serde/)、[serde_json](https://docs.rs/serde_json/latest/serde_json/)、[toml](https://docs.rs/toml/latest/toml/) |
| 恢复、会话及保存操作日志 | `rusqlite`，调研文档 0.43.0，features `bundled`, `backup` | 单写入工作线程、SQLite WAL、`synchronous=FULL`；checkpoint + edit delta。`bundled` 固定随程序构建的 SQLite，`backup` 用于一致的数据库迁移备份。[crate](https://docs.rs/rusqlite/latest/rusqlite/)、[构建 features](https://github.com/rusqlite/rusqlite#notes-on-building-rusqlite-and-libsqlite3-sys) |
| 内容戳与恢复完整性 | `blake3`，调研文档 1.8.7 | 后台按块 `Hasher::update`，计算源字节、保存输出、恢复块及文本快照摘要；禁止逐键全文 hash。[官方 crate](https://docs.rs/blake3/latest/blake3/) |

不新增另一套文本 buffer、文本 undo、自动保存插件或第二种默认恢复存储。`file_io` 不直接持有 UI 实体；通过 `editor` 提供的不可变快照读取唯一权威文本。

## 2. 文本表示、换行和编码约束

### 2.1 唯一权威文本保留实际换行

GPUI-Kit Editor 的 Rope / buffer 保存解码后的 UTF-8 文本，**原样保留 CRLF、LF、CR**；不开启全局 LF 归一化，不为每条编辑建立另一份 EOL 权威 sidecar。BOM 是 `EncodingSpec` 元数据，不作为编辑区首字符。

派生的 `EolIndex` 只记录分隔符位置、计数、首个类型和首选 Enter 类型，标注所依据的 `DocRevision`；随编辑失效或增量更新，可从文本重建，不能独立改变保存结果。保存时直接编码快照中的真实 `\r` / `\n`。正则与编辑偏移都针对同一份原始换行 UTF-8 文本。

M0 必须验证 CR-only 文件的行号、行列定位、上下移动、选择、删除、软换行、解析位置；不能未经验证依赖 Kit 的 `.lines()` 或默认 LF 行索引。CRLF 在光标、退格、Delete 和换行转换中视为一个逻辑换行；一项命令不能留下意外的半个 CRLF。必要时在 Kit 输入/显示/行索引适配层做最小补丁，并保留源码补丁及回归测试。

Enter 默认使用文件中数量最多的换行；并列取最早出现的类型；无换行的新文档在 Windows 默认 CRLF。粘贴保留剪贴板传来的实际换行；若产品要提供“统一粘贴换行”设置，必须作为显式命令策略。换行转换替换真实文本，并进入同一文本撤销事务，不是修改一个会悄悄重写全文的保存选项。

### 2.2 不把“可解码”当作“字节可往返”

`encoding_rs` 面向 WHATWG Encoding Standard；GB18030 是接近 2022 的 Web 兼容映射，包含特定双字节、四字节映射差异，不能表示 19 个私用区字符。GBK 的 decoder 与 gb18030 相同，encoder 才限制为 GBK 兼容输出，因此“以 GBK 解码成功”不证明原字节是严格 GBK。[GB18030 官方边界](https://docs.rs/encoding_rs/latest/encoding_rs/static.GB18030.html)、[GBK 官方边界](https://docs.rs/encoding_rs/latest/encoding_rs/static.GBK.html)

首版界面写“GBK”“GB18030”，编码详情记录实现和映射版本，发布说明记录上述例外。若需求变为特定国家标准版本的严格一致性，必须增加正式映射测试与实现决策，不能自行将当前库称为合规实现。

普通文件打开时保留一份不可变 `RawSourceSnapshot`，按块存入恢复库 `source_chunks`，关联原始摘要和来源版本；不因 UI 清理一个临时 `Vec` 就失去原字节。对同一来源 generation 去重引用，关闭后按保留策略回收。原路径可能被外部改写，因此不能把稍后重新读取原路径当作原始字节快照。

`recovery.enabled=false` 时仍需原字节快照支持编码重解释与原样另存，但改用 `tempfile::tempfile_in(local_cache)` 的临时句柄，不写入 durable source chunks、checkpoint 或 delta；关闭文档释放临时资源。该设置控制崩溃恢复正文，不关闭保存操作元数据 journal。已有恢复数据是否清除由单独明确动作决定，不把关闭功能当作静默删除命令。

初次后台执行 `decode -> exact encode` 字节比较，记录 `RoundTripClass::Exact` / `NonCanonical` / `Malformed`。无修改的保存或另存副本必须能使用原始 source chunks 输出逐字节相同的结果，包括非典型但可解码的 GB18030 序列；不能仅用“字符相同”代替字节验收。

`NonCanonical` 文档可以浏览和选择；首次修改前说明后续保存会重新编码整份文件，要求用户明确选择“转换为当前编码后编辑”或“另存为 UTF-8 后编辑”。不做字节拼接式局部转码来假装所有历史编码都有无损增量编辑能力。确认当前编码转换时将 `EncodingSpec.write_policy` 改为 `Reencode` 并标记 dirty，即使文本与编码名称未变也不会误走 no-op；原始 source chunks 留作恢复副本。

### 2.3 打开、重解释与有损处理

编码选择顺序为：用户显式选择 > BOM > 严格 UTF-8 有效性 > chardetng 候选。显式选择与 BOM 冲突时展示冲突，不能悄悄吞掉 BOM；仅在选定编码与 BOM 一致时剥离编码标记。无 BOM 的 UTF-16 不仅凭零字节猜成可编辑正文，提供显式 UTF-16 选择。

chardetng 不提供本产品可直接使用的概率置信度；不编造“95%”。默认 `EncodingDetector::new(Iso2022JpDetection::Deny)`，完整 feed 后 `guess(None, Utf8Detection::Allow)`；不把本机域名后缀塞入文本编码判断。无 BOM 且不是严格 UTF-8 时默认先展示候选与预览，用户确认候选后进入编辑。`feed` 处理前缀时若流尚未结束，不传 `last=true`。[检测 API](https://docs.rs/chardetng/latest/chardetng/struct.EncodingDetector.html)

解码使用显式 BOM 策略的 decoder 和 `decode_to_utf8_without_replacement` 等无替换 API。`Malformed` 产生带原始字节偏移的诊断：默认只读预览、保留原字节、可重新选择编码；禁止原位保存包含 U+FFFD 替换结果。用户主动选择“按替换字符打开副本”时创建独立未命名文档，原文件不随之改变。

UTF-16 奇数字节、未配对代理项、无效 UTF-8 和截断多字节序列都属于上述错误路径。流结束使用 `last=true` 检查残留；正常分块边界上的半个字符不应被提前判为错误。

“重新按编码打开”从保留的原始 source 或经核实的当前磁盘版本重新解码；文档已修改时先处理未保存内容。“转换保存编码”保持当前字符文本、修改 `EncodingSpec`；两者不是同一个函数。

上面的候选确认是普通打开的 UI 决策，不由共享 codec 直接弹窗。文件夹任务通过 `DecodePolicy` 传入已选择的 fallback encoding；默认只对 BOM 或严格 UTF-8 确定的文件自动执行替换。允许猜测编码的只读搜索必须在结果记录候选来源；批量替换前应按候选编码分组让用户明确采用，或跳过未确认文件。不得因为一次无替换解码成功就认为猜测已获确认。

### 2.4 共用流式 codec 与日志偏移映射

`text-codec` 是普通打开、磁盘文件夹搜索及 `large-file` 的唯一编解码实现。公开 `StreamingDecoder`、`decode_chunk`、`EolScanner`、`CodecCheckpoint`、`BoundaryMap`；日志模块不得自己将 `encoding_rs` 直接包成另一套语义。

首版支持可重新开始的日志编码为 UTF-8、UTF-16 LE/BE、GBK、GB18030。`CodecCheckpoint` 包含 `EncodingSpec`、`RawByteOffset`、对应已解码 `Utf8Offset`、逻辑行号、generation，以及待定 CR 状态；仅落在完整源字符之后。默认不序列化 `encoding_rs::Decoder` 私有状态，不把任意分块结尾当作可重启点。其他有状态编码若用于普通文档可从开头解码，但不能自动加入随机访问日志支持。

有界 reader 可留下未完成的 UTF-8 序列、GB18030 四字节序列、UTF-16 单元或代理对，等下一块到来；真实 EOF 才按最终解码处理。在尾随模式，当前可见文件尾的半个字符暂存为 pending，不能当作永久解码错误或写入替换字符；截断/轮转时丢弃旧 generation 的 pending 状态。

`RawByteOffset` 包含实际文件起点及 BOM 字节；解码文本 `Utf8Offset` 不包含 BOM。二者不能用固定倍数互换。`encoding_rs` 的 consumed/written 计数是块级信息，不是逐字符位置表。[Decoder 块行为](https://docs.rs/encoding_rs/latest/encoding_rs/struct.Decoder.html)

默认建立稀疏 codec-safe checkpoint；查某个结果的源偏移时，从最近 checkpoint 局部重解码，并临时构建“源字符结束位置 -> 输出 UTF-8 结束位置”的有界 `BoundaryMap`。UTF-8 按有效序列、UTF-16 按端序及完整代理对、GBK/GB18030 按该 decoder 接受的 1/2/4 字节序列识别源边界；实际字符映射仍调用无替换 decoder，不自行实现字符映射表。边界扫描器必须与所锁定 decoder 做差分与随机分块测试。

普通文档查找返回原样文本的 `Utf8Range`。日志结果返回 generation、原文件 `RawByteOffset` 和行内 UTF-8 偏移；位置映射只接受合法字符边界，对指向多字节字符中间的位置返回边界错误或显式夹取结果，不能默默当作准确定位。全文件每字符映射表不落内存，局部映射随窗口/搜索块缓存一起回收。

## 3. 数据模型与版本语义

以下类型为实现契约，具体 Rust 类型名可调整，字段语义不能由实现 AI 自行省略。

```rust
struct FileIdentity {
    volume_serial: u64,
    file_id_128: [u8; 16],
}

struct FileStamp {
    identity: Option<FileIdentity>,
    byte_len: u64,
    last_write_time: u64,
    content_blake3: Option<[u8; 32]>,
}

struct EncodingSpec {
    encoding: EncodingId,
    mapping_version: String,
    bom: BomPolicy, // None / Utf8 / Utf16Le / Utf16Be，验证合法组合
    write_policy: ByteWritePolicy, // PreserveSourceIfUnchanged / Reencode
}

struct SaveBaseline {
    text_fingerprint: [u8; 32],
    serialization: EncodingSpec,
    disk_stamp: FileStamp,
    source_snapshot: SourceSnapshotId,
}

struct SaveSnapshot {
    document_id: DocumentId,
    lifetime_generation: u64,
    revision: DocRevision,
    text: SnapshotHandle,
    serialization: EncodingSpec,
    expected_disk: Option<FileStamp>,
    target_binding: TargetBinding,
}
```

`FileIdentity` 用真实文件句柄取得的 volume serial + 128-bit file ID；不把字符串小写、`canonicalize` 或目录归一化当作同一文件的证明。两个硬链接可能有同一身份；保存替换后 file ID 可能变化，必须重新查询并更新文档注册表。[FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)、[ReplaceFileW 身份与属性说明](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)

不能取得可靠身份的文件系统使用 `IdentityCapability::Unavailable`，继续保留原生路径和内容戳，标注能力差异，不能伪造 ID。网络盘、设备、重解析点和带特殊流的路径不自动进入普通文件可靠保存路径。

路径保存 Windows 原生 `OsString` / UTF-16 单元，持久化为 UTF-16LE BLOB；可读显示值另存 JSON 字符串。不得用 `to_string_lossy()` 后的结果重新打开文件。`DocumentId` 是本应用稳定标识，不等于路径或 file ID。

`DocRevision` 对任意已提交文本或序列化元数据修改单调增加，用于丢弃过期任务。它不能判断撤销后是否回到已保存内容。

`dirty` 取决于当前文本内容与 `SaveBaseline.text_fingerprint` 是否一致、以及 `EncodingSpec` 是否一致。真实换行已在文本中；仅改编码、BOM 或明确的重新编码策略也必须标记 dirty。Enter 的首选换行属于编辑偏好，不改已有文本时不单独让文件 dirty。

优先使用 Kit 可验证的历史 savepoint / 内容节点 identity 快速判定文本一致。若当前 Kit 不支持，编辑后先保守 dirty，撤销/重做后在后台对不可变快照按块计算 BLAKE3；结果仅对相同 revision 生效。禁止每次按键复制全文或全文 hash，也不能因为递增 revision 没回退而让撤销后的文件永远 dirty。

## 4. 普通文件打开流程

1. `DocumentService::open` 分配 operation ID / lifetime generation；查询已打开身份表，决定聚焦已有文档还是新建文档。
2. Windows worker 打开普通文件句柄并取得身份、属性和初始 stamp；读取期间优先申请阻止并发写入的共享模式。遇到正在写的文件可提示重试或转日志浏览，不把不一致读取宣布为稳定编辑基线。
3. 后台顺序读块、计算原始摘要、执行编码判断、无替换解码，并准备 `source_chunks`；初始和结束身份/长度/时间不一致时丢弃结果并报告 `ChangedDuringRead`。
4. 生成 UTF-8 Rope 构建输入、原始换行派生索引、编码诊断、round-trip 分类；实际 UI 发布不得重新调用 `value()` 产生第二份全文。
5. 建立初始恢复 checkpoint，记录该 snapshot 的 `durable_revision`。大型初始持久化尚未完成时可先显示与编辑，但必须显示恢复尚未就绪，并缓冲有界的增量直到初始 checkpoint 提交；不能显示“已保护”。
6. UI 仅在 document ID + generation 仍有效时接纳结果。建立 watcher；对打开期间的待处理事件再做一次版本核实。

无修改 Ctrl+S 在磁盘 stamp 和身份仍一致时允许 no-op，不更改 mtime。另存副本要实际生成原始相同字节。外部变化不能因 no-op 被忽略。

## 5. 保存状态机与 Windows 提交

### 5.1 状态和并发规则

状态为 `Idle -> Preparing -> ReadyToCommit -> Committing -> Succeeded`，异常进入 `Conflict` / `Failed` / `NeedsReconciliation`。每文档同时最多一个提交；快速重复 Ctrl+S 合并为“完成当前快照后保存最新 revision”，不能让两个 worker 反序覆盖。

创建 `SaveSnapshot` 时冻结文本、编码、BOM、原路径绑定、预期磁盘版本。保存期间继续输入只产生较新 revision，已开始的 worker 不能读取正在变化的编辑实体。

成功只设置“磁盘已保存的那个 snapshot”为 baseline。当前文本和 metadata 已变动时保留 dirty；编码在保存过程中切换也不能被旧保存结果覆盖。文档已关闭或重开，旧 operation 只记录结果，不修改新 generation 的 UI 状态。

### 5.2 默认提交流程

1. 检查目标是受支持的普通本地文件/新文件。使用 `CreateFileW` 与 `FILE_FLAG_OPEN_REPARSE_POINT` 检查叶节点；检测路径组成中的已知重解析/链接变化、目标身份、父目录身份。发现不支持的链接或特殊文件，返回明确原因，引导另存为；不自动跟随新链接写入。
2. 对已有文件比较 `FileIdentity`、长度、时间；内容 BLAKE3 与打开/上次保存基线复查，默认不只依赖时间戳。只有磁盘仍为预期版本才继续。对硬链接计数大于 1 的文件提示“替换路径会与其他硬链接分离”，首版默认另存为，不偷偷改成原位 truncate。
3. 同目录创建随机临时文件，流式输出 BOM 与全部文本；编码错误立刻停止，原目标不动。允许无修改、元数据相同的情况直接输出原始 source chunks。写入完成调用 `flush` 和 `sync_all`，记录输出字节数与摘要。
4. 为已有文件生成同卷、唯一名称的操作回滚备份路径；将 temp 输出注册为新 source generation 后，通过存储端口提交 `save_ops.Prepared`，包括目标、temp、rollback 名称、预期身份/戳、输出摘要、new_source_snapshot_id 和 snapshot revision。恢复关闭时新 source 是临时句柄资源，journal 不强制引用 durable source BLOB。这个临时回滚物与用户的长期备份设置无关。
5. 再次验证目标与父目录绑定、reparse 状态和内容版本；变化则 `Conflict`，保留已生成快照，用户明确决定重载、另存或重新尝试。不能静默覆盖变化后的路径。
6. 已有文件默认 `ReplaceFileW(target, temp, rollback, flags=0, ...)`；新文件默认 `MoveFileExW(temp, target, MOVEFILE_WRITE_THROUGH)` 且**不带** `MOVEFILE_REPLACE_EXISTING`，避免“另存为”预检后的新文件被静默覆盖。不启用跨卷 copy/delete 或延迟到重启提交。
7. 查询真实目标的新身份、长度与摘要，记录 `Succeeded`；只有确认目标是该 snapshot 输出才推进 baseline，并将 `source_snapshot` 切到本次输出 generation。不能保留上一次打开的 raw source ID，导致随后无修改另存副本输出旧字节。按本节错误分类处理不确定结果。
8. 提交记录稳定后再清理操作临时文件；rollback 按长期备份设置升级或在启动复查后回收。失败不能自动删除唯一可恢复副本。

`tempfile::persist` 不能代替上述目标检查、备份和 Windows 语义设计；原目标禁止用 `File::create`、`truncate`、复制覆盖作为 ReplaceFile 失败时的自动兜底。

Windows 路径 API 的最后一次检查与实际提交之间，仍可能有不合作进程改变命名空间；本方案不声称消除了所有 TOCTOU 竞态。实际保证是对已观察到的身份、stamp、重解析点变化拒绝提交，并且不通过重新打开目标路径进行破坏性写入。M0 用交换文件/链接的故障用例验证可观察行为；不能仅用一次 `stat` 宣称“绝对不跟随链接”。[CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)、[符号链接文件 API 行为](https://learn.microsoft.com/en-us/windows/win32/fileio/symbolic-link-effects-on-file-systems-functions)

### 5.3 能力边界与错误分类

`ReplaceFileW` 能保留一组原文件属性与安全信息，但 `REPLACEFILE_WRITE_THROUGH` 明确不受支持。`MoveFileExW` 的 write-through 也不能证明所有设备缓存、文件系统和断电时机都不会丢失已提交字节。禁止发布“完全原子、绝对耐断电”的描述。[ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)、[MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)

| 结果 | 处理 |
| --- | --- |
| 编码失败、写满、权限拒绝、目标忙 | 不推进 baseline；展示结构化错误；保留编辑文本与可用恢复点；清理仅确定未提交且可重建的临时数据 |
| 身份、内容、父目录或重解析状态变化 | `Conflict`；不覆盖；保留 temp 供另存副本；由用户选择下一动作 |
| `ERROR_UNABLE_TO_REMOVE_REPLACED` / 1175 | 检查 target/temp/rollback 是否仍对应预期；不把“API 失败”直接等同于完全没有磁盘变化 |
| `ERROR_UNABLE_TO_MOVE_REPLACEMENT` / 1176，或 1177 | `NeedsReconciliation`；保留全部候选与日志；重新检查各路径的身份和摘要，给出原版、保存版各自位置；不删除或盲目重试 |
| API 成功后进程在 DB 更新前结束 | 启动读取 Prepared 记录；目标摘要若是输出则标记已提交，否则列出原版与保存版候选；不根据路径存在就推断成功 |
| 原文件被占用、文件系统不支持属性合并 | 说明失败原因；另存副本或显式重试；不忽略 ACL 错误以换取成功 |

对只读属性、ACL、ADS、压缩/加密属性分别做本机测试。检测不能保真的文件类型时说明范围并走另存为；不能声称所有特殊属性在所有文件系统都被保留。

## 6. 外部变化、批量替换和所有权

`notify` 监听父目录，以便捕获 rename/replace；回调只入队，不在回调线程访问 UI。使用应用层约 150 ms 合并窗口，合并事件后重新打开并查询真实身份/戳。窗口重新激活、保存前、watcher 错误或事件溢出后也复查。网络通知不可靠时使用显式刷新或 `PollWatcher`，不能依赖事件完整有序。[notify 已知问题](https://docs.rs/notify/latest/notify/)

自身保存产生的事件仅在目标的新身份与输出摘要匹配时归类为已知事件，不能用“保存后忽略两秒”吞掉外部修改。干净文档默认提示重载；dirty 文档保留当前缓冲并标记冲突，不自动重载覆盖。

文件夹替换对**已打开文件只改当前缓冲，不自动保存**，经过 editor 统一事务，可撤销并保留修改标记；该操作不得顺带把用户原有未保存修改写入磁盘。预览依据 revision，应用前复查 revision。

未打开文件由搜索模块生成替换计划，`file_io` 用相同无替换解码、版本检查、可靠保存和备份流程逐文件提交。预览依据磁盘 `FileStamp`，应用前复查。执行期间若某文件已打开，转入已打开缓冲路径并要求重新确认新预览。

跨文件不承诺全局原子事务：结果逐文件为 `AppliedToBuffer` / `SavedToDisk` / `SkippedConflict` / `Failed` / `CancelledBeforeCommit`。批量替换强制保存前版本备份，记录批次 manifest；回滚前也复查新版本，避免覆盖执行后的其他编辑。

| 所有者 | 唯一职责 |
| --- | --- |
| `documents` | DocumentId、generation、revision、路径绑定、baseline、dirty 与冲突状态 |
| `editor` | 唯一文本、文本事务、undo/redo、immutable snapshot、编辑差分 |
| `file_io` | 编码、BOM、实际字节、文件身份、保存提交、磁盘备份和 watcher 复核 |
| `session` | 标签、窗口与视图状态、恢复 checkpoint/delta、durable revision 和恢复 UI |
| `settings` | settings.toml、校验、配置版本、配置写入和迁移 |
| `search` | 匹配与替换计划、预览、批次编排；不越过 documents/editor 修改已打开文件 |

crate 依赖遵循总架构：`file-io` 不直接导入 `storage` 或 `app`。原始快照、保存操作日志等持久化请求使用定义在 `contracts` 的 `BlobStore` / `SaveJournal` 端口，由 `storage` 实现并在 app 组合时注入。语义上 `file-io` 管理 source 的生命周期，物理上只有 `storage` 线程执行 SQLite；避免以持久化为由形成反向依赖。

## 7. 会话、恢复库与备份

### 7.1 单一恢复存储与 schema

默认一个本地 profile 的 `recovery.sqlite3`，由专用线程持有写连接，其他线程通过请求/回复通道访问。窗口可共享 profile，不为每个标签建立独立数据库；多进程实例使用不同 profile 或让单实例宿主统一管理，不能各自竞争维护同一恢复队列。

启动设置并读取核实：`journal_mode=WAL`、`synchronous=FULL`、`foreign_keys=ON`、`busy_timeout=5000`。FULL 只对成功提交的事务提供 SQLite 文档描述的同步策略，不能包含尚未入队的按键，也不能修复损坏存储设备。WAL 必须处于本机支持的文件系统；不将 WAL 恢复库直接放在 UNC/网络 profile。[SQLite WAL](https://www.sqlite.org/wal.html)、[synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous)

便携版默认配置可放在便携目录；若该目录是网络盘，恢复库放本机 `data_local_dir` 并用 profile 标识关联，界面说明实际恢复位置。只复制 `.sqlite3` 而忽略有效 `.wal` 可能丢数据；导出/迁移用 SQLite backup API，不能一边写入一边复制数据库文件。[rusqlite backup](https://docs.rs/rusqlite/latest/rusqlite/backup/index.html)

建议表结构：

| 表 | 关键字段与用途 |
| --- | --- |
| `schema_meta` | schema_version、profile_id、创建/迁移版本 |
| `documents` | document_id、path_utf16、generation、encoding/BOM、baseline stamp、source_snapshot_id、last_durable_revision、关闭状态 |
| `sources` | source_snapshot_id、总字节数、完整摘要、generation、complete；只允许引用已封口或明确仍在准备的source，不读取不完整组作为原样保存源 |
| `source_chunks` | source_snapshot_id、chunk_no、bytes BLOB、chunk digest；存实际原始字节，无 Unicode 重编码 |
| `checkpoints` | checkpoint_id、document_id、revision、serialization JSON、完整文本摘要、完成标记 |
| `checkpoint_chunks` | checkpoint_id、chunk_no、UTF-8 bytes BLOB、chunk digest；保持原始 CR/LF，不跨 UTF-8 字符边界切文本 |
| `edit_batches` | document_id、from_rev、to_rev、transaction_id、操作序列与插入文本 BLOB、批次摘要 |
| `view_state` | window/view/document id、选区、滚动和布局 JSON；只有 UI 状态，无第二份正文 |
| `save_ops` | operation_id、snapshot_rev、target/temp/rollback UTF-16、expected stamp、output digest、状态及错误 |
| `backup_manifest` | batch/save operation、原目标身份/戳、备份位置、摘要、保留原因 |

source chunks 默认 1 MiB，text checkpoint chunks 默认 1 MiB 并在 UTF-8 边界分块。大 checkpoint 可以分批写入标记为 incomplete 的 generation；只有完整摘要验证通过的最后事务才移动 active checkpoint 指针。恢复绝不读取 incomplete generation。

delta 是“针对哪个 revision，在哪些 UTF-8 字节范围替换成什么文本”的已提交编辑批次，包括 undo/redo 实际产生的差分；不是另一套 undo 栈。偏移作用于批次开始版本；多范围编辑按统一契约从后向前应用。批次起止 revision 和内容摘要必须连贯，否则停止恢复到上一个完整点。序列化 metadata 变化也必须进入同一个可恢复批次。

恢复流程：加载最近完整 checkpoint -> 顺序验证并回放连续 delta -> 恢复相应编码/BOM -> 计算摘要 -> 以独立“恢复的副本”文档打开 -> 展示原路径当前版本供比较/保存。恢复副本不能自动覆盖磁盘当前文件。损坏、跳号或不匹配的后续批次隔离保留，不猜测补齐。

### 7.2 默认配置与持久化节奏

```toml
[files]
default_encoding = "utf-8"
default_bom = false
new_document_eol = "crlf"
preserve_pasted_eol = true
external_change = "prompt"
legacy_encoding_open = "preview_then_confirm"

[recovery]
enabled = true
flush_interval_ms = 2000
max_queued_age_ms = 5000
checkpoint_interval_s = 60
checkpoint_delta_bytes = 8388608
checkpoint_delta_count = 1000
closed_document_retention_days = 7
disk_soft_limit_bytes = 2147483648

[backup]
on_manual_save = false
on_batch_replace = true
versions_per_file = 20
retention_days = 30
```

每 2 秒提交有变化文档的 delta；任一 delta 达 5 秒仍未 durable 时优先提交，并显示尚未保护的状态。上述 5 秒是正常负载的验收目标，初始大快照、慢盘/写满时不能虚报达成。窗口退出应等待最后批次或明确告知失败；不能仅依赖析构函数保证落盘。

delta 达 60 秒、8 MiB 或 1000 条之一时构建新 checkpoint；UI 只提供廉价不可变快照，序列化和 hash 在 worker 上执行。新 checkpoint 已完整提交后才删除旧 delta；至少保留前一个完整 checkpoint 到下一次启动校验。

SQLite WAL checkpoint 与“文档恢复 checkpoint”是两种不同操作。控制 SQLite 长读事务，空闲时做 PASSIVE checkpoint；关闭时在允许范围内做 TRUNCATE checkpoint，busy 时保留 WAL，不删除它来强行腾空间。不能为追求 WAL 文件小而每按键 checkpoint。

软容量达到 2 GiB 时先回收已明确关闭且过期的文档、废弃 incomplete generation 和已验证无引用的历史 source；**不自动删除仍打开/未保存文档的唯一恢复点**。无法继续持久化时停止宣称保护成功、提示位置与原因，并允许用户保存副本。默认不加全文压缩依赖，若测量需要再引入并单独版本化格式。

“恢复快照”保留尚未保存的当前工作；“保存前备份”保留上一个磁盘版本；“会话清单”只保留标签/视图/路径。三者独立，不因关闭一个标签自动删掉用户启用的长期备份。长期备份文件命名含文档 ID/摘要/操作 ID，不能只用同名 basename 导致不同目录文件冲突。

### 7.3 启动、损坏和迁移

先检查 SQLite 可打开与版本；迁移前使用 backup API 生成本地一致副本，再在事务内迁移 schema。遇到比程序新的 schema 不覆盖，提示升级或启用独立新 profile。

未干净退出时核实完整 checkpoint、连续 delta、Prepared save_ops；逐个展示恢复候选与最后 durable 时间。数据库损坏时隔离原库及 WAL/SHM、保留文件，不自动重建后覆盖原库；允许新 profile 继续使用，同时给出可检查的旧库位置。

关闭文档的“丢弃修改”是明确的用户动作：记录 tombstone 并推进 session 状态，防止下一次启动把已主动丢弃内容重新作为待恢复文档。GC 仍遵循保留期，不影响已导出的长期备份。

## 8. 设置持久化

`settings.toml` 由 `settings` 唯一写入，带 `schema_version`，反序列化为强类型结构后校验范围。设置界面修改经短时间合并后以临时文件 + 替换保存，不等待程序退出；失败显示未持久化状态。首次写入也必须使用 no-clobber 创建语义。

手工修改配置时先检查磁盘版本，不能静默覆盖外部编辑。解析失败保留原文件，使用最后已验证配置或默认值启动并显示错误位置；不把默认值自动写回坏文件。未知字段在主版本兼容范围内保留或明确提示，不能无声删除用户设置。

配置、布局、最近文件和恢复正文分开保存；用户关闭恢复功能不应丢失主题等设置。工作区内的普通文件不能通过配置内容自动执行命令或改变 profile 存储位置。

## 9. 对其他模块的接口契约

```rust
trait FileIoService {
    fn open(&self, request: OpenRequest) -> Task<OpenOutcome>;
    fn probe(&self, binding: TargetBinding) -> Task<FileProbe>;
    fn prepare_save(&self, snapshot: SaveSnapshot) -> Task<PreparedSave>;
    fn commit_save(&self, prepared: PreparedSave) -> Task<SaveOutcome>;
    fn decode_preview(&self, source: SourceSnapshotId, encoding: EncodingId)
        -> Task<DecodePreview>;
}

trait RecoveryService {
    fn checkpoint(&self, snapshot: RecoverySnapshot) -> Task<DurableAck>;
    fn append(&self, batch: CommittedDocumentDelta) -> Task<DurableAck>;
    fn recover(&self, document: DocumentId) -> Task<RecoveredCandidate>;
}
```

任务返回公共 `OperationError`，包含可匹配的领域 detail，而非可随意解析的字符串。detail 最少包括 `MalformedInput`、`UnmappableCharacter`、`ChangedDuringRead`、`Conflict`、`UnsupportedFileKind`、`DiskFull`、`PermissionDenied`、`SharingViolation`、`NeedsReconciliation`、`RecoveryUnavailable` 和 `Cancelled`；例如不可编码错误映射为 `OperationError::EncodingLoss` 并附字符、偏移和目标编码。错误同时携带安全的显示文本与操作 ID，不在日志写入完整正文。

取消在准备阶段停止可撤销工作；进入 Windows 提交临界区后先取得确定结果或进入 reconciliation，再报告最终状态，不能一边显示“已取消”一边仍提交磁盘。`DurableAck` 明确包含 document ID、generation、实际 durable revision 和持久化时间，UI 不把“请求已入队”显示为“已恢复保护”。

## 10. 实施 AI 任务与验收

| 任务 | 交付物 | 验收与依赖 |
| --- | --- | --- |
| IO-01 编码与原始字节 | `text-codec`、原始 source chunks、编码诊断和 preview API | UTF-8/BOM、UTF-16 LE/BE、GBK、GB18030 无修改逐字节往返；GBK 四字节输入、GB18030 非规范序列与 19 PUA 边界按定义处理；不可编码字符不输出实体或问号；所有字符分块边界与局部raw/UTF8映射正确 |
| IO-02 换行与 Kit 接缝 | 原始换行验证用例、必要的 editor patch、派生 EolIndex | CRLF/LF/CR/混合/无末尾换行经 Enter、paste、delete、批量转换、undo/redo 后原样保存；CR-only 行索引与括号/解析位置正确；未通过不得改用隐含 LF sidecar |
| IO-03 身份与冲突 | Windows FileProbe、文档注册与 FileStamp | 大小写别名、硬链接、同名不同目录、路径替换、同长度同时间改写均不误当相同版本；新 reparse 发现后拒绝原位保存 |
| IO-04 保存状态机 | prepare/commit API、save_ops、错误分类与启动 reconciliation | temp 写满、权限/只读/占用、1175/1176/1177模拟、提交前后强杀有可检查结果；不自动 truncate；保留唯一原版/保存版候选；已成功保存后刷新 file ID |
| IO-05 dirty 与并发保存 | SaveBaseline/EncodingSpec、revision guard | 输入/撤销回原值、只改 BOM/编码、保存时继续输入、保存时改编码、关闭后晚到结果、连续 Ctrl+S；baseline 只推进到实际输出 snapshot |
| IO-06 watcher 与批量接口 | notify service、buffer/disk 两条替换入口 | 自身保存事件不吞掉外部事件；rename/replace 重绑定；dirty 文档不自动重载；已打开批替换只改缓冲；部分成功逐文件报告与回滚冲突检查 |
| IO-07 SQLite 恢复 | migration、source/checkpoint/delta、DurableAck、restore UI 数据接口 | 初始 checkpoint 中断、delta 跳号/损坏、写满、WAL存在、schema升级、主动丢弃、未命名文档恢复；只到已确认 durable 点；恢复不覆盖原文件 |
| IO-08 配置与备份 | typed settings、TOML、安全写入、backup manifest 与 GC | 配置损坏/外部修改/不支持schema可启动且不覆盖；同名文件备份不冲突；关闭恢复不删配置；容量清理不删活跃文档唯一恢复点 |
| IO-09 容量回归 | release 性能数据与峰值内存/恢复磁盘用量 | 100 MB 打开、全文替换、一次撤销/重做、保存再打开；不逐键复制/hash全文；实际 GBK/UTF-16 解码膨胀计入预算；1 GB reader 不误入全文恢复路径 |

IO-01、IO-02、IO-03、IO-04 的最小原型在 M0 验证；IO-05 至 IO-08 随 M2 完整交付，IO-06 的批量部分与搜索阶段接合。文件保存、恢复与配置工具不能绕过本规格自行实现另一条简化写入路径。
