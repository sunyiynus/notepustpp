# 搜索替换与大日志实现规格

版本：0.1。日期：2026-11-21。适用范围：Windows 首版、100 MB 内普通编辑、1 GB 级只读日志。本文给出默认技术方案和开发任务；库的接口已对照官方资料，依赖组合、编译与性能仍需 M0 实证。公共数据类型以 [ARCHITECTURE.md](ARCHITECTURE.md) 为准。

## 库选型与所有权

| 功能 | 默认库或系统接口 | 应用负责实现的部分 |
| --- | --- | --- |
| 单个区分大小写的字面量 | memchr 2.x 的 memmem::Finder / FinderRev | Rope 分块、边界拼接、范围过滤、结果定位与取消 |
| 多关键词高亮 | aho-corasick 1.x | 模式数量与内存上限、匹配优先级、可视区装饰；不把 ASCII folding 当作 Unicode 大小写处理 |
| 常规正则 | regex 1.x | 常规模式默认引擎、编译预算、统一替换语法、后台进程协议 |
| 高级正则 | pcre2 0.2.x，仅在 search-worker 中加载 | 用户显式切换、前后查找/反向引用、错误转换、超时与硬取消 |
| 目录枚举与忽略规则 | ignore 0.4.x | 统一 WorkspaceFilterPolicy、去重、限流、错误收集 |
| 文件包含/排除模式 | globset 0.4.x | 模式验证、根路径语义和筛选原因说明 |
| 任务与结果通道 | std 线程、AtomicBool、架构层提供的有界 async-channel | 两个搜索槽位、背压、过期结果丢弃和 UI 节流；不新增 Tokio runtime |
| 进程控制与 IPC | std::process / std::io、serde / serde_json；Windows Job Objects 由 platform 封装 | 持久 worker、帧协议、快照寿命、超时、kill 与 wait |
| 日志随机读取 | std::fs::File；Windows FileExt::seek_read，Unix FileExt::read_at | 自有 ReadAt 适配、短读重试、文件身份与变化检查 |
| 日志缓存 | 应用自有按字节预算的 LRU | 淘汰按实际字节计算，不按“条目数”假定内存有界 |
| 解码与源偏移 | text-codec 的 encoding_rs 解码服务 | 复用同一编码规则；本模块消费 codec checkpoint 与偏移映射，不再实现一套转码 |
| 尾随事件 | contracts 端口注入的监视与身份检查；实现使用 notify + 定时轮询 | 追加、截断、轮转、替换状态机；large-file 不反向依赖 file-io |
| 超长行与选区 | GPUI TextSystem、自有分段布局；unicode-segmentation 1.x、unicode-width 0.2.x | 字符边界、列网格、可视区布局与跨段选择 |

