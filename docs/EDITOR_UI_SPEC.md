# 编辑器与桌面界面实施规格

日期：2026-11-21。与根目录 [PLAN.md](../PLAN.md)、[公共架构契约](ARCHITECTURE.md)、[构建发布与验证](BUILD_RELEASE_TESTS.md) 配套；供后续开发 AI 按任务实施。本文件决定编辑器、文本命令、工作区和交互方案；文件编解码、可靠保存、搜索引擎及大日志索引由对应模块规格负责。公共类型以 ARCHITECTURE 为准；编辑器实现放在 `crates/app/src/editor/`，工作区和文档管理分别放在 app 的 workspace/documents 模块。

## 1. 基线、证据与必须先完成的验证

**采用 Rust + GPUI-Kit Editor，不重新实现常规文本排版和输入引擎。** 初始依赖候选锁为 `gpui-kit = "=0.10.0"`，保留默认 `component`、`assets`，按需启用语言特性。规划阶段尚未创建工程、解析 Cargo.lock 或在 Windows 实编，因此这是已核对发布源码的候选基线，不能记作已验证可运行版本。

证据分两组，禁止混用行号或把 main 的 API 当成发布版已有能力：

- 发布版：tag `v0.10.0` 指向提交 `f697c3133d55c64fa8a5b67926147fb3dadd276d`。下文实现判断以此版本为准。[发布清单](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/kit/Cargo.toml)
- 早期调研：main 提交 `4373234ff46df3a3227972788a41dc6603d352ce`。根计划保留该次调研记录；它不是本规格的发布依赖。

M0 必须产出 `docs/adr/editor-kernel.md`，记录以下结果及真实运行命令：

| 验证门 | 必测内容 | 失败时的既定处理 |
| --- | --- | --- |
| Windows 基线 | 初始化、Root、字体、默认图标、输入、弹窗、原生文件选择器、release 构建 | 修复构建配置；有上游阻断时制作固定提交的小范围 Kit 补丁，不能悄悄切换框架 |
| 原样换行 | CRLF/LF/CR/混合换行加载、显示、Enter、粘贴、删除行界、撤销/重做、语法位置 | 保留实际 EOL 于权威 Rope；修补 Kit 的 LF-only 输入/显示假设，不自动改为 LF + 独立文本副本 |
| 编辑事务 | 应用侧多处替换、单次 undo/redo、选区恢复、savepoint、IME | 向 Kit 暴露受控批量编辑与历史接缝；不逐个修改再拼接应用撤销栈 |
| 列编辑 | Tab、短行、中文、emoji、软换行、虚拟空格插入 | 按第 5 节补充列意图与命中转换，不只交付上游鼠标框选演示 |
| 性能 | 100 MB、1/10 MB 单行、局部编辑/撤销、搜索装饰、切换标签 | 限制高亮/软换行成本、去掉全文复制；仍失败则在 M0 更新内核决策，不延迟到发布前 |
| 共享文档 | 同一文档两视图的独立光标/滚动与共同撤销 | 首版先保证不同文件分屏；同文档双视图仅在通过后启用 |

M0 的结论必须区分“源码可见”“已编译”“已真机验证”；任何依赖升级都重跑这些门槛。

## 2. 选型表

| 功能 | 决定使用的库/组件 | 应用需要实现的部分 |
| --- | --- | --- |
| 常规文本内核 | `gpui-kit =0.10.0` 的 `Editor` / `EditorState` | 文档生命周期、适配层、事务补丁、换行兼容、能力限制 |
| 文本存储 | Kit 自身 `ropey =2.0.0-beta.1` | 复用同一个 Rope；如业务模块直接引用 ropey，必须与 Kit 完全同版本/特性，不能引入 1.x 的另一种类型 |
| 撤销与重做 | Kit 内部 UndoManager | 暴露批量事务、历史标记、选区快照；不再建立文本撤销栈 |
| grapheme 边界 | `unicode-segmentation` 1.x，与锁文件统一 | Rope 跨块 grapheme 扫描；不把一个 Rust char 当成一个可见字符 |
| 列宽与命中 | Kit DisplayMap、GPUI 文本布局、Tab stops | 鼠标 x 与字节偏移转换、虚拟列、短行填充；`unicode-width` 0.2.x 只供估算，不能代替像素排版 |
| 高亮与折叠 | Kit Tree-sitter；运行时统一为 Kit 使用的 `tree-sitter` 0.26.13 候选版本 | 语言目录、queries、超时/容量降级、扩展名与 shebang 识别 |
| XML / INI | `tree-sitter-xml =0.7.0`、`tree-sitter-ini =1.4.0`，通过 `LanguageRegistry` 注册 | 检查 language ABI、queries 与 runtime 兼容；维护折叠规则，不假定 grammar 自带全部能力 |
| 搜索命中可视化 | Kit 的 Text/Range/OverviewRuler decorations collections | 由独立 search 服务供给有版本的范围；只保留必要命中，定位和替换不走 Kit 字面量 matcher |
| 主题 | Kit Theme / ThemeRegistry / ActiveTheme | 语义 token、紧凑密度、状态颜色与用户设置 |
| 标签与面板 | Kit Dock / DockArea / Panel | 文档打开关闭、修改提示、分屏命令、布局版本与恢复 |
| 文件树 | Kit Tree | 后台惰性读目录、稳定节点 ID、排序、错误状态、禁止递归跟随循环链接 |
| 命令面板 | Kit Command + GPUI Action / KeyBinding | 稳定 command ID、可执行判断、参数、统一分发 |
| 快速打开/命令模糊评分 | Kit Input/Command + `nucleo-matcher` 0.3.1 | 在既有后台 CPU 池计算、有界 top-K、取消、去重与 MRU 加权，不在 UI loop 匹配全目录 |
| 搜索结果列表 | Kit VirtualList；等高项优先使用 GPUI uniform list | 分页、批次合并、总数/截断状态、版本校验 |
| 图标 | Kit 默认 assets 中 Lucide SVG | 稳定图标映射、tooltip 和可访问名称，不混入 emoji 图标 |
| 国际化 | `rust-i18n` 4.2.x | `zh-CN` / `en` 应用文案、组件翻译扩展、切换后窗口刷新 |
| 剪贴板 | GPUI / Kit 原生 Copy/Cut/Paste 与 ClipboardItem | 列粘贴策略、异步粘贴目标检查，不另引 arboard |
| 文件选择器 | GPUI `prompt_for_paths` / `prompt_for_new_path` 原生入口 | 解码选择由应用菜单/对话框提供，取消与后台加载生命周期，不默认引入 rfd |
| 布局序列化 | 与全局方案共用 `serde` / `serde_json` | schema version、迁移、错误回退，不序列化 GPUI Entity 或控件内部历史；用户设置采用全局规格的 TOML，不另建一套 JSON 设置 |

