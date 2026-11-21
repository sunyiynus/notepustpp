# AI 开发任务与交付协议

本项目使用 Rust 和 GPUI-Kit 重写 Notepad++ 主要核心体验。已确认：Windows优先、VS Code/Zed风格界面、100 MB完整编辑、1 GB日志按需浏览搜索尾随、列编辑/文件夹搜索/中文编码为首版必做，宏和插件后续。

把本文件与对应规格一起提供给开发AI。仅提供单个任务编号而不提供公共架构，容易产生不兼容的文本缓冲、搜索坐标和保存流程。

## 阅读顺序与决策优先级

1. 用户已经确认的范围见 ../PLAN.md。
2. docs/README.md 提供功能到库、规格和任务的索引。
3. ARCHITECTURE.md 定义共享类型、所有权、版本、并发和协议。
4. 按任务阅读 EDITOR_UI_SPEC.md、FILE_IO_RECOVERY_SPEC.md 或 SEARCH_LARGEFILE_SPEC.md。
5. BUILD_RELEASE_TESTS.md 定义构建、Windows集成、打包和验证。

公共类型和跨模块协议以 ARCHITECTURE.md 为准；某领域的算法、默认值和测试以对应规格为准。发现冲突时，在当前任务内提出具体修订和受影响调用方，统一后实现，不能静默维护两套相近类型。源码API与文档不符时以锁定版本源码为准，留下兼容性记录。

所有依赖选型都是本项目默认决策。开发AI应完成必要的API核实和编译，不重新自由挑选另一套UI、文本缓冲、数据库或搜索运行时。

## 可以直接复制的任务提示词

~~~text
请在当前仓库实现任务 <任务ID>。

先阅读 PLAN.md、docs/README.md、docs/ARCHITECTURE.md、
docs/AI_HANDOFF.md，以及任务所指向的领域规格和构建验证规格。
遵循用户已确认的首版范围与指定库，不更换GPUI-Kit。

开始时说明任务依赖是否具备、准备修改的模块、接口和验收用例。
依赖未实现时完成当前任务能独立交付的部分，明确指出缺失接口；
不能通过复制另一套buffer/undo/codec/文件身份类型绕开依赖。

实现指定行为并处理规格中的错误、取消、版本漂移和资源上限。
库API必须查看锁定版本源码/官方文档，不调用仅凭名字猜出的接口，
不能把pub(crate)方法当成公开API。必要的上游补丁集中放在适配层。

完成与本任务相关的自动化测试和构建检查。界面任务还要验证
真实键盘、焦点和输入；无法执行的真机检查明确标为未验证。
不要以占位返回值、静态mock界面或仅能编译作为功能完成。

交付：修改文件、实现说明、实际运行的命令和结果、验收映射、
已知限制、下一任务接口。更新功能状态与兼容性记录。
本任务范围内的可逆实现与验证继续完成，不添加逐步审批流程。
~~~

## M0 验证任务

M0使用独立原型或spikes目录。原型成果应变成正式适配层、fixture和决策记录；不在正式项目中长期保留第二套buffer或临时搜索引擎。

| ID | 依赖 | 负责范围 | 交付物与通过条件 |
| --- | --- | --- | --- |
| P00 | 无 | 工具链与依赖 | 记录Rust/MSVC/SDK/CMake；解析Kit 0.10.0及grammar/native库；最小窗口和无GUI worker均能构建；锁定依赖并记录真实API |
| P01 | P00 | 编辑器适配 | 验证唯一Rope/Undo、快照、增量事件、批量事务；公开需要的最小接缝；一个多处修改一次undo/redo，无重复事件 |
| P02 | P01 | 原样换行与坐标 | CRLF/LF/CR/混合换行端到端；修补Kit的LF假设；行号、定位、Enter、粘贴、删除、undo、搜索range与保存一致 |
| P03 | P01、P02 | 输入与列编辑 | 微软拼音、emoji、Tab、宽字符、短行、鼠标列选、多光标；确定IME进入组合态时的行为并通过真机验收 |
| P04 | P00 | 编码与保存验证 | encoding_rs/chardetng版本、GB18030边界、UTF16写入、无损API、Windows提交失败分类；原样字节往返和磁盘满保留原文件 |
| P05 | P00 | 正则与进程 | regex与pcre2编译；双worker协议、超时kill/回收、超内存、stdout污染、零宽与捕获组替换；验证取消确实停止CPU工作 |
| P06 | P00、P04 | 大日志读取 | 1 GB首屏、分块解码、CRLF边界、UTF16、10 MB单行；不全量加载；观察索引和缓存增长 |
| P07 | P01—P06 | 容量与决策汇总 | 100 MB编辑、全文替换、一次undo/redo、保存再打开；峰值内存和延迟；冻结功能差异、补丁清单、性能门槛及后续工作量 |

