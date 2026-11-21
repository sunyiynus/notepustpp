# 功能状态（当前实现）

日期：2026-11-21。状态只描述仓库中**已编译并测试/已真机启动**的行为，不把设计规格当作完成。

状态：`已完成`、`部分`、`核心库完成（UI 未接入）`、`未接入`。

| 首版能力 | 状态 | 已实现与证据 | 主要缺口 |
| --- | --- | --- | --- |
| 窗口与标签 | 部分 | GPUI-Kit 桌面窗口、标签栏、活动标签、关闭/切换、标题脏标记；欢迎页展示最近文件；`tests/fixtures/smoke.ps1` 验证窗口标题与响应 | 标签固定/排序、拖入文件 |
| 打开与保存 | 部分 | 异步打开、保存、另存为、全部保存、从磁盘重载（脏文档需二次确认）；Windows `ReplaceFileW`；同目录 temp；冲突检测；保存期间输入保留脏状态；另存为到另一已打开文档的路径会被拒绝 | 保存失败后的另存引导 UI、写满/权限故障注入的 GUI 验收 |
| 基础编辑 | 部分 | Kit 原生输入、撤销/重做、剪贴板、自动缩进、Tab、软换行、行号、多光标基础行为；跳转行对话框（Ctrl+G）；空白字符显示开关（Ctrl+Alt+W，持久化）；Alt+点击/Alt+Shift 拖动列选区 | 中文 IME 真机验收、列操作命令化 |
| 换行保真 | 部分 | 打开/保存保留真实 CRLF/LF/CR；vendored gpui-base 让 Enter 使用文档主导换行；GPUI 真实输入测试覆盖 CRLF/LF Enter 与 undo，含中文/emoji | CR-only 文件的行号/上下移动仍依赖 Kit 的 LF 索引；换行转换命令 UI |
| 编码 | 部分 | UTF-8/BOM、UTF-16 LE/BE、GBK/GB18030 严格解码；chardetng 候选；无替换错误；UTF-16 手工写入；不可编码字符不落盘；状态栏编码面板提供“重新解释为”和“保存编码”两类操作（重解释要求文档干净） | BOM 不匹配的引导文案、编码选择冲突对话框、GB18030 标准样本外部核验 |
| 另存字节保真 | 部分 | 未修改文档另存为使用保留的原始 source bytes；保存后更新 source baseline | 非规范映射的专门 fixture 集 |
| 查找替换（当前文件） | 部分 | 字面量/常规 regex/PCRE2 高级 regex、大小写、整词、全部替换、撤销、命中装饰、F3 循环；零宽命中终止；只读日志内字面量/行级 regex 搜索、命中行高亮与 F3 循环 | 选区范围、全部打开文件、捕获组替换预览/回滚、日志内替换（只读设计） |
| 文件夹搜索 | 部分 | 统一 ignore/globset 策略；隐藏文件、`.gitignore`、默认排除、身份去重；编码/大小/二进制跳过原因；结果面板点击定位 | 文件夹替换、结果分页/虚拟列表、逐文件版本复查、搜索取消按钮 UI |
| 搜索结果模型 | 部分 | 500 条/512 KiB 批次上限、10,000 条/32 MiB 截断模型；UI 只保留首批结果 | 分页、结果来源版本校验的完整交互、跨文件归并 |
| 正则 worker | 已完成（库/UI 查找替换） | 独立 `search-worker.exe`；Hello/Ready/帧上限；标准 regex；PCRE2 前后查找；替换 artifact + BLAKE3；取消/超时 kill+wait；进程级测试通过；查找条提供 `PCRE2` 开关，查找与全部替换均可走 worker（快照 + 校验 artifact），应用测试覆盖前后查找与 `$0` 替换 | 跨行高级搜索的 UI 选项、PCRE2 原生 match limit 未设置（依赖进程隔离/超时） |
| 大日志 | 部分 | `large-file` 核心库 + 只读 UI：超过 100 MiB 或单行超过 2 MiB 时自动进入日志模式；行号、顶部/上一屏/下一屏、跟随暂停、刷新、后台稀疏索引进度；流式日志搜索（字面量/行级 regex，1 MiB 块 + 跨块行，UTF-16/GBK/GB18030）与命中行高亮；1 GB 首屏 371–813 ms / WorkingSet 79–83 MB（`log-smoke.ps1`）；追加/截断/替换事件驱动刷新 | 日志内替换（只读设计）、未索引远距离命中的即时跳转（需等待后台索引）、横向滚动与超长行导航、GUI 级截断/轮转验收 |
| 编辑事务 | 部分 | Kit 原生编辑与撤销；应用层用 `replace_all` 执行全文替换；revision 递增；过期搜索结果丢弃 | 公开批量 `EditBatch` 上游接缝（`replace_text_in_ranges` 仍为 `pub(crate)`），一次事务多光标程序化编辑 |
| 修改状态 | 部分 | InputEditsEvent 标脏；保存后仅保存快照修订变干净；≤2 MB 文档撤销回保存文本后由后台比较清除 dirty | >2 MB 文档撤销回保存点仍保持 dirty（保守）；Kit history marker 未公开 |
| 外部修改检测 | 部分 | 激活/定时检查身份、大小、mtime；保存前内容摘要复查 | notify 事件、同大小同时间重写的深检查 UI、重载对话框 |
| 会话与恢复 | 部分 | SQLite WAL/FULL、检查点 BLAKE3 校验、恢复副本、会话路径、最近文件、退出最终检查点 | 恢复窗口提示、恢复内容 diff、磁盘配额回收的完整策略 |
| 设置 | 部分 | TOML schema、版本校验、范围规范化、原子保存；明/暗主题、中英标签切换；字体面板（`Ctrl+,`/工具栏）：界面字体、编辑字体、各自备用字体链与字号，即时生效并持久化 | 缩进/快捷键/搜索排除项的完整设置面板、配置迁移 UI |
| 工作区 | 部分 | 标签、状态栏、查找条、文件夹结果面板、欢迎页（最近文件）、快速打开（Ctrl+P）；分屏：两个窗格显示不同标签，点击窗格切换当前文档，工具栏开关 | 文件树、命令面板、布局持久化 |
| 行操作/列编辑/书签 | 部分 | 复制/删除/上下移动/排序/去尾空格/注释/缩进行，均通过一次 `replace_all` 事务撤销；`Ctrl+Alt+↑/↓` 添加上下多光标；Kit 原生 Alt+点击与 Alt+Shift 拖动列选区；纯函数测试覆盖 CRLF/多行/中文列移动 | 去重行、大小写转换、列插入删除/编号、书签、跳转行持久列表 |
| 语法高亮/折叠/补全 | 部分 | Tree-sitter 增量高亮接入 GPUI-Kit `InputHighlighter` 接缝；首批 Rust/C/C++/Python/JS/TS/TSX/JSON/YAML/TOML/Markdown/HTML/CSS/SQL/Shell；语法树折叠；>2 MiB 文档关闭解析；主题切换保留解析树 | 括号匹配/自动闭合开关 UI、单词补全、INI/XML 专用 grammar、大文件高亮性能基准 |
| Windows 集成 | 部分 | clap CLI（文件、`--line`、`--column`、`--register-file-types`/`--unregister-file-types`）、原生文件选择器、DPI 由 GPUI 处理；单实例 mutex + 命名管道转发，`--new-window` 真正新建独立窗口；HKCU `Software\Classes` 文件关联（不抢默认打开方式）；NSIS 安装脚本 `installer/notepust.nsi`；`single-instance-smoke.ps1` 验证双进程转发 | 便携 ZIP、代码签名、图标/版本资源、安装包实机验收 |
| 性能验收 | 部分 | release 单次样本：10.8 MB/175K 行日志打开到标题 438 ms；100 MB 文本 865–891 ms；4 MiB/50K 行后降级折叠与软换行；10 MB 单行自动进入日志视图，492 ms / 79 MB；1 GB 日志 371–813 ms / 79–83 MB / Responding=True；语法高亮仅对 ≤2 MiB 文档解析 | 输入 P95、撤销、保存、峰值内存、语法高亮大文件基准仍待固定基准机测量 |

