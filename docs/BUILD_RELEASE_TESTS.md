# 构建发布与验证方案

本文件指定工具链、Windows集成、日志和测试的实施方案。当前交付物是开发规格；下列命令和配置由后续实现任务落地并验证。

## 工具链和依赖锁定

GPUI-Kit 基线采用 =0.10.0，发布tag对应提交 f697c3133d55c64fa8a5b67926147fb3dadd276d。精确依赖图由M0生成并提交 Cargo.lock；需要补丁时保留同一框架家族的版本和可复现源码。

使用 stable MSVC Rust、VS2022 C++ Build Tools、Windows SDK、CMake和Git。Rust版本必须满足实际锁定依赖图；文档中框架的最低要求不等于本应用已经验证的MSRV。[GPUI-Kit安装说明](https://gpui-kit.com/docs/installation/)

M0记录 rustc -Vv、cargo -V、rustup show active-toolchain、cmake --version、SDK版本和Windows build。将验证成功的Rust版本写入 rust-toolchain.toml。首发以Windows x64为目标；其他target须经过独立编译与真机验证。

版本策略：

1. workspace统一声明依赖和features，各crate按需引用；GUI、SQLite和PCRE2等native依赖不进入contracts。
2. GPUI facade固定 =0.10.0；Rope与Tree-sitter依赖与Kit严格对齐，禁止自行引入不兼容的同名类型。
3. 其他库使用规格给出的API主版本族，在M0解析具体版本并锁定；不用“*”、未固定的git main或每个AI自行cargo update。
4. 分别执行 cargo tree -d 和 cargo tree -e features，重点核对GPUI家族、ropey、Tree-sitter和native库。重复版本逐项解释，不简单要求整个图没有任何重复。
5. 升级依赖是独立任务，必须重新运行受影响的编码、编辑、搜索和性能验收。

[Cargo依赖解析与锁文件](https://doc.rust-lang.org/cargo/reference/resolver.html)说明了此处采用的锁定机制。

## 构建配置

app和search-worker分别构建。worker依赖图必须不含GPUI，便于测试硬取消并缩短迭代周期。所有性能数据来自release，不用debug模式结果判断100 MB和1 GB容量能力。

根manifest设置开发依赖优化，优先按已解析的GPUI和文本渲染包做package override；确认实际包名后再写配置。release默认 opt-level=3、lto="thin"、codegen-units=1、保留必要行号调试信息；记录构建时间和体积后再决定strip。不要为缩小包体直接启用可能降低编辑性能的opt-level="z"。[Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)

图标、版本资源、应用manifest用 embed-resource 3.x 编译。DPI manifest和Kit平台行为一起验证，覆盖100%/125%/150%/200%以及跨显示器移动，不重复叠加缩放。GUI发布版使用Windows GUI子系统；worker保留标准输入输出且由父进程隐藏启动。[embed-resource](https://docs.rs/embed-resource/latest/embed_resource/)

## Windows平台集成

库默认使用 windows 0.62.x，仅在cfg(windows)依赖，features按实际API精简。初始涉及 Foundation、Storage/FileSystem、System/Threading、System/Pipes、System/JobObjects、Security、System/Registry、UI/Shell；具体feature拼写以0.62.x生成文档为准。[Rust for Windows](https://github.com/microsoft/windows-rs)

| 功能 | 方案 | 验收 |
| --- | --- | --- |
| CLI | clap 4.x derive，参数PathBuf，支持 files、--line、--column、--encoding、--new-window、--reuse-window | --help与错误参数、空格/中文/非Unicode路径、行列越界 |
| 单实例 | 当前用户SID+profile路径摘要区分实例；Windows命名mutex竞争主实例 | 同时启动两进程不丢请求；便携版和不同profile互不误转发 |
| 打开请求转发 | 当前用户可访问的命名pipe；长度前缀JSON，最多1 MiB；传NativePath及位置 | 主窗口启动中排队、旧pipe残留、主进程崩溃重启 |
| 新窗口 | 发给同一主进程创建额外窗口，共用文档服务和存储owner | 不产生第二个恢复数据库写入者 |
| 文件关联 | 安装时注册应用ProgID/OpenWith入口；通过系统默认应用界面由用户选择 | 正确引用可执行路径和文件参数；卸载仅移除本应用项 |
| 进程监督 | worker启动时不显示控制台，加入Job Object，超时/关闭应用后回收 | 孤儿进程、标准输出阻塞、无效协议、超内存、超时 |
| 路径和身份 | 原生UTF-16路径、句柄file ID、长路径按manifest/API能力测试 | 同文件不同路径、大小写、重解析点、替换后身份更新 |

[clap](https://docs.rs/clap/latest/clap/)、[Windows mutex](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createmutexexw)、[命名pipe访问控制](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)、[Windows文件类型注册](https://learn.microsoft.com/en-us/windows/win32/shell/fa-file-types)分别支持上述库/API选型。

GUI不要在窗口启动回调中同步等待pipe客户端或worker；监听和协议解析在后台。单实例IPC只传受支持的打开请求，不执行任意shell字符串。

## 错误与诊断

领域模块使用 thiserror 2.x 定义可匹配错误，app/xtask边界用 anyhow 1.x补充上下文。Cancelled、Conflict和ResourceLimit是可预期操作结果，不能全部显示为“未知错误”。[thiserror](https://docs.rs/thiserror/latest/thiserror/)

采用 tracing 0.1.x、tracing-subscriber 0.3.x 和 tracing-appender 0.2.x，异步写入滚动日志。默认记录job、阶段、耗时、文件大小、错误类别，不记录文本正文或完整搜索替换内容。非阻塞写入guard跟随应用生命周期保留。[tracing-appender](https://docs.rs/tracing-appender/latest/tracing_appender/)

性能事件包括 open/read/decode/attach、snapshot/spool、search/replace、save/commit、index/window/tail、recovery/checkpoint。日志任务本身需要限额；默认按天滚动、保留7天，并设置总量清理上限。

## 测试工具

| 用途 | 工具 | 使用边界 |
| --- | --- | --- |
| 逻辑与集成测试 | Rust内置test | 编码、编辑事务、搜索语义、可靠保存、协议与恢复 |
| 并行测试运行 | cargo-nextest | worker/SQLite测试使用独立临时profile；限制需要串行的测试组 |
| 性质测试 | proptest 1.x | 随机Unicode编辑+undo、块边界解码、EOL扫描与偏移转换 |
| 结构快照 | insta 1.x，json feature | 命令注册、设置迁移、协议样例；不靠快照掩盖错误结果 |
| 微基准 | criterion 0.8.x | 块扫描、匹配、偏移查找、解码、行索引；不替代端到端UI延迟 |
| 临时环境 | tempfile 3.x | 每个故障测试独立目录与文件；模拟失败后检查残留 |
| GPUI组件测试 | Kit对应版本的测试宏与headless能力 | 真实Action、焦点、布局与选区；先验证Windows支持路径 |
| 真机交互 | 场景脚本+人工复核 | 微软拼音、候选框、DPI、鼠标列选、辅助功能、睡眠唤醒 |
| 依赖检查 | cargo-deny | 依赖来源、重复、许可证和公告按项目策略配置 |

来源：[nextest](https://nexte.st/docs/)、[proptest](https://docs.rs/proptest/latest/proptest/)、[insta](https://docs.rs/insta/latest/insta/)、[criterion](https://docs.rs/criterion/latest/criterion/)、[cargo-deny](https://github.com/EmbarkStudios/cargo-deny)。

## 必须实现的验收集合

| 集合ID | 内容 | 失败时阻止的交付 |
| --- | --- | --- |
| GATE-EDITOR | 私有API适配、原样EOL、事务、undo、IME、100 MB和超长行 | M0进入正式编辑开发 |
| T-ENC | 各编码原样往返、无损转码、BOM、GB18030映射、坏字节 | 可靠编辑里程碑 |
| T-EDIT | 多光标/列操作、Tab/宽字符/CRLF、一次撤销、编码元数据修改状态 | 高效编辑里程碑 |
| T-IO | 磁盘满/权限/占用/外改/提交错误/保存期间输入/链接策略 | 正式使用真实文件 |
| T-RECOVERY | 强杀恢复、部分写入、校验失败、数据库迁移、恢复窗口 | 日常可用与发布 |
| T-SEARCH | 标准/高级语义、零宽、替换、取消、worker崩溃、上限、版本漂移 | 搜索里程碑 |
| T-LOG | 1 GB首屏、长行、分块编码、行索引、tail截断轮转、资源限额 | 大日志里程碑 |
| T-UI | 键盘、焦点、主题、文件树、命令面板、DPI、中文输入 | 首版发布 |
| T-PACKAGE | 全新用户安装、便携模式、worker路径、关联、卸载保留文档 | 分发包 |

损坏文件与参考编码数据作为小fixture提交；100 MB和1 GB样本由xtask用固定seed生成，记录实际byte length和hash，不把巨型文件提交进Git。每个场景记录预期原始字节/文本、操作序列、正确输出和资源限制。

故障注入使用file-io/SQLite/worker接口的可替换实现，在read、encode、flush、commit、journal等阶段明确失败；只mock到难以可靠重现的故障点，不模拟整套编辑器后宣称真机通过。

## CI和验证命令

M1建立Windows CI，每次修改运行格式、编译和受影响测试；Linux可先运行不依赖GUI的核心crate测试，不把它当成Linux桌面版已支持。

~~~text
cargo fmt --all --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
cargo deny check
cargo build -p app -p search-worker --release --locked
~~~

上面的package名称是计划中的名称，实施时与manifest同步。features按产品实际组合做矩阵，不盲目使用all-features触发未采用的实验后端或重复引擎。端到端性能在固定基准机运行，不能拿共享CI的波动替代发布指标。

## 打包和发布

首版交付便携ZIP与NSIS 3.x Unicode安装器。NSIS脚本位于packaging/windows；不为打包引入Tauri或Electron运行时。[NSIS](https://nsis.sourceforge.io/Main_Page)、[NSIS脚本参考](https://nsis.sourceforge.io/Docs/Chapter4.html)

分发目录至少包含app.exe、search-worker.exe、必要grammar/assets、第三方声明及版本信息。安装包默认当前用户安装，安装后不自动抢占默认编辑器；卸载默认保留用户配置/恢复数据并提供明确选项。

便携模式由可执行目录的portable标记启用；目录不可写时明确报错或要求选择profile位置，不能悄悄写到无法发现的位置。不同profile分别管理配置、恢复库、IPC实例名和日志。

自动更新不属于首版必做；先用版本化安装包升级并验证schema迁移与回退策略。签名、最终软件名称、图标和发布地址在分发前落定。发布记录必须包含功能差异、已验证平台、容量条件、已知限制和可复现的性能结果。