XML/INI 的具体包版本已在官方 Rust API 页核对，但尚未与本工程编译验证。决定统一 Tree-sitter，不启用“Kit 同时自动为部分文件选 syntect、部分选 Tree-sitter”的假设。Kit 的两个 feature 同开时优先 Tree-sitter，并不是自动 fallback。

主来源：[Kit workspace 依赖](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/Cargo.toml)、[XML grammar](https://docs.rs/tree-sitter-xml/0.7.0/tree_sitter_xml/)、[INI grammar](https://docs.rs/tree-sitter-ini/1.4.0/tree_sitter_ini/)、[Unicode grapheme API](https://docs.rs/unicode-segmentation/latest/unicode_segmentation/)、[Unicode width API](https://docs.rs/unicode-width/latest/unicode_width/)。

## 3. 文本、文档与视图所有权

### 3.1 唯一权威文本

一个普通编辑文档由一个 Kit EditorState 持有权威文本和撤销历史。`documents` 管理 `DocumentId`、路径、磁盘版本、编码、默认插入 EOL、保存状态、会话标识；它引用编辑适配层，不复制一份可写 String/Rope。

app/editor 中的 `editor_adapter` 为 UI 以外的模块提供以下语义接口。**这些是本项目拟定契约，不是声称已存在的上游 Rust 函数签名。** 真正导出类型及签名在 M0 编译后固定。跨模块使用 `DocumentId`、`DocRevision`、`ViewId`、`SnapshotHandle`、`NativePath`，不要再创建同义的 Revision/TextBuffer/PathString 类型。

| 契约 | 输入 | 输出与约束 |
| --- | --- | --- |
| 读取快照 | DocumentId | `SnapshotHandle`：含 DocumentId、DocRevision、SnapshotId、EncodingSpec、utf8Length，支持分块读取；Rope 仅为不暴露给调用者的后端实现，history marker/EOL信息由适配器关联 |
| 应用编辑事务 | 公共 `EditBatch`：DocumentId、expected_revision: DocRevision、TransactionId、非重叠 Utf8Range 编辑、选区后态、command_id | 成功时一个撤销单元及一个提交结果；DocRevision 不同拒绝；全部范围预验证后才修改 |
| 获取/设置视图选区 | ViewId、带方向的范围数组、active selection、可选列意图 | UTF-8 字节坐标；同时保留 anchor/head，禁止只保存排序后的 Range 丢失方向 |
| 导航与显示 | ViewId、文档位置、scroll policy | 展开必要折叠并将位置滚入视口；不改变文本或撤销历史 |
| undo/redo | DocumentId、发起 ViewId | 复用 Kit 历史，恢复对应选区；文本修订递增，history_marker 回到既有状态 |
| 更新装饰 | DocumentId、source_revision、owner、命中范围批次 | 过期批次丢弃；每个 owner 独立 collection；关闭/重搜时显式 clear/dispose |
| 查询能力 | DocumentId | editable、column_edit、highlight、folding、soft_wrap 等真实可用状态及降级原因 |

禁止后台线程持有 GPUI Context、Window、EditorState 可变引用或 Entity 的 UI 操作权限。后台结果回 UI 时同时检查文档还存在、请求 generation 和 revision。

### 3.2 已核实上游接缝和限制

v0.10.0 发布源码中的实际接缝：`text()` 借用 Rope；`value()` 每次生成全文字符串；`InputEditsEvent` 携带有顺序的 edits 和每一步后的 Rope；`set_selected_range_with_direction`、`add_selection`、decorations collection 可用于导航与标注。`set_highlighter_factory` / `InputHighlighter` 是公开接缝，但本项目主线直接复用 Tree-sitter。

`set_value` 会静默替换、清历史、重置选区/滚动；仅用于首次加载或用户明确接受的重新加载。`replace_all` 可撤销，但整个内容替换适合整文格式化等真正操作，不能用作每次局部操作的通用实现。内部 `replace_text_in_ranges` 是 `pub(crate)`，应用不能直接调用。

因此 M0 的 Kit 最小补丁应明确公开本项目所需的批量编辑门面，复用现有批量逻辑，包含 UTF-8 范围验证、原子撤销、选区快照、一次高亮批处理、对应 InputEditsEvent。仅公开一个无校验的内部函数不算完成。补丁进入独立依赖目录或固定 Git revision，并记录与 v0.10.0 的差异；正式工程不能继续声明纯 crates.io `=0.10.0` 却依赖未发布 API。

源码：[状态与事件](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/base/src/input/base/state.rs)、[UndoManager](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/base/src/input/base/undo_manager.rs)、[高亮接缝](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/base/src/input/editor/highlighting.rs)。

### 3.3 修订、变更事件与保存标记

- 应用 `DocRevision`（表格字段名 revision/expected_revision）表示一次已提交文本或会影响保存结果的编码/BOM状态变化的序号；undo/redo 也递增，用来拒绝过期任务。它不等于“是否有未保存内容”。
- 原生输入通过 `InputEditsEvent` 更新应用修订；应用事务也通过同一入口确认提交。一次事务若触发多个内部事件，适配层聚合为一条提交通知，避免订阅者重复记账。事件顺序与 Rope 快照必须成对使用。
- IME 预编辑是临时 UI 状态，不作为可保存的提交内容；确认、取消和焦点切换必须验证。上游在某些 IME 路径会发出全文 replacement，消费者必须支持，但不可据此每次全文复制。组合输入期间显式保存应先结束组合并等待提交；若无法可靠请求结束，则提示先确认输入，不读取临时拼音正文。后台恢复使用最近已提交 SnapshotHandle。
- dirty 的主实现为 Kit 历史中的稳定 history marker + EncodingSpec 保存选项标记。EOL转换改变真实文本，新插入行EOL偏好不单独标正文dirty。保存建立 coalescing boundary，避免保存前后的连续输入被合并到一个无法回到保存点的撤销单元。
- revision 始终递增，但 undo 回到保存 history marker 后应立即 clean。若组件无法可靠暴露 marker，则先保守显示 dirty，再在 undo/idle/保存后调用全局 fingerprint 服务对不可变快照后台比对；只接受相同 revision 的结果。禁止每敲一键全文哈希。
- 保存完成只确认被写入的快照，不能把保存期间新输入的修订标为 clean。

### 3.4 分屏

首版确定支持不同文件分屏。每个文档只有一个活动普通编辑视图；把同一文件再次打开应激活已有文档。普通标签关闭后只要文档仍在另一组存在，就不能销毁文档。

同文件独立双视图先设 capability 为 false。通过验证后，目标模型是共享一个文本/历史对象，ViewId 分别拥有选区、滚动、折叠和布局。当前 EditorState 内部耦合这些字段，不能将同一 Entity 同时绘制两次，或用两个 state 的 `set_value` 互相同步，来宣称满足该目标。

## 4. EOL 与坐标契约

### 4.1 原样换行政策

保持解码后字符中的真实 CRLF/LF/CR 于唯一权威 Rope，不在打开时自动规范化。BOM 由文件元数据负责，不作为编辑正文的首字符。未修改保存须由 file_io 的原字节保留策略保证，编辑器不得先改换行破坏它。

发布源码 `normalize_input` 对多行 Editor 没有主动统一 EOL，但 `rope_ext.rs` 的行转换使用 `LineType::LF`，`slice_line` 只移除 LF 并保留 CR。这说明 CR-only 行与 CRLF 导航尚不能视作已正确支持，M0 必须检查整个输入/显示链，而不是只检查加载 API。

统一使用 `EolScanner`：从字节边界扫描，优先识别 CRLF 二字节为一个换行，再识别独立 CR 与 LF；返回逻辑行的 content span、terminator span/kind。行号、行操作、列选择、文件保存扫描使用相同定义。CRLF 内部不能出现用户光标、选区截断或半个换行删除。

插入政策：Enter 使用当前逻辑行已有终止符；该行无终止符时用文档 dominant EOL，平票优先首次出现的样式，新建 Windows 文档默认 CRLF。普通粘贴保留剪贴板实际换行，提供单独“按文档换行粘贴”命令；自动缩进、重复行和列粘贴新建的行使用确定的 preferred EOL。显式“转换换行”是一个可撤销事务。

Tree-sitter 的行列解释与编辑器的 EolScanner 不得直接等同。若 parser 只以 LF 计行，解析适配器按原始字节重新计算 parser point；优先使用字节跨度回传高亮/折叠，显示层再映射成逻辑行。不要建立第二份可写的规范化正文。

依据：[发布版 rope_ext](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/base/src/input/base/rope_ext.rs)。这是实现缺口判断，并非已修复状态。

### 4.2 禁止混用的坐标

| 类型 | 定义 | 使用场景 |
| --- | --- | --- |
| RawByteOffset | 原编码文件字节位置 | file_io、大日志索引；不直接用于 Editor |
| Utf8Offset / Utf8Range | 当前 Rope UTF-8 字节位置 | 编辑、搜索命中、装饰、范围锚点；Range 为半开区间 |
| Utf16Position | UTF-16 code units | 原生 IME/某些协议边界；只在边界转换 |
| LogicalPosition | EolScanner 的逻辑行 + grapheme 列 | 跳转行列、状态栏；界面一基，内部零基 |
| DisplayColumn / 视图局部 LayoutPosition | 显示列；对应视图另维护显示行、像素 x/y、Tab 展开位置 | 软换行、鼠标命中、矩形选择；像素坐标不跨进程、不等于逻辑行列 |

统一包装这些类型，不能全部使用裸 `usize` 互传。用户范围必须落在 UTF-8/grapheme 边界；任意命令不得切开 emoji ZWJ、组合字符、CRLF。搜索引擎的字节范围需先验证 UTF-8 边界；高亮绘制可按 grapheme 向外吸附，替换只修改原匹配范围，不能因绘制吸附而改掉额外字符。

## 5. 多光标与列编辑

### 5.1 多光标基本规则

Windows 默认提供 Alt+单击加光标、Ctrl+Alt+Up/Down 加上下光标、Ctrl+D 选择下一相同文本。相同位置光标去重；重叠范围按 union 归并，保留活动光标方向。所有编辑范围基于编辑前同一 revision，从高偏移到低偏移应用，并作为一个事务。

普通粘贴：剪贴板逻辑行数恰等于光标数时每个光标分配一行；否则每个光标插入完整剪贴板。显式“列粘贴”命令始终按第 5.3 节处理。粘贴读取异步完成时复查文档 revision 和选区快照，目标已变更则取消，不把文本插入用户后来移动到的位置。

Esc 关闭最顶层弹出层；没有弹出层时退出列意图、只保留活动光标。Ctrl+A 在编辑区选全文；焦点在搜索输入框时只选该输入值。

### 5.2 列选区几何

产品列编辑以**逻辑行上的可见横向位置**为准，不能按 UTF-8 字节数或 UTF-16 单元数对齐。鼠标框选使用真实 GPUI 布局命中；Tab stops 使用同一等宽基准 advance。中文、字体 fallback、emoji 的宽度取真实布局，不能用 `unicode-width` 直接替换。

- Alt+拖动 / Alt+Shift+拖动进入列模式。进入时暂时关闭当前视图软换行，保存原开关；退出时恢复。这样同一逻辑行不会因为换行产生多次替换。状态栏显示“列模式”。
- 记录 `ColumnSelectionIntent`：起止逻辑行、左右布局位置、活动端、虚拟列、进入前 wrap 状态。它是视图元数据，不是第二份正文。
- 中文/emoji/grapheme 不能半选；非空矩形边界分别向 grapheme 外侧吸附。零宽列光标落在最近边界，等距离时取左边界。
- 矩形穿过 Tab 的部分宽度时，只在执行实际编辑时将受影响的 Tab 展开成等价空格，保留未选中的视觉宽度；仅选择、复制不改正文。
- 短行允许显示虚拟列意图。输入/列粘贴时用空格补到目标 Tab-grid 列，再插入；删除空虚拟部分不补空格。空行同样处理，不在每次拖动时制造空格。
- 折叠内容在开始跨行列编辑时展开涉及的折叠，避免对不可见逻辑行悄悄修改。拖动、滚动或字体/DPI 改变后重新命中；不能复用旧像素布局。

上游提供基础多光标与矩形拖选；它的短行默认裁切行为并不等价于上面的虚拟列插入要求。M0/ M3 需要补充意图与批量编辑，验收必须检查修改后的文本和一次 undo，而非截图看起来对齐。[Editor 功能文档](https://gpui-kit.com/component/editor/)

### 5.3 列插入、删除与粘贴算法

1. 取 revision、Rope 快照、ColumnSelectionIntent 与有效布局，逐逻辑行计算 grapheme/Tab-safe 范围。
2. 替换选区时，只删除实际存在的选中字节；短行缺失部分不删除。插入点不足左界时生成必要 padding。
3. 列粘贴一行时向每个选定逻辑行复制该行；多行时按行对应。剪贴板比选区高则向下扩展，超出 EOF 按 preferred EOL 新建行；比选区矮时其余行保留原文，不循环重复。
4. 行片段不含剪贴板自己的换行分隔符。新插入文本可含 Tab，按文档 tab_size 布局，不整体替换为硬空格。目标 padding 只使用空格。
5. 预先计算全部编辑，合并重复点、检查非重叠；通过统一 apply-batch 一次提交。每行光标移动到自身插入片段末端，保留活动行。undo 恢复文本、原选区、虚拟列意图和必要视图状态。

列编号是项目命令 `editor.column_numbers`：输入起始值、步长、最小宽度、填充字符（空格或0）、可选前后缀；首版仅十进制有符号整数。对列选区逐逻辑行生成 `start + row_index * step`，用 checked 整数运算，任一溢出或非法宽度则整批拒绝；负号在0填充前，例如宽度4的 `-3` 为 `-003`。按对应列插入/替换规则生成一个事务，不经 locale 分组格式化；提供结果预览前几行和行数，确认一次执行。未选择列时只对当前行插入一个编号，不猜测处理全文。

IME 政策：沿用 Kit 的单活动光标组合输入。开始中文 preedit 时退出矩形意图、清除次级光标，并在状态栏给出短提示；一次确认是一条撤销记录，取消不留下历史脏记录。不承诺同时在所有光标组合输入中文。单次已提交中文字符串的普通/列粘贴仍支持多处插入。真实微软拼音验收不可由直接调用插入 API 代替。

## 6. 常用编辑命令

这些命令由应用生成局部字节编辑并交给 Kit 事务；不需要新的编辑器库。针对行的命令以逻辑行而非软换行显示行为单位。范围末端恰位于下一行开头时，下一行不算被选中；空选区只处理当前行。多光标触及同一行时只处理一次。

| 命令 ID（项目定义） | 行为与算法 | 验收重点 |
| --- | --- | --- |
| `editor.duplicate_lines` | 复制被触及的完整逻辑行块到其下方；有多个不相交块时倒序处理；末行无 EOL 时先生成行间分隔，但复制结果仍保留无末尾 EOL | 多行、EOF、空行、多光标一次撤销 |
| `editor.move_lines_up/down` | 合并相邻行块，与前/后一行交换内容；保留原位置的 terminator 样式序列和全文末尾换行状态；位于边界无操作 | 混合 EOL 无意外统一；选区跟随移动内容 |
| `editor.delete_lines` | 删除完整被触及行；若删除抵达 EOF 的无终止符末行，则一并去掉前一个行间分隔；只剩一行时结果为空 | `a\nb` 删第二行得到 `a`；首行/末尾空行正确 |
| `editor.join_lines` | 删除块内行间 EOL，第二行起去掉前导空白，以单个空格连接非空片段；空片段不产生额外空格 | 仅修改被选块；可完全撤销 |
| `editor.indent/outdent` | Tab 插入 hard tab 或 tab_size 空格；outdent 删除一个 Tab 或不超过一个 tab stop 的前导空格 | 混合缩进、空行、CRLF不被算入内容 |
| `editor.trim_trailing_whitespace` | 只删除行内容末端空格/Tab，不删除 EOL；默认作用全文，也可选区 | 无内容变化时不记历史、不变 dirty |
| `editor.tabs_to_spaces` | 根据每行当前位置的 tab stop 展开 Tab，不固定逐 Tab 替换 N 个空格 | 前面有中文/组合字符时基准清晰 |
| `editor.leading_spaces_to_tabs` | 仅处理前导缩进，按 tab_size 贪心转换；行中空格保持 | 保持视觉缩进与 EOL |
| `editor.sort_lines` | 选择内行内容稳定排序，默认 Unicode 字符串字典序；可选倒序/忽略大小写，不采用 OS locale 随机变化；EOL 槽位与末尾换行保持 | 相等项稳定、空行、超大块后台计算与版本复查 |
| `editor.unique_lines` | 默认选中行块内全局去重、保留首次出现顺序；另设“只去相邻重复”命令，不自动排序 | 大量重复、大小写选项、末尾 EOL |
| `editor.uppercase/lowercase` | Rust Unicode 大小写转换，仅针对非空选区；转换可能改变长度，由 edit delta 更新范围 | `ß` 等长度变化、组合符、一次撤销 |
| `editor.goto` | 支持 `行[:列]`，一基 grapheme 列；超界夹到合法位置并反馈；展开目标折叠 | 中文/emoji列、末行、空文档 |
| `editor.select_next_occurrence` | 按当前明确选择搜索下一个相同字面量并加选；空选区先选当前单词；跳过现有范围 | 有界扫描、零长度、搜索范围过期 |
| `editor.toggle_line_comment` | 根据语言配置确定前缀；所有非空目标行都已有前缀时统一去除，否则在块的共同缩进位置统一添加 | 无语言前缀时禁用，不能给JSON伪造注释 |
| `editor.toggle_block_comment` | 使用语言配置的起止符包裹/解除完整选区；冲突/不支持时反馈，不修改 | XML `<!-- -->`、Python不冒充三引号注释 |

语言编辑配置由应用维护 `{line_comment, block_comment, bracket_pairs, auto_close, indentation_rules}`，与 grammar 分开。默认支持前缀：Rust/C/C++/JS/TS/JSONC `//`；Python/Shell/YAML/TOML `#`；INI `;`；SQL `--`；HTML/XML/Markdown 用块注释。JSON 禁用注释命令。YAML/TOML/INI 的注释是文本功能，不能据此宣称已通过各格式的完整解析验证。

### 6.1 自动缩进、括号与基础补全

优先使用 Kit 的 `LanguageConfig`、smart_indent、auto_close。两种选项独立保存。只有具备可靠语法上下文时，才允许按“字符串/注释中禁用自动闭合”决策；不得在无 context 时假装精准。括号跳转优先从对应语法树或只扫描有界文本；括号高亮使用独立装饰 owner。

首版补全为当前文档单词，不启动 LSP。后台维护有上限的词频索引：对变动逻辑行增删词，候选按前缀、频率与最近位置排序，限制候选数与词长；默认忽略超过容量阈值的文档。通过 Kit completion provider 接缝展示；M0 固定已编译的 provider 实际接口，不能根据文档中的 LSP 示例假定任意签名。显式触发先绑定 Alt+/，避免 Ctrl+Space 抢占中文输入法。

### 6.2 书签

由 `BookmarkService` 保存稳定 bookmark ID 与 UTF-8 行起点 anchor，gutter collection 只负责画标记。F2/Shift+F2 跳转，Ctrl+F2 切换当前逻辑行；书签开关只修改会话元数据，不修改正文 dirty。

编辑时由同一批 edit delta 更新 anchor；插入行前移动到原内容的新位置，删除整行移除对应书签。对文本 undo/redo，需要按相同 Kit transaction ID 恢复书签变动差量；这些差量不构成第二套文本撤销历史。行移动/排序命令另外提交旧新行映射，让书签跟随内容而非随机留在行号上。gutter decoration 的自动 tracking 不能代替业务书签，因为删除后的装饰不保证 undo 自动复活。

## 7. 语言与高亮配置

使用显式 grammar 白名单，首版不启用 `tree-sitter-languages` 全量打包。下表 feature 名已在 v0.10.0 Kit manifest 核对。`tree-sitter` 基础特性自带 JSON，没有 `tree-sitter-json` 这个 Kit feature。

| 文件类型 | 构建/注册方式 | 首版行为 |
| --- | --- | --- |
| 纯文本 | 不选 grammar | 无语法高亮；保留编辑、搜索、书签 |
| JSON | `tree-sitter` | JSON 高亮/结构折叠；语法错误不阻止编辑保存 |
| JSONC | Kit 将 jsonc alias 到 JSON grammar；单独样本验收 | 先提供宽容高亮；未通过注释、尾逗号、嵌套样本前，不宣称完整 JSONC 结构验证或可靠折叠 |
| YAML | `tree-sitter-yaml` | 高亮；缩进折叠需检查标量块与多文档样本 |
| TOML | `tree-sitter-toml` | 高亮、适用结构折叠 |
| INI | `tree-sitter-ini =1.4.0` + LanguageRegistry 注册 | 应用提供 queries/section 折叠与 `;` 注释规则 |
| XML | `tree-sitter-xml =0.7.0` + LanguageRegistry 注册 | XML grammar + 对应 XML_HIGHLIGHT_QUERY；折叠含嵌套元素，不复用 HTML 冒称 XML |
| Markdown | `tree-sitter-markdown`、必要时 `tree-sitter-markdown-inline` | 标题/围栏代码；嵌入语言仅对已启用 grammar 生效 |
| HTML/CSS | `tree-sitter-html` / `tree-sitter-css` | 各自 grammar；script/style 注入按已启用语言验证 |
| JavaScript/TypeScript/TSX | `tree-sitter-javascript` / `tree-sitter-typescript` / `tree-sitter-tsx` | 扩展名映射不混用；JSX覆盖需样本验证 |
| Python/Rust | `tree-sitter-python` / `tree-sitter-rust` | 高亮、语言缩进、括号；Python不以括号替代所有缩进结构折叠 |
| C/C++ | `tree-sitter-c` / `tree-sitter-cpp` | 分开映射；`.h` 默认 C++，用户可持久改指定类型 |
| SQL/Shell | `tree-sitter-sql` / `tree-sitter-bash` | SQL方言不承诺全覆盖；Shell首版指 Bash，不把 PowerShell 视为已支持 |

外部 grammar 注册通过公开 `LanguageRegistry` / `GrammarConfig`，不安装运行时下载器，也不 fork 整个语法引擎。确保直接 `tree-sitter` 依赖与 Kit 的 runtime 统一；生成 grammar 的 ABI 是否可用由真实 parse 和 query 编译确认，不能仅看 crate 版本相近。高亮 queries 的 capture 名映射到主题；无映射时使用普通文本颜色而非 panic。

语言检测优先用户指定，其次完整文件名映射、扩展名、shebang，最后 plain text。映射存于应用配置；切换语言不重建正文，不清撤销，不改用户缩进和自动闭合偏好。

性能政策：10 MB 以下默认开启支持的高亮/折叠；10–100 MB 先按 M0 profile 决定是否开启，默认关闭最贵的结构功能；1 MB 以上单行默认停用该文档结构高亮与软换行并显示原因。这些阈值是待测候选配置，不是已经达成的性能保证。所有降级保留完整编辑、保存和撤销；后台结果带 revision；大日志 reader 不接入完整 Editor 语法解析。

来源：[发布版 grammar features](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/kit/Cargo.toml)、[语言实现与别名](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/highlighter/languages.rs)、[公开注册表](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/highlighter/registry.rs)。

## 8. 工作区与界面

### 8.1 结构与组件职责

采用紧凑、内容优先的 VS Code / Zed 类工作区。首版至少包含菜单、可折叠文件树、标签组、编辑区、可展开的底部搜索结果、状态栏。避免在空白欢迎页堆仪表盘与装饰卡片。搜索、编码、日志模式的状态必须直接可见。

- Dock 负责布局、标签拖动、分组和不同文件分屏；应用 Panel 包装文档视图、文件树、结果列表。固定标签不会因 Ctrl+W 普通关闭被误关闭；显式关闭仍须检查修改。中键关闭和关闭组必须走同一关闭事务。
- 文档标签显示文件名、修改点、只读/错误状态；同名文件显示足够父目录以区分。点击、快捷键切换和命令面板激活应执行同一 workspace 命令。
- 默认文件树约 240 逻辑像素，可拖动；底部面板关闭时不预留大块空间。所有 px 是 DPI 缩放后的逻辑单位，不硬编码设备物理像素。
- 候选密度：UI 字号 13、编辑字号 14、行高约 1.5 倍；菜单/标签约 30–32 逻辑像素。用统一设置控制密度和字体；不把参考数字散落在每个 render 函数。
- 状态栏显示逻辑行列、选择/光标数、编码/BOM、EOL（含 Mixed）、缩进、语言、普通/日志模式、后台进度。涉及后台任务的按钮提供取消；日志模式持续显示只读和 tail 状态。

布局持久化只记录 Dock schema、panel ID/type、DocumentId/path 映射和可恢复视图状态。恢复时由 panel factory 重建 Entity；未知 panel 类型跳过并报告，不使整个会话崩溃。布局保存 debounce；窗口正常退出时提交最后状态。文档内容恢复由 session 模块负责，不能把未保存正文塞进 Dock JSON。

组件依据：[Dock](https://gpui-kit.com/component/dock/)、[Theme](https://gpui-kit.com/component/theme/)。

### 8.2 文件树、快速打开与命令面板

Tree 仅在展开目录时后台列举一级子项；节点按“目录优先、名称稳定排序”。加载/权限错误是单独节点状态。隐藏目录、搜索排除、是否显示被 ignore 文件是不同选项，不能把文件树是否显示误当搜索白名单。路径 identity 由全局 platform/file_io 服务提供，不能只 `to_lowercase()` 当成 Windows 唯一标识。

快速打开使用工作区文件路径索引和 MRU。先返回最近候选，再分批补充后台索引；每次查询 generation 递增，旧查询结果丢弃。默认候选渲染上限 200；不等待全文目录扫描结束才让输入框交互。模糊评分选定 `nucleo-matcher` 0.3.1，使用其 Pattern/Matcher API，在 ARCHITECTURE 已定义的后台 CPU 池分块计算并维护 top-200 最小堆；块边界检查 generation/取消，避免每次构造全量排序结果。以模糊分数为主、MRU与稳定路径为同分排序；查询中普通字符按字面 fuzzy 处理，避免无说明启用特殊查询操作符。命令面板复用同一评分服务。

matcher 操作的显示字符串与真正的 NativePath 分开；不可表示为 Unicode 的文件名用可区分的转义标签，打开仍使用原 NativePath。命中索引先按 matcher 的坐标映射为显示文本 grapheme 范围，不能直接当 UTF-8 偏移。官方特别提醒大集合不应直接在 UI loop 调低层 matcher，本项目由后台调度、增量任务和 top-K 满足该要求，不新建高层 nucleo 自带的另一套线程池。[nucleo-matcher 官方 API](https://docs.rs/nucleo-matcher/0.3.1/nucleo_matcher/)

Command 适合几百条命令，绑定全局 CommandRegistry。注册项包含稳定 ID、翻译 key、可见/可执行条件、参数 schema、快捷键与处理器。所有菜单、按钮、快捷键、命令面板都走同一 dispatcher。不能在四个入口写四套保存/替换逻辑。Command 会为条目测量布局，不能把百万文件或全部搜索结果作为 command entries。

搜索结果用等高摘要行；确需变高时再用 VirtualList 提供测量缓存。虚拟列表只减少渲染，不减少结果内存；数据层仍须分页、命中上限、截断提示与 source_revision。点击结果交 search/document 定位服务校验版本，不直接用旧 offset 移动光标。

依据：[Tree](https://gpui-kit.com/component/tree/)、[Command](https://gpui-kit.com/component/command/)、[VirtualList](https://gpui-kit.com/component/virtual-list/)。

### 8.3 搜索装饰接入

关闭 Editor 自带搜索 UI 的默认入口，应用统一实现查找/替换面板。独立 search 服务提供命中 UTF-8 范围；使用公开 TextDecorationCollection / RangeDecorationCollection 绘制，OverviewRulerCollection 显示概览。当前命中与其他命中分 owner 或独立样式，避免更新一个来源时清掉书签/诊断。

只装饰当前视口附近和有界命中集合；总数、分页游标与全部命中存储不放在绘图 collection。点击下一匹配后重新请求需要的范围。只读文件可搜索、不可替换；日志模式按 reader 能力显示支持的搜索类型。搜索面板关闭后，显式清理 collection 与请求 generation；持有 handle 的 drop 不等于清理所有装饰。

### 8.4 键盘、焦点和系统交互

默认使用现代编辑器快捷键方案，保留后续 Notepad++ preset 的 CommandRegistry 映射空间。候选默认键位：

| 功能 | Windows 默认 |
| --- | --- |
| 打开/保存/另存为/全部保存 | Ctrl+O / Ctrl+S / Ctrl+Shift+S / Ctrl+Alt+S |
| 快速打开/命令面板 | Ctrl+P / Ctrl+Shift+P |
| 当前查找/替换/文件夹搜索 | Ctrl+F / Ctrl+H / Ctrl+Shift+F |
| 跳转行 | Ctrl+G |
| 加下个相同文本/加上下光标 | Ctrl+D / Ctrl+Alt+Up/Down |
| 重复行/移动行 | Shift+Alt+Down / Alt+Up/Down |
| 行注释/块注释 | Ctrl+/ / Shift+Alt+A |
| 单词补全 | Alt+/，不占 Ctrl+Space |
| 书签切换/前后书签 | Ctrl+F2 / Shift+F2、F2 |
| 关闭标签/下一标签 | Ctrl+W / Ctrl+Tab |

快捷键按作用域分发：modal → 当前弹出层 → 当前输入控件/编辑器 → 当前 panel → workspace。快捷键设置保存 command ID + 参数 + context；启动时检查冲突、未知命令和保留键，并提供恢复默认。Alt 文本选择手势需要在编辑区消费，避免误触菜单。

打开 modal 前保存 WeakFocusHandle；Esc/确认后恢复仍存在的来源控件，否则回当前编辑器。关闭面板恢复合理邻近焦点，不把键盘留给隐藏控件。Tab/Shift+Tab 遍历界面，但编辑器内 Tab 默认插入缩进，使用显式“切换 Tab 导航模式”命令供键盘用户离开编辑器。菜单和列表支持方向键、Enter、Esc。焦点环清晰可见，图标按钮具有可访问名称；真实 NVDA、系统缩放和 IME 候选框在 M7 真机验证，不能只依据控件已有 AccessKit 宣称应用无障碍完成。

原生打开/另存为经 GPUI path prompt；用户取消保持文档与焦点状态，原生回调后才启动后台读写。编码/BOM/EOL 通过应用明确控件选择，不能要求不存在的原生编码下拉。剪贴板优先 Kit 原生动作；自定义列粘贴才包装 ClipboardItem。原生异步读剪贴板失败应显示可重试反馈，不静默吞掉。

GPUI path prompt 的 v0.10.0 官方示例：[Editor 示例](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/examples/editor/src/main.rs)。原生 API 参考：[GPUI app 源码](https://github.com/zed-industries/zed/blob/main/crates/gpui/src/app.rs)，此链接属于滚动 main，具体签名须以 Kit 锁定的 gpui-pre 0.3.12 为准。

### 8.5 主题与中英文

使用 Kit 语义主题；应用增加 editor/background、active-line、selection、search-match、bookmark、dirty、read-only 等语义 token，不在组件中硬编码散乱颜色。主题切换保留正文/历史/光标并触发需要的布局重算。字体 fallback 必须检查中文和 emoji；用户字体不可用时显示实际回退名称。

`rust-i18n` 4.2.x 应用文案维护 `assets/locales/ui.yml` 的 `app.*` namespace，组件覆盖放 `gpui_component` namespace；app crate 中的资源声明按工作区实际相对路径配置。在 Kit 初始化前注册翻译扩展，统一设置 locale，切换后主动刷新所有窗口。command ID、设置 key、文件名、正则正文不翻译。中英文界面均验收长标签、tooltip、菜单宽度与状态栏溢出。文案使用完整句子消息 key，不拼接翻译碎片。

依据：[Kit I18N](https://gpui-kit.com/docs/i18n/)、[发布版 I18N 指南](https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/website/docs/i18n.md)。

## 9. 验收用例

| 编号 | 输入/动作 | 通过条件 |
| --- | --- | --- |
| ED-AC-01 | 分别打开 `a\r\nb\n中\rc`、仅 CR、空文档、无末尾 EOL | 原正文逐字节 UTF-8 快照不变；逻辑行数正确；CRLF 内无光标；未修改保存走 file_io 原样保证 |
| ED-AC-02 | 每类 EOL 做 Enter、跨行删除、粘贴、一次 undo/redo | 精确恢复原字符串、选区、EOL 统计；CR-only 的显示和 parser point 不错位 |
| ED-AC-03 | 中文 + emoji ZWJ + 组合重音，跨 Rope chunk 移动/删除 | 不截断 UTF-8、grapheme 或 CRLF；行列显示与跳转一致 |
| ED-AC-04 | 100 光标批量修改、重复光标/重叠选区 | 一个事务、无重复应用；一次 undo/redo 恢复所有选择方向 |
| ED-AC-05 | `a\t中x`、空行、短行、长行上的矩形插入/删除/多行粘贴 | Tab部分处理、padding、超出EOF、每行对应按第5节精确实现；一次undo恢复原文 |
| ED-AC-06 | 开启软换行/折叠后进入列模式，随后改变字体/DPI | 明确切换wrap/展开涉及折叠；退出恢复原偏好；不复用过期像素几何 |
| ED-AC-07 | 微软拼音中文组合输入、取消、确认、焦点切换，原有多光标 | 按规定收束单光标；候选位置正确；一次undo；取消不留下文本/脏历史 |
| ED-AC-08 | 编辑→保存→编辑→undo→redo；保存期间输入 | 保存点clean判断正确，新输入保持dirty；无每键全文hash |
| ED-AC-09 | 所有行命令跑CRLF/LF/CR混合、末行有/无EOL、多选区 | 半开范围无多处理下一行；EOL/末尾换行符合各命令契约；无操作不增加历史 |
| ED-AC-10 | 注册全部首批语言、XML/INI queries；切主题/语言 | query编译成功；无未登记feature；文本和历史不重建；JSONC能力标记诚实 |
| ED-AC-11 | 打开100MB、1/10MB超长单行、持续输入撤销 | 满足PLAN冻结后的候选预算；profile无每键value()；降级有状态说明 |
| UI-AC-01 | 菜单/键盘/Command执行同一命令，修改参数/禁用状态 | 统一dispatcher；readonly下编辑命令不可执行，查找可用 |
| UI-AC-02 | 标签/不同文件分屏/布局恢复/关闭有修改文档 | 无文档丢失；焦点合理；所有关闭入口进入同一确认流程 |
| UI-AC-03 | 文件树慢目录/权限拒绝/循环链接，快速打开旧请求晚到 | UI可交互；目录惰性加载；无递归死循环；旧generation不污染新查询 |
| UI-AC-04 | 十万命中结果流入、滚动、关闭搜索、编辑后点击旧命中 | 有界结果与装饰；可取消；版本检查；关闭后不再接收结果 |
| UI-AC-05 | 中英文、明暗主题、125/150/200% DPI、多显示器 | 不截断核心按钮；焦点可见；IME/tooltip/原生对话框位置正确 |
| UI-AC-06 | 异步粘贴期间移动光标/编辑/关闭文档；取消原生选择器 | 不插入错误目标；不访问销毁Entity；状态与焦点恢复 |

自动化重点是文本契约、事务、EOL、修订与过期结果；IME、DPI、NVDA和原生对话框保留真机验收。对于几何依赖，用固定字体与尺寸的 GPUI test-support 集成测试；不能用字符串算法测试替代真实鼠标矩形选择。

## 10. 可直接分派给 AI 的任务

实施顺序与根计划一致。模块位置遵循 ARCHITECTURE 的 Cargo workspace：编辑器与UI在 `crates/app`；坐标/协议在 `crates/contracts`；EolScanner在 `crates/text-codec`。不能由各 AI 分别创建另一套 documents/editor 服务。下面是领域子任务，不代替 [AI_HANDOFF.md](AI_HANDOFF.md) 的全局任务 ID：基线对应 P00，事务对应 P01，EOL对应 P02，列输入原型对应 P03，容量汇总对应 P07；正式实现统一归入全局 E/U/G/S 任务。M0编辑接缝由同一负责人按 P01→P02→P03串行维护。

| 任务 | 依赖 | 具体实现与交付 | 必须通过 |
| --- | --- | --- | --- |
| E-M0-01 基线原型 | 无 | Windows Kit =0.10.0示例；记录toolchain/features/锁文件；核实默认图标、原生picker、IME；输出ADR | ED-AC-01/ED-AC-07基础路径，实际release可运行 |
| E-M0-02 EOL接缝 | E-M0-03；依全局P02顺序 | 审计LF-only helpers；统一EolScanner；输入/显示/parser位置补丁；原样CRLF/LF/CR路径 | ED-AC-01/ED-AC-02/ED-AC-03；不引入第二份可写正文 |
| E-M0-03 事务适配 | E-M0-01；对应全局P01 | EditorAdapter；原子apply-batch、选区快照、revision、InputEditsEvent聚合、history marker/保存boundary；记录补丁API | ED-AC-04/ED-AC-08；应用不调用pub(crate) API |
| E-M0-04 容量与双视图 | 前三项 | 100MB/超长行基准；共享文档实验；公开能力矩阵、降级阈值及双视图取舍 | ED-AC-11；对失败保留数据和决策记录 |
| E-M1-01 工作区壳 | E-M0-01，冻结DocumentId/ViewId | Dock panels、标签、不同文件分屏、状态栏、版本化layout恢复 | UI-AC-02，修改关闭流程可对接全局document服务 |
| E-M1-02 命令与输入 | E-M0-03 | CommandRegistry/dispatcher、GPUI Action、context快捷键、Command面板、焦点恢复 | UI-AC-01/UI-AC-05/UI-AC-06；不复制业务实现 |
| E-M2-01 设置和主题 | M1壳 | Theme语义token、字体与密度、rust-i18n双语、settings绑定 | UI-AC-05；切换不清历史 |
| E-M3-01 多光标列编辑 | EOL+事务+布局 | ColumnSelectionIntent、Tab/grapheme命中、虚拟padding、列粘贴、IME退出政策 | ED-AC-04/ED-AC-05/ED-AC-06/ED-AC-07，真实手势与单次undo |
| E-M3-02 行命令与书签 | EOL+事务 | 第6节全部稳定command IDs、语言注释配置、BookmarkService与事务元数据差量 | ED-AC-09；每条命令有输入输出样例 |
| E-M4-01 搜索呈现 | search服务请求/结果契约 | 应用搜索面板、独立装饰owner、结果虚拟列表、版本定位、取消清理 | UI-AC-04；不调用Kit matcher冒充正则服务 |
| E-M5-01 语法与补全 | EOL+事务，M0容量数据 | 白名单features、XML/INI注册、语言识别、单词索引provider、降级状态 | ED-AC-10/ED-AC-11；编译核对全部feature |
| E-M5-02 文件导航 | workspace索引/路径服务 | 惰性Tree、MRU、快速打开、后台nucleo-matcher评分/top-K、晚到结果过滤 | UI-AC-03；大目录不阻塞 |
| E-M7-01 交互回归 | 以上任务 | GUI集成与人工脚本、DPI/IME/NVDA结果、差异清单、性能profile | 第9节全部相应验收，失败不标完成 |

每个 AI 的交付说明必须包含：改动模块、调用的真实库/API、上游补丁范围、对应验收编号、运行结果、剩余差异。尚未编译/真机运行的功能写“未验证”，不能仅根据成功生成代码写“已完成”。
