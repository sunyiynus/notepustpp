# Notepad++ 重写开发规格总入口

日期：2026-11-21。用途：把首版功能、默认库、实现方案、模块契约和任务编号交给其他AI或开发者执行。产品范围见 [PLAN.md](../PLAN.md)。

这套文件是实施规格，尚未建立或编译应用。版本号来自官方文档与源码核对；M0必须解析真实依赖图、完成Windows原型和容量验证，再提交Cargo.lock。文中的项目接口是待实现契约，不能当作上游已有API。

## 文档索引

| 文档 | 内容 | 交给谁 |
| --- | --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | crate结构、唯一文本所有权、ID/坐标/修订、事务、快照、并发和worker协议 | 所有开发AI必读 |
| [EDITOR_UI_SPEC.md](EDITOR_UI_SPEC.md) | Kit真实接缝与补丁、列编辑/行操作、语言、工作区、主题、键盘和焦点 | 编辑器、界面开发 |
| [FILE_IO_RECOVERY_SPEC.md](FILE_IO_RECOVERY_SPEC.md) | 编码、字节保真、打开保存、冲突、SQLite恢复、会话和设置 | 文件、编码、存储开发 |
| [SEARCH_LARGEFILE_SPEC.md](SEARCH_LARGEFILE_SPEC.md) | 搜索/替换语义、正则进程、文件夹过滤、大日志索引、长行和tail | 搜索、日志开发 |
| [BUILD_RELEASE_TESTS.md](BUILD_RELEASE_TESTS.md) | 工具链、依赖锁、Windows集成、日志、测试、CI和安装包 | 工程、集成、发布开发 |
| [AI_HANDOFF.md](AI_HANDOFF.md) | 可复制提示词、M0验证任务、正式任务、依赖和交付协议 | 任务分派与集成负责人 |

## 功能到库和方案的总表

表中列出默认方案，不让不同AI重新自由选型。库的精确features、已核实API与限制见领域规格；任务编号见AI_HANDOFF。