memmem 可处理任意字节，空模式会在每个字节位置产生匹配，因此界面将空字面量视为“清空搜索”，不执行匹配。aho-corasick 的内置不区分大小写属于 ASCII 范围。[memmem 文档](https://docs.rs/memchr/latest/memchr/memmem/)、[aho-corasick 文档](https://docs.rs/aho-corasick/latest/aho_corasick/)

regex 不支持前后查找和反向引用；高级模式选择 PCRE2，禁止遇到编译错误时静默换引擎。两个引擎共享应用的搜索请求、结果和替换语法，模式语义差异由帮助与测试样例说明。[regex 文档](https://docs.rs/regex/latest/regex/)、[pcre2 builder 文档](https://docs.rs/pcre2/latest/pcre2/bytes/struct.RegexBuilder.html)

上述版本线用于声明依赖意图。最终精确版本、features 和 Cargo.lock 由 M0 在全工作区统一解析并编译锁定，不能在各功能模块独立升级同一个库。

## 搜索模式与行为

| 模式 | 普通编辑文档 | 文件夹搜索 | 大日志浏览 |
| --- | --- | --- | --- |
| 区分大小写字面量 | 支持任意选区和全文，包含跨行字面量 | 对可严格解码的文件搜索 | 分块流式搜索，支持跨块字面量 |
| Unicode 不区分大小写字面量 | 将模式 regex::escape 后交给常规引擎，不先将原文转小写 | 同左 | 仅对转义后的有限宽度字面量进行跨块匹配，支持超长行；不使用行级正则限额 |
| 常规正则 | 支持捕获组、多行与跨行，受快照和执行预算限制 | 每个符合容量限制的文件完整匹配 | 仅行级正则，默认最大解码行 8 MiB |
| 高级正则 | 用户显式选择 PCRE2，支持其可用高级语法 | 可用，逐文件复用 worker 中的编译结果 | 首版禁用，避免将 1 GB 文件整体作为回溯输入 |
| 替换 | 当前命中、选区、全部命中；每次操作有统一撤销边界 | 预览后逐文件提交；已打开文件只改缓冲 | 首版只读，不提供替换 |

普通编辑器的搜索坐标是未归一化的真实 UTF-8 文本偏移，CRLF、LF、CR 保持原样。磁盘编码字节位置使用 RawByteOffset；UTF-8、UTF-16 code unit 和显示列不能互换。

默认设置：区分大小写关闭、整词关闭、正则关闭、环绕搜索开启。开启正则时默认常规引擎；高级引擎属于显式选项。搜索栏输入防抖 150 ms，输入法组合未提交时不触发新查询；Enter 立即执行，Esc 取消并返回编辑器焦点。

“整词”在应用中统一定义为命中前后不紧邻 Unicode 单词字符，采用常规引擎的 Unicode 单词字符规则；中文不做自然语言分词。字面量候选通过邻接字符检查，不能仅检查 ASCII 字母和下划线。搜索不用 Unicode 规范化或全文转小写，以保持偏移与原文一致。

常规引擎启用 Unicode、CRLF 模式和多行锚点，dot-all 默认关闭；高级引擎设置 UTF、UCP、CRLF、多行锚点，dot-all 同样默认关闭。显式的模式内标志按相应引擎解释。模式中的字面 CR、LF 仍匹配原字符；不进行换行重写。[regex builder 的 CRLF 与 Unicode 行为](https://docs.rs/regex/latest/regex/struct.RegexBuilder.html)

两个引擎的 CRLF 开关并不等价：常规 regex 的多行锚点处理 LF/CRLF，不把孤立 CR 当作换行；pcre2 的 crlf(true) 将 CR、LF、CRLF 都视为行界。帮助和兼容用例必须明确这个差异，CR-only 文件需要这些锚点时使用显式高级模式，不能改写原文或静默切换引擎。[pcre2 的 CRLF 行界说明](https://docs.rs/pcre2/latest/pcre2/bytes/struct.RegexBuilder.html#method.crlf)

查找下一个从当前选区末端开始，查找上一个从选区起点之前开始；环绕最多一次。当前命中失效或文档版本变更时重新搜索。选区内搜索按真实范围执行，不能先截取文本而无意改变前后查找和文件锚点含义；M0 用明确样例冻结“选区边界是输入边界还是仅结果过滤”的行为，默认选择后者，即保留全文上下文，仅接受完全位于范围内的命中。

## 普通文档字面量算法

1. 从 EditorAdapter 取得不可变 SnapshotHandle 和 DocRevision，不在每个按键事件中调用全文 value()。
2. 区分大小写字面量优先用 Finder。按 Rope chunk 顺序扫描，保留最多 pattern_utf8_len - 1 的跨块尾部；只报告起点属于当前负责范围的命中，防止重叠窗口重复计数。非空模式最大 UTF-8 长度为 64 KiB。
3. 每处理一个块和每发出一批结果检查取消、文档关闭和 query_id。命中缓存只保留限额内的范围，不创建与文档长度等比例的逐字符表。
4. Unicode 忽略大小写通过转义模式后走常规正则 worker。多关键词高亮使用 aho-corasick，默认模式不超过 64 个、总模式字节不超过 64 KiB，只绘制当前视口附近的命中。
5. 所有匹配先验证 UTF-8 边界，再转换为 EditBatch 或装饰。点击结果时比较 DocRevision，过期则定位到可验证的新命中，不能直接沿用旧 offset 修改文本。

## 正则 worker 与硬取消

所有正则请求统一走持久 search-worker，包括常规和高级引擎。应用保留两个槽位：当前文件/打开文件槽，以及目录搜索/日志行级正则槽。每个 worker 同时执行一个 job；正常请求复用进程和编译模式，取消或超时则销毁对应进程并重建，不能按每个文件重启进程。

输入流程：EditorAdapter 在固定修订上生成 UTF-8 快照，后台分块写入应用管理的临时文件；worker 接收快照路径与版本后读取。控制消息不携带整份 100 MB 文本。目录文件通过 file-io 读取、text-codec 严格解码成快照，不能由 worker 使用不同的默认编码。

IPC 采用架构层定义的“小端 u32 长度 + UTF-8 JSON 控制帧”，单帧最大 1 MiB。平台路径采用 NativePath：Windows 为 UTF-16 单元数组，Unix 为原始字节数组；显示标签才允许有损展示。wire 消息遵循架构层的 Hello、Ready、SearchRequest、MatchBatch、Progress、ReplacementReady、Finished、Failed，使用 job_id 与 SnapshotId 关联输入，文档版本保留在父进程的关联表。SearchRequest 显式区分 Purpose::Find 与 Purpose::ReplacePlan，后者的完整执行不受预览条数限制。目录聚合器在父进程另外产生 Started、FileBegin、FileDone 等应用事件；不要为每个事件修改 wire 协议。一次目录父任务可以串接多个单快照子请求，每个请求独立标识，但复用同一 worker 与已编译模式。错误有可判断的枚举码，不靠解析错误文案控制流程。

默认资源预算：

| 项目 | 默认上限或行为 |
| --- | --- |
| 正则模式 | UTF-8 64 KiB；编译失败给出位置与原因 |
| 常规引擎编译大小 | RegexBuilder::size_limit 8 MiB；DFA 缓存 8 MiB；嵌套层数 128 |
| 普通文档快照 | 最大 384 MiB 解码后 UTF-8，覆盖 100 MB 合法 GBK 字节解码最多约 3 倍膨胀的场景；超限给出明确容量结果，不截取后伪装全文搜索 |
| worker 内存 | 每进程 768 MiB；Windows 通过 Job Object 的进程内存限制执行 |
| 单文件匹配预算 | 常规 5 秒，高级 2 秒；用户可显式提高至最多 30 秒 |
| 取消 | UI 立即反馈，父进程 kill，再 wait 回收；候选目标 1 秒内不再计算或发送结果 |
| 目录任务 | 单文件超时可记录后继续；全任务另有用户取消，不用每文件 2 秒限制冒充整个目录 2 秒 |
| 编译缓存 | 每 worker 最多 16 个模式，并计入 32 MiB 缓存预算；淘汰后重新编译 |

worker 是独立的 search-worker.exe，不链接 GPUI；使用标准库以隐藏窗口方式启动。启动时先与父进程握手，加入 Job Object 并设置内存限额后才接收执行请求。主程序退出时关闭 Job handle，子进程一并终止；不能在 UI 线程等待进程或管道。stdout 仅输出协议，stderr 由有限日志收集器读取，避免管道填满死锁。父进程持续读取，但结果接收仍受总字节预算与过期检查约束。

pcre2 高层 RegexBuilder 没有公开 match_limit/depth_limit setter，本文不要求 AI 调用不存在的方法。首版关闭 JIT，使用高层 wrapper 与进程内存、墙钟超时限制；若后续采用 pcre2-sys 的 native match context，再单独实现和验证 native limits，不能把模式前缀或丢弃 Future 当作硬取消。[pcre2 builder](https://docs.rs/pcre2/latest/pcre2/bytes/struct.RegexBuilder.html)、[PCRE2 native match limit](https://www.pcre.org/current/doc/html/pcre2_set_match_limit.html)

std::process::Child 被丢弃不会自动结束子进程，终止后也必须 wait 回收。Windows Job Object 的进程内存限制与终止由 platform 模块封装。[Rust Child 生命周期](https://doc.rust-lang.org/std/process/struct.Child.html)、[Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)

## 结果流与分页

每批最多 500 条且序列化后不超过 512 KiB，先到达任一上限就发送。批次使用文件 ID，路径在 FileBegin 中发送一次；每条预览最多 200 个 Unicode 字符，超长匹配显示省略位置，不能把整条超长行塞进结果消息。

父进程通道有界，最多积压 8 个批次；结果模型连同路径、snippet、范围及索引合计最多 32 MiB。默认全任务最多显示 10,000 个命中，达到结果数量或字节上限即停止 find-all 扫描并标记 Truncated。界面显示“已找到至少 N 项，结果已截断”，不能把 N 当作精确总数。列表对已收集结果分页/虚拟化；需要更多结果时由用户缩小条件或显式提高预算后重查。

顺序不同的并行目录结果由文件 ID 与批次序号归并，UI 不要求等待全局排序完成才显示首批。任务完成前显示“已搜索文件数”和实际进度，目录总数未知时不用虚构百分比。失败、跳过、超时、不可解码、文件过大和长行跳过均单独计数，并提供原因。

ReplaceAll 的完整扫描独立于预览的 10,000 条上限，不能只替换前 10,000 个却显示“全部替换”。

## 替换语言与提交方案

应用自有 ReplacementParser，两个正则引擎均只提供捕获区间，不直接调用库各自的 replace_all 语法。

| 替换输入 | 规则 |
| --- | --- |
| $0 | 整个匹配 |
| $1 到 $99、${name} | 编号或命名捕获组；未参与本次匹配的可选组展开为空 |
| $$ | 字面美元符号 |
| \n、\r、\t、\\ | 仅正则替换且开启“解释转义”时有效；未知转义报输入错误 |
| 普通字面量替换 | 默认完全按输入文字处理，不展开捕获组和转义 |

引用不存在的组在执行前报错。新增换行不暗中改写为原文件主换行；用户可使用替换转义选择 LF/CRLF，或执行专门的换行转换命令。

标准模式与高级模式都采用从左向右、不重叠的匹配。默认使用引擎的原文 offset API，不能每次切开子串导致文件锚点或前后查找语义改变。零宽命中后推进到下一个合法 UTF-8 标量边界，EOF 的空命中最多报告一次；对 a*、^、$、空备选和 emoji 分别冻结结果序列。空模式输入不启动替换。

单次替换流程：取得源修订 → 验证命中及捕获 → 展开替换 → EditorAdapter 在同修订提交一个事务 → 更新命中与焦点。一次多光标或替换全部对应一个撤销事务。

全文替换使用 Purpose::ReplacePlan：父进程提供 ReplacementParser 的统一 AST 和预创建的私有 job 输出路径；worker 边匹配边将原快照未修改片段与展开后的替换流式写入该临时产物。worker 只向父进程回传限额内的 MatchBatch 预览，不跨 IPC 传递百万条捕获记录；达到预览上限后仍完成全文匹配与替换统计。默认输出上限 512 MiB，任务临时文件总预算 1 GiB；超限、错误或取消时整份文档不提交。

完成后 worker 发出 ReplacementReady，包含 source SnapshotId、输出 artifact 路径与身份、UTF-8 长度、BLAKE3 摘要、完整替换数量和成功完成标识。worker 只能写父进程授权的 job 临时产物，不能写真实目标文件。父进程验证产物位于本 job 管理范围、长度与摘要一致、源快照和原文档修订仍有效，再调用架构层 EditorAdapter.apply_replacement_artifact 形成一次可撤销事务；后台保存与恢复同样消费版本化快照。不能调用会清空撤销的 set_value。缺少对应 Kit API 时，在 M0 补齐适配/上游补丁后再接全文替换。取消/崩溃留下的半成品只有临时状态，启动清理和引用释放时删除。

文件夹替换分为 Plan 与 Apply。Plan 固定每个目标的 FileIdentity、FileStamp、文本修订、编码与命中摘要，预览仅保留限额内的片段。Apply 在每个文件前复查版本并重新验证匹配，已打开文档只更新当前缓冲；未打开文件调用 file_io 的可靠保存服务。逐文件记录 Applied、Conflict、Skipped、Cancelled、Failed 与备份引用。发生冲突不覆盖目标；取消保留已完成文件并显示摘要，跨文件操作不宣称全局原子提交。

## 文件夹枚举与统一过滤策略

WorkspaceFilterPolicy 由工作区拥有，文件树、快速打开和搜索共用同一份规则与解释器。搜索只额外增加包含模式、文件容量和内容解码条件。

默认配置如下，必须通过显式 builder 设置，避免依赖 ignore 的隐含默认：

- 包含隐藏文件：WalkBuilder.hidden(false)，使 .env 等配置文件可发现；固定排除 .git、.hg、.svn 元数据目录。
- 遵守工作区内 .ignore、.gitignore、.git/info/exclude；require_git(false)；git_global(false)、parents(false)，不让机器全局或工作区外的父级规则悄悄改变结果。
- 不跟随符号链接/重解析点：follow_links(false)。显式打开的单个链接文件由 file_io 解析身份，递归目录默认不跨链接，避免循环和搜索范围扩张。
- 用户排除项默认 node_modules、target、dist、build，可在设置中关闭；使用相对工作区路径、/ 分隔符、globset literal_separator(true)。包含模式默认 **/*，非法 glob 在启动任务前报错。
- 单个普通搜索文件原始大小默认上限 100 MB；超过上限记录跳过原因，可显式转为大日志搜索。严格解码失败或判定二进制时跳过，不进行有损解码后替换。编码策略复用 FILE_IO_RECOVERY_SPEC 的 DecodePolicy：文件夹替换默认自动处理 BOM/严格 UTF-8；传统编码必须在本任务显式设置 fallback 或在候选预览中确认。只读搜索允许的猜测也要记录来源，不能把“解码成功”当作“猜测已获确认”。
- 遍历在架构层已有的 2 线程 I/O 队列中执行，最多预取 4 个文件目标，不再启动独立的 ignore 并行线程池。默认不越过工作区根；FileIdentity 去重，两个路径指向同一个文件只搜索/替换一次。

被过滤路径按同一规则引擎返回原因；文件树可用“显示被忽略项”覆盖显示，但打开此显示开关不会自动扩张搜索范围。ignore 原生支持多级忽略规则及符号链接策略，globset 用于应用显式包含/排除项。[ignore WalkBuilder](https://docs.rs/ignore/latest/ignore/struct.WalkBuilder.html)、[globset](https://docs.rs/globset/latest/globset/)

## 大日志读取与索引

LargeFileSource 与普通 EditorState 分开实现，只有只读视图、选择复制、搜索和导航接口共享。禁止将 1 GB 文本全部读成 String 再传入 readonly Editor；首屏应在小范围读取后即可交互。

ReadAt 适配统一签名为“给定 RawByteOffset 读取至调用者提供的缓冲并返回实际字节数”，所有代码都处理短读与 Interrupted。Windows 使用稳定的 seek_read，并自行循环；不使用当前仅 nightly 的 seek_read_exact。Windows seek_read 会改变句柄的顺序 cursor，Unix read_at 不会，因此禁止在同一个 reader 句柄混用顺序 read/seek 与 offset 读取。reader、索引与搜索可各自持有专用读取句柄。[Windows FileExt](https://doc.rust-lang.org/std/os/windows/fs/trait.FileExt.html)、[Unix FileExt](https://doc.rust-lang.org/std/os/unix/fs/trait.FileExt.html)

默认顺序扫描块 1 MiB，首屏先读 64 KiB，视口预取上下各 2 个块。复用 [文件与恢复规格](FILE_IO_RECOVERY_SPEC.md) 定义的 StreamingDecoder、EolScanner、CodecCheckpoint 和 BoundaryMap：RawByteOffset 包含文件 BOM，普通文本 Utf8Offset 不包含 BOM；偏移映射通过最近检查点局部重解码与短期边界表取得，不全文件常驻逐字符表。解码服务返回完整字符边界、未完成尾部与源偏移映射；UTF-16 从 BOM 后的正确 2 字节边界起读，并处理代理对。GBK/GB18030 同样通过 codec-safe checkpoint 重启，不能按固定倍数从 UTF-8 偏移换算原始位置，也不能假定 GBK 标签的实际 decoder 永远只消费双字节字符。

索引同时维护两种检查点：每 4,096 个逻辑换行建立 LineCheckpoint；即使没有换行，也至少每 1 MiB 原始字节建立 DecodeCheckpoint。检查点包含文件世代、RawByteOffset、已确认行号、当前行起点及可重启的解码边界。连续 CRLF 算一个换行，跨块保留 pending CR；孤立 CR 与 LF 各算一个换行。尾部尚未完成的多字节字符和 CR 暂不作为最终完成状态。

跳转到已索引区域时从最近检查点继续扫描定位。跳到未索引行时启动高优先级定位任务，显示索引进度并可取消；总行数未完成前显示未知，滚动位置可按已读原始字节比例估算，不能将估算值写成精确行号。

缓存采用应用自己的按字节 LRU，以下预算合计不超过 256 MiB：原始读取缓存 32 MiB、解码/布局缓存 64 MiB、索引 64 MiB、搜索结果 32 MiB、在途解码/匹配 32 MiB、其他模块状态 32 MiB。索引接近预算时增大稀疏步长；任何一项达到预算要先淘汰或停止扩张。进程总内存、GPU 资源和正则子进程另行报告。

首版禁用整文件 mmap。后续若引入 memmap2，只作为有针对性的性能优化，需重新验证文件被截断、替换和轮转时的生命周期，不能作为默认可靠性假设。

## 超长行与日志搜索限制

超长行不能作为一个无上限 String、一个 GPUI layout 或一个 regex subject。视图按字节检查点找到行片段，每段最多 8 KiB 解码文本，先呈现当前视口所需内容，再渐进建立横向导航索引。默认关闭软换行和语法解析；不创建与整行字符数等长的 glyph 数组。

日志模式使用固定等宽网格，Tab 默认 4 列，中文宽字符占 2 列，组合字符与 emoji 按 unicode-segmentation / unicode-width 计算并在 M0 用真实 GPUI 字体测量校验。列宽函数返回列数，不是像素；复杂字形按视口局部布局。跨段保留字素上下文，若异常组合序列导致上下文超过 64 KiB，采用明确的转义显示片段并标记，仍保留原始偏移用于复制，避免无界累计。远水平位置尚无精确索引时显示准备状态，不一次性 shape 全行。[Unicode 分段](https://docs.rs/unicode-segmentation/latest/unicode_segmentation/)、[Unicode 列宽](https://docs.rs/unicode-width/latest/unicode_width/)

日志区分大小写字面量在解码后的 UTF-8 块中扫描，跨块保留有限 pattern_len - 1 尾部并映射回 RawByteOffset。Unicode 不区分大小写字面量必须先 regex::escape，使用 Unicode simple folding；此时模式只有固定数量的标量原子，无量词、前后查找或其他用户正则结构，最大匹配 UTF-8 宽度可保守界定为 4 × 模式标量数。因此可以按该上限加两侧字符上下文构建有界窗口，向常规 worker 提交小快照，复用编译模式；单行再长也不需要全行入内存。

有界窗口保留未决匹配起点和最后已报告的全局位置，使用保留上下文的 offset 搜索及全局游标去重，不能每块从尾部重新 find_iter 后简单丢重导致漏掉重叠候选。整词匹配位于窗口尾部时，等下一个完整字符确认右侧边界后再提交结果。该算法仅适用于从字面量构造的有限宽度模式，不能泛化到任意正则。原文件不是 UTF-8 时由 text-codec 将命中映射回原始字节，不能直接拿 UTF-8 模式搜索源编码字节。只需要当前视口的结果装饰；完整结果列表受全局 10,000 / 32 MiB 上限约束。

日志窗口交给 regex worker 时，父进程为每个有界窗口生成临时 UTF-8 小快照，以相同协议串接子请求，复用已编译模式。父端保留窗口的全局 UTF-8 锚点与 BoundaryMap，将返回的窗口内范围转换为日志 RawByteOffset；处理完成立即释放该窗口资源。不能为此先生成整份 1 GB 的 UTF-8 快照，也不能把窗口文本塞进超限控制帧。若M0发现小快照落盘成为瓶颈，再在同一协议版本管理下增加有界流式输入，不改变匹配范围语义。

日志行级正则逐行向常规 worker 提交受限快照/片段，复用已编译模式；默认每条逻辑行最多 8 MiB 解码数据。超过上限时继续扫描以找到下一行，记录 LongLineSkipped，结果状态必须是“不完整”，并提供字面量搜索入口。不得把一个长行切成独立小行后声称锚点、贪婪和捕获语义不变。大日志不支持跨行正则及高级 PCRE2；该限制在模式选择中直接表达。

## 尾随与轮转状态机

日志源持有 FileIdentity、FileStamp、LogGeneration、已确认字节位置、最后完整解码位置、最后确认行状态。一次搜索固定开始时的 observedLength，普通追加不扩张这次查询范围；“跟随并持续搜索”另建作业。notify 事件只是重新检查提示，同时每 500 ms 轮询文件身份与大小；窗口后台可降低频率。读取句柄允许共享读取、写入和删除，以便日志生产者追加和轮转。

| 事件 | 处理 |
| --- | --- |
| 同一身份、大小增长 | 从最后完整解码位置继续；保留跨块尾部；更新索引与尾部视口 |
| 同一身份、大小减少 | generation 增加，旧索引/结果失效，从头或新尾部重建，并显示文件已截断 |
| 路径指向新身份 | 显示文件已轮转；默认切换新文件，旧句柄按任务引用释放；不能把两个文件拼为同一原始偏移空间 |
| 大小不变但修改时间或内容指纹变化 | 视为重写，失效受影响索引，无法确定范围则新 generation 重建 |
| 文件暂时消失或占用 | 保留已显示内容和错误状态，退避重试；不把短暂缺失当作空文件覆盖模型 |
| 用户暂停尾随 | 停止自动滚动，继续限额内的源状态检查；恢复后定位到新尾部 |

尾随从末尾打开时先获取末尾块及解码安全边界，最后 N 行可立即显示；完整起始行号尚未索引时标记为尾部相对行号。代际变化后的搜索结果不能继续按旧偏移定位。

## 公共契约与任务拆分

公共类型由架构层定义，以下列出本模块需要的含义，不要求直接照抄为上游 API：

- SearchQuery：query_id、Literal/RegexStandard/RegexAdvanced、原模式、大小写/整词/dot-all、搜索范围和结果预算。
- SearchTarget：已打开文档的 DocumentId + DocRevision + SnapshotHandle，或磁盘文件的 NativePath + FileIdentity + FileStamp，或日志 LogGeneration + 编码检查点。
- SearchMatch：目标 ID、源版本、Utf8Range 或 RawByteRange、行号是否精确、限长 snippet；不能只存一个无法判定坐标系的 usize。
- SearchBatch：job_id/query_id、序号、目标 ID、结果数组、扫描进度及限额状态。Finished 的 Complete/Truncated/Cancelled/Partial/Failed 分别表示不同结果。
- ReplacePlan：固定源版本、统一替换 AST、完整匹配统计、临时产物引用、受限预览、冲突状态；提交由文档/文件服务完成。
- LargeFileSource：ReadAt、codec checkpoint、行/解码索引、generation、缓存预算和尾随状态；不返回整份全文字符串。

| 任务 | 前置条件 | 具体交付与验收 |
| --- | --- | --- |
| S01 搜索契约与字面量 | SnapshotHandle/DocRevision、公共偏移类型 | 真实 Rope 跨块字面量、反向/环绕、整词、UTF-8 边界；无全文热路径复制 |
| S02 worker 协议 | platform 子进程和临时文件管理 | 两槽位、Purpose、帧限额、NativePath、ReplacementReady、超时、kill+wait、退出清理；协议畸形输入不会无限分配 |
| S03 常规与高级正则 | S02、严格 UTF-8 快照 | 固定引擎切换、CRLF/Unicode/捕获、零宽推进；灾难回溯取消后进程确实退出 |
| S04 结果面板 | S01/S03、GPUI VirtualList | 首批结果先显示；500 条/512 KiB 帧、10,000 条和 32 MiB 截断状态准确 |
| S05 文件夹搜索 | S04、WorkspaceFilterPolicy、text-codec/DecodePolicy | 隐藏配置、gitignore、glob、链接、同文件去重、解码/大小错误；不阻塞 UI |
| S06 替换与预览 | S03、EditorAdapter 事务、file_io 保存 | 单次与全文撤销、超过预览上限仍完整替换、版本冲突与部分失败；已打开文档不自动保存 |
| L01 reader/codec/index 原型 | ReadAt、text-codec checkpoint、contracts 监视端口 | 1 GB 首屏、小缓存、UTF-8/16/GB18030 跨块、CRLF/CR、10 MB 单行无全行 layout |
| L02 日志视图 | L01、GPUI 布局与选择接口 | 视口预取、超长行横向导航、复制的原偏移映射、未索引行跳转可取消 |
| L03 日志搜索 | L01、S01/S03/S04 | 大小写字面量跨块、有限宽度窗口去重、受限行级正则、长行跳过可见、结果随 generation 失效 |
| L04 日志尾随 | L01/L02、notify/FileIdentity | 追加、跨块未完成字符、截断、轮转、暂时消失、暂停与恢复尾随 |

## 最低验收集

1. 匹配跨越 Rope chunk、1 MiB 读取边界、CR/LF 分界和多字节字符边界；结果始终映射到原文本或原文件的正确位置。
2. Standard 对 lookaround/backreference 明确报不支持；Advanced 能执行对应受支持样例。两个模式均不承诺完整 Boost 兼容。
3. 验证 ^、$、a*、空备选、Unicode 大小写、中文整词规则、捕获组不存在/未参与、$$、换行替换，以及 EOF 零宽匹配终止。
4. 100 MB 全文查找/替换、一次撤销/重做、保存重开；在百万命中情况下，预览、完整替换和临时文件配额各自正确。
5. 目录含 .env、.gitignore、.ignore、链接循环、同文件多路径、长中文路径、权限拒绝、编码不明文件；每个跳过原因可见。
6. 新查询覆盖旧查询、文档关闭、搜索取消、worker 超时和崩溃后，不再提交旧结果；CPU、工作进程和管道资源被回收。
7. 1 GB 文件首屏不等待全文索引，缓存与索引不超过预算；1 MB 和 10 MB 单行可以浏览和字面量搜索，正则长行限制如实报告。
8. UTF-16 的奇数字节尾部、跨块代理对、GB18030 四字节序列、混合 CRLF/LF/CR、尾部未完成字符在继续追加后恢复正确。
9. 日志追加、截断、重命名轮转、原路径新文件替换、同大小重写和休眠后恢复；generation 与过期搜索结果处理正确。

执行 AI 应先完成 S01/S02/L01 的可运行验证，再并行连接界面和其余功能。发现锁定 GPUI-Kit 缺少必需的快照/事务 API 时，提交具体缺口与最小补丁方案；不能通过另加权威缓冲、全量 set_value 镜像或删除验收用例绕开约束。