P01/P02/P03由同一编辑适配负责人串行维护；P04/P05/P06可并行验证。P07不能仅凭“窗口能打开”通过。

## 正式实现任务

| ID | 先决条件 | 模块或文件范围 | 必须交付的行为与验收 |
| --- | --- | --- | --- |
| C01 | P07 | contracts、workspace根 | 固化ID/offset/revision/error/path和worker协议；crate依赖无环；序列化往返、非法range、协议版本测试 |
| C02 | C01 | app/jobs、platform | 有界IO队列、CPU池、取消与generation、后台结果桥接；UI无阻塞send；关闭文档后不提交过期结果 |
| C03 | C01 | app/commands、settings模型 | 稳定command ID、Action桥接、快捷键上下文和命令可用性；菜单/快捷键/面板调用同一实现 |
| F01 | C01、P04 | text-codec | 编码检测/无替换解码、UTF16编码、BOM、EolScanner、raw与UTF8映射；所有块边界和编码fixture通过 |
| E01 | C01、P01—P03 | app/editor | 唯一编辑状态、快照、EditBatch、事务、事件去重、版本和选区接口；公开适配API契约测试 |
| F02 | F01、C02 | file-io、platform文件接口 | 文件身份/打开/可靠保存/备份/监视/冲突；保存期间输入、失败注入、外部变化与链接策略测试 |
| D01 | E01、F02 | app/documents | 打开/另存/全部保存、重复文件检测、dirty/savepoint、编码元数据、关闭提示；undo回已保存内容与竞态保存测试 |
| R01 | F01、C02 | storage | SQLite schema、单writer、source chunks、checkpoint/delta、校验、迁移；强杀恢复与损坏尾部截断验证 |
| R02 | D01、R01 | app/session、app/settings | 未命名与未保存恢复、会话/最近文件、TOML设置、profile与便携模式；恢复内容不覆盖原文档 |
| U01 | C03、D01 | app/workspace、assets | 窗口骨架、标签、状态栏、明暗主题、图标、焦点与i18n；真实文件状态显示正确 |
| E02 | E01、C03 | app/editor/commands | 行操作、缩进、大小写、排序去重、注释、书签、跳转；选择范围和一次undo边界正确 |
| E03 | E01、E02 | app/editor/column | 多光标、矩形选区、列插入删除/编号、短行与Tab策略；视觉列、中文和IME行为匹配已定规则 |
| S01 | C01、C02、P05 | search、search-worker | 字面量与标准regex、快照spool、worker协议/监督、范围与零宽规则；可取消且有界 |
| S02 | S01 | search-worker、platform进程 | PCRE2显式高级模式、捕获组与资源限制；不会暗中fallback；灾难性回溯可终止并回收 |
| S03 | S01、E01、U01 | app/search | 查找栏、选区/当前/全部打开文件、可见匹配高亮和结果定位；结果绑定revision，过期不能错位替换 |
| S04 | S02、S03、F02 | search/workspace、app/results | 统一ignore/globset过滤、目录遍历、结果分页/截断、替换预览与版本复查；打开缓冲不自动保存，部分失败有报告 |
| U02 | U01、S04 | app/file_tree、quick_open | 文件树按需加载、共用过滤库和规则模型但显示/搜索策略分别配置、nucleo匹配、MRU、取消；十万路径不逐帧重新扫描 |
| L01 | C01、C02、F01、P06 | large-file/reader、index | ReadAt封装、稀疏行索引、codec边界、缓存、generation；1 GB首屏不等全文索引 |
| L02 | L01、U01 | app/log_view、large-file/window | 虚拟化日志视口、横向长行窗口、行/字节跳转、有限选择与复制；无超长行整行无限分配 |
| L03 | L01、S01、S02 | large-file/search、search | 字面量与行级正则、范围限制、raw/UTF8映射、取消和长行限制；不会静默漏掉跨块字面量 |
| L04 | L02、L03、F02 | large-file/tail、app/log_controls | 追加/暂停/恢复、轮转/截断/替换、尾部半字符、跟随状态；用户向上滚动暂停自动跟随 |
| G01 | E01、U01 | app/languages、assets/grammars | 统一Tree-sitter注册、首批语言、高亮折叠、括号/缩进、单词补全；未知grammar有明确纯文本降级 |
| U03 | U01、G01 | app/dock、layout | 不同文件分屏、面板布局持久化、焦点恢复；同文件独立双视图只在P0验证通过后增加 |
| W01 | C01、R02、U01 | platform/instance、app/cli | clap、用户/profile单实例mutex与pipe、原生路径、--new-window；并发启动与主实例崩溃测试 |
| Q01 | 相关任务完成 | tests、xtask、CI | 编码/IO/编辑/恢复/worker/日志验收集、固定seed样本、release基准；输出真实指标而非估计 |
| W02 | W01、Q01 | packaging/windows、build资源 | 图标/manifest、便携ZIP、NSIS、worker与grammar打包、文件关联/卸载；全新用户环境验证 |