| 功能 | 默认库或组件 | 本项目需要实现的方案 | 任务 |
| --- | --- | --- | --- |
| 窗口与UI运行时 | gpui-kit =0.10.0 | 使用facade统一初始化、Root与资源，保留同一GPUI依赖图 | P00/U01 |
| 常规编辑内核 | Kit Editor / EditorState | 适配文档生命周期、命令与增量事件；公开最小事务补丁 | P01/E01 |
| 文本缓冲 | Kit使用的ropey =2.0.0-beta.1 | 唯一权威Rope；按块读取、廉价快照；不另建ropey 1.x buffer | E01 |
| 撤销/重做 | Kit UndoManager | 批量事务、savepoint、选区恢复与事件去重；不另建文本撤销栈 | P01/E01 |
| 多光标与列编辑 | Kit selections/DisplayMap + 应用列意图 | 像素命中、Tab/短行/虚拟列、批量插入删除与编号 | P03/E03 |
| 中文/emoji导航 | unicode-segmentation 1.x + GPUI布局 | grapheme边界；UTF8/UTF16/显示列分别转换 | P03/E01/E03 |
| 列宽估算 | unicode-width 0.2.x | 仅作估算；鼠标命中以Kit实际字形布局为准 | E03/L02 |
| 行操作/排序/去重 | std + 编辑事务适配 | 选区扩展、稳定排序、保留EOL槽位、一次undo | E02 |
| 注释与缩进 | Kit LanguageConfig + 应用语言目录 | 按语言规则处理、统一事务、明确不支持的上下文 | E02/G01 |
| 书签/跳转/行号 | Kit gutter decorations + 项目锚点 | 编辑后位置变换、undo一致性与行列边界 | E02 |
| 语法高亮/折叠 | Kit Tree-sitter，runtime与Kit对齐 | 明确启用grammar、queries、增量处理、容量降级 | G01 |
| XML/INI | tree-sitter-xml 0.7.0 / tree-sitter-ini 1.4.0 | LanguageRegistry注册；验证ABI、queries和折叠规则 | G01 |
| 括号与单词补全 | Kit编辑规则/补全接缝 | 语言配置、局部/后台词索引；首版不依赖LSP server | G01 |
| 标签/分屏/面板 | Kit Dock / Panel | 文档与视图区分、关闭流程、布局版本化 | U01/U03 |
| 文件树 | Kit Tree + ignore/globset | 惰性目录读取、稳定节点、可配置显示策略、错误反馈 | U02 |
| 命令面板 | Kit Command + GPUI Action/KeyBinding | 稳定command ID、统一分发、上下文可用性 | C03/U01 |
| 快速打开/模糊评分 | nucleo-matcher 0.3.1 | 既有后台池中top-K、generation取消、MRU加权 | U02 |
| 搜索结果列表 | Kit VirtualList / uniform list | 有界批次、分页、定位、截断标记 | S03/S04 |
| 明暗主题/字体/密度 | Kit Theme / ThemeRegistry | 语义token、用户设置、编辑状态不重建 | U01/R02 |
| 图标 | Kit assets内Lucide SVG | 统一图标映射、标签和tooltip | U01 |
| 中英文界面 | rust-i18n 4.2.x | zh-CN/en资源与动态刷新，缺词回退 | U01/R02 |
| 剪贴板 | GPUI/Kit原生接口 | 标准复制粘贴和列粘贴策略，目标版本检查 | E01/E03 |
| 文件选择器 | GPUI原生prompt入口 | 异步取消和打开生命周期；编码选择由应用提供 | D01 |
| UTF8/中文编码解码 | encoding_rs 0.8.x | 无替换增量API、映射profile、失败只读与显式转码 | F01 |
| 编码候选 | chardetng 1.x | BOM/显式选择优先，候选预览；不编造置信度 | F01 |
| UTF16 LE/BE写入 | std::str::encode_utf16 | 明确端序、BOM和代理项；不用encoding_rs的UTF16 encoder | F01 |
| 换行保真 | 自有EolScanner + Kit兼容补丁 | CRLF优先、再CR/LF，权威文本保留原样换行 | P02/F01/E01 |
| 原始字节保真 | 原始source chunks + codec校验 | 无修改另存字节一致；非规范映射的转换策略明确 | F01/R01 |
| 可靠保存 | tempfile 3.x + windows 0.62.x | 同目录临时文件、同步、身份复查、Windows提交状态机 | F02/D01 |
| 文件身份与路径 | windows句柄API + NativePath | volume/file ID与原生UTF16；不把字符串路径当强身份 | F02/W01 |
| 外部修改与日志事件 | notify 9.x + 轮询复查 | 父目录watch、去抖、identity/stamp复核 | F02/L04 |
| 恢复与会话 | rusqlite 0.43.x，bundled/backup | 单writer、WAL/FULL、checkpoint+delta+校验、迁移 | R01/R02 |
| 设置和布局 | serde 1.x + serde_json 1.x + toml 1.x | TOML用户设置、JSON结构数据、schema版本和安全写入 | R02/U03 |
| profile目录 | directories 6.x | config/data/cache区分；显式便携profile | R02/W01 |
| 指纹与完整性 | blake3 1.x | 原始字节、保存输出、快照与恢复块摘要；后台增量计算 | F02/R01 |
| 单字面量查找 | memchr 2.x / memmem | Rope/日志块扫描、跨块边界、取消和版本检查 | S01/L03 |
| 多关键词高亮 | aho-corasick 1.x | 有限模式集、可见区高亮；ASCII folding不冒充Unicode | S01/S03 |
| 常规正则 | regex 1.x | 默认正则引擎；所有正则请求走worker | S01 |
| 高级正则 | pcre2 0.2.x | 用户显式选择；前后查找/反向引用、硬取消 | S02 |
| 正则运行隔离 | std::process + Windows Job Objects | 2持久worker槽、超时kill/wait、内存限额、版本化协议 | P05/S01/S02 |
| 文件夹搜索与过滤 | ignore 0.4.x + globset 0.4.x | 统一规则模型、搜索/显示策略分别配置、错误及跳过报告 | S04/U02 |
| 替换与批量替换 | 项目ReplacementArtifact/EditBatch | 完整扫描生成受限临时结果、版本复查、单文件提交/undo | S03/S04 |
| 大日志随机读 | std FileExt + ReadAt适配 | 短读循环、独立句柄、codec安全边界；默认不用整文件mmap | L01 |
| 日志行索引 | 自有稀疏索引 | 后台扫描、字节/逻辑行锚点、未完成索引状态 | L01 |
| 日志视口与缓存 | Kit/GPUI布局 + 有字节预算的LRU | 可见窗口解码、横向长行分段布局、有限选区复制 | L02 |
| 日志搜索 | memmem + 常规regex受限流式方案 | 跨块字面量；行级正则；超限和未扫描范围明确报告 | L03 |
| 日志尾随 | reader状态机 + notify/定时probe | append/truncate/rotate/replace、暂停跟随、半字符处理 | L04 |
| 后台调度 | std线程 + rayon 1.x + async-channel 2.x | 有界IO、独立CPU池、版本化回调、取消 | C02 |
| ID与错误 | uuid 1.x(v4/serde) + thiserror 2.x | 文档/视图/任务身份和结构化领域错误 | C01 |
| 应用边界错误 | anyhow 1.x | app/xtask诊断上下文，不替代领域错误类型 | C01/W02 |
| 日志诊断 | tracing 0.1 / subscriber 0.3 / appender 0.2 | 非阻塞滚动日志、性能span与容量清理 | C02/Q01 |
| CLI与单实例 | clap 4.x + Windows mutex/pipe | 当前位置打开、当前用户/profile隔离、并发启动 | W01 |
| Windows资源 | embed-resource 3.x | 图标/版本/manifest，DPI验证 | W02 |
| 安装包 | NSIS 3.x + 便携ZIP | 打包worker与grammars、关联入口、卸载保留用户文档 | W02 |
| 测试与性能 | proptest/insta/criterion/nextest + Kit测试 | 性质/故障/协议/真机场景与固定seed容量样本 | Q01 |