## 自动化测试

`cargo test --workspace` 当前 155 个测试通过，覆盖：

- contracts：ID/偏移/revision/协议帧/最大帧拒绝。
- text-codec：BOM、UTF-16、GB18030 四字节、不可编码、EOL 混合、grapheme/显示列、流式跨块解码。
- file-io：原始字节往返、冲突拒绝、备份、编码失败不写目标、外部修改。
- storage：SQLite 检查点/会话/最近文件/保存日志/设置迁移。
- search：字面量/regex/零宽/整词/大小写、替换语法、预览上限、批次上限、文件夹过滤与跳过原因、worker 监督。
- platform：文件身份与时间戳；Windows 单实例 mutex + 命名管道的真实双进程转发。
- large-file：CRLF 跨块、稀疏索引、UTF-16 视口、长行截断、tail、append/truncate；流式搜索跨 1 MiB 块、UTF-16/GBK、行级 regex、取消与结果截断。
- search-worker：真实子进程 Hello、标准/PCRE2 查找、捕获组替换 artifact、非法模式错误。
- app：GPUI headless 真实键盘输入（CRLF/LF Enter、undo、中文/emoji）；打开路由、PCRE2 worker、快速打开过滤/目录跳过；行操作（多行 CRLF、排序、注释、缩进、中文显示列多光标）；语法扩展名映射与字体回退链。

真机交互（微软拼音、DPI 缩放、NVDA、鼠标列选、休眠恢复）尚未验收，不能标记完成。