以上任务覆盖首版全部功能。某项任务过大时可按目录和验收拆分，但不得跳过其错误路径、数据安全或容量要求。

## 并行分工建议

建议由一位集成人员维护contracts、manifest/lock、公共接口、补丁和最终分支。其他AI按以下领域分工：

| 角色 | 任务 | 不应私自修改 |
| --- | --- | --- |
| 编辑器与界面 | E01/E02/E03/U01/G01/U03 | codec、SQLite schema、搜索协议 |
| 文件与存储 | F01/F02/D01/R01/R02 | Kit权威buffer、regex引擎 |
| 搜索 | S01/S02/S03/S04 | 文档保存流程、其他模块的offset定义 |
| 日志 | L01/L02/L03/L04 | 编码映射实现、原始文件提交路径 |
| 集成与发布 | C01/C02/C03/U02/W01/Q01/W02 | 未经评审地更改领域语义 |

只有依赖接口具备后才并行正式实现。AI数量增加不能绕过编辑内核、编码和保存语义的验证；共享依赖文件保持单一负责人以减少冲突。

## 每个任务的交付清单

- 修改了哪些文件，解决了哪个具体用户行为。
- 使用的crate、真实版本、features和必要native依赖；是否修改Cargo.lock。
- 新增/变更的公共接口、调用方和错误语义。
- 对应验收用例、实际执行命令与退出结果；未执行的检查列出原因。
- 对100 MB编辑或1 GB日志有影响时给出release内存/耗时变化。
- 已知行为差异和限制；不得把未验证的条件功能标成完成。
- 后续任务所需接口与数据fixture；移除临时mock和重复buffer。

任务完成应同时具备实现、相关测试和集成证据。正式功能状态建议写入docs/FEATURE_STATUS.md；M0真实API/补丁与性能结论写入docs/M0_RESULTS.md。这两个文件由后续实现创建，此轮规格不填造测试通过结果。

## 后续版本

首版完成后再拆宏、插件、LSP、函数列表、文件比较、旧Notepad++配置/插件兼容、macOS/Linux桌面版任务。宏复用command ID和事务记录；不能先录原始键鼠事件再把它称为可重复执行的宏系统。