本表中通用库的官方入口：[uuid](https://docs.rs/uuid/latest/uuid/)、[serde_json](https://docs.rs/serde_json/latest/serde_json/)、[toml](https://docs.rs/toml/latest/toml/)、[anyhow](https://docs.rs/anyhow/latest/anyhow/)。领域库的主来源与API限制随对应规格列出。

## 已定的架构决策

- 普通编辑只有一份权威文本和撤销历史，直接复用Kit；原样保留实际换行。
- 普通文件与大日志是两条数据路径；1 GB只读模式不是给Editor设置readonly后全文载入。
- 字面量扫描使用轻量后台作业；所有正则使用可终止的worker。高级模式不与常规模式偷偷互换。
- 普通编辑快照要覆盖100 MB原文件解码膨胀，搜索基线预算384 MiB，实际峰值在M0测量。
- 文件夹替换对打开文档只改缓冲；未打开文档才写磁盘；预览上限不等于替换执行上限。
- 所有任务结果绑定revision或generation；worker、结果队列、日志缓存、恢复磁盘均有上限。
- SQLite负责恢复/会话等结构存储；TOML负责用户设置；不把1 GB日志装入恢复数据库。

## 必须由M0验证的部分

选型已经明确，以下是实现验证门槛，不是让开发AI再随意选框架：

1. Kit公开事务、savepoint、快照和增量事件的实际接缝；必要时维护最小补丁。
2. Kit中LF行索引假设对CR-only和混合换行的修正。
3. 真实中文输入法、多光标与矩形列编辑的组合行为。
4. 100 MB完整编辑、解码膨胀、超长行以及1 GB reader的release性能。
5. Tree-sitter runtime与额外grammar ABI/queries，以及JSONC等语言的实际能力边界。
6. Windows保存错误状态、文件身份与残余路径竞态的可解释处理。
7. PCRE2资源上限、取消、worker退出和完整替换artifact回传。

同一文档独立双视图仍是条件功能；不同文件分屏属于必做。宏、插件、LSP、旧DLL插件兼容和其他平台产品化按后续版本处理。

## 使用方法

先把AI_HANDOFF的提示词连同P00—P07交给负责M0的AI。M0结束后，再按C01及各领域任务依赖分派正式实现。不要把本文所有任务作为一个没有验收节点的巨大提示词交给多个AI同时改公共文件。
