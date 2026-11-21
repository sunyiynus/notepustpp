# M0 验证与实施记录

日期：2026-11-21。状态：工程已建立并编译运行；容量验收尚未完成，本文不把未测量的性能写成已通过。

## 环境

| 项目 | 实测值 |
| --- | --- |
| OS | Windows 11 IoT Enterprise LTSC，build 26100 |
| CPU | Intel Core i7-12700（12 代，20 逻辑核心） |
| 内存 | 34,129,498,112 字节（约 31.8 GiB） |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| cargo | 1.100.0 (3acc79c0c 2026-11-05) |
| host | x86_64-pc-windows-msvc |
| Rust channel | stable（`rust-toolchain.toml` 固定 stable；实际 1.100.0） |
| C 编译 | `cmake` 不在 PATH；`cc` crate 使用 MSVC 工具链成功编译 bundled SQLite 与 PCRE2 |

## 依赖锁定（Cargo.lock）

| 组件 | 版本 |
| --- | --- |
| gpui-kit | 0.10.0 |
| gpui-pre | 0.3.12 |
| gpui-base | 0.10.0（本地补丁，见 ADR） |
| gpui-component / gpui-kit-assets | 0.10.0 |
| ropey | 2.0.0-beta.1（与 Kit 同版本） |
| encoding_rs | 0.8.44 |
| chardetng | 1.0.0 |
| regex | 1.13.2 |
| pcre2 | 0.2.11 |
| rusqlite | 0.43.0 |
| windows | 0.62.2 |
| blake3 | 1.8.7 |
| tempfile | 3.27.0 |

`Cargo.lock` 已提交。`cargo tree -d` 仍会显示 GPUI 家族内部及 `windows` 的多版本；未做“全局无重复”的虚假承诺。

## 已编译验证的 Kit 接缝

以下 API 通过真实编译和运行验证，而不是按名称猜测：

- `gpui_kit::application().run`、`gpui_kit::init`、`open_window`、`Root`。
- `EditorState::new`、`set_value`、`value`、`set_selected_range_with_direction`、`set_soft_wrap`、`set_line_number`、`set_show_whitespaces`、`set_folding`、`set_tab_size`、`set_editor_paddings`、`set_searchable`。
- `InputEditsEvent`、`InputEvent::PressEnter` 订阅。
- `create_range_decorations_collection` / `RangeDecoration` / `RangeDecorationStyle::Fill`，用于查找命中高亮。
- `InputState`（查找/替换输入框）与 `Input` 元素。
- `window.on_window_should_close`，用于退出前写恢复检查点。

限制（未解决，不能声称完成）：

- `replace_text_in_ranges` 是 `pub(crate)`，应用未获得公开批量事务 API。
- `set_value` 仍会清历史；只用于首次加载或恢复副本。
- 同一文档独立双视图未启用（capability = false）。
- Kit 行索引仍以 LF 为主：CRLF 文件的行号正确；CR-only 文件的行号/上下移动未验收。

## 本地 gpui-base 补丁

见 [adr/editor-kernel.md](adr/editor-kernel.md)。补丁只在 Enter 插入路径增加 `PreferredLineEnding`，默认值仍为 LF，保持 0.10.0 其他行为不变。应用打开 CRLF 文档后设置为 CRLF，避免用户按 Enter 后静默混入 LF。

补丁已通过编译和 GPUI test-support 的真实输入测试：

- `enter_inserts_crlf_when_the_document_uses_crlf`：输入 `X中😀` 后按 Enter，断言文本为 `X中😀\r\na\r\nb`。
- `enter_inserts_lf_for_an_lf_document`：LF 文档仍插入 LF。
- `undo_restores_crlf_document_after_enter`：Enter 后 Ctrl+Z 恢复原 CRLF 文档。

这些测试使用 headless 窗口、真实焦点和键位分发，不是直接调用插入 API。仍未做微软拼音真机、候选框、DPI 和 CR-only 行号验收。

## 文件与保存验证

- `notepust-file-io` 9 个测试通过：原始字节保存、冲突拒绝、外部修改检测、备份、编码失败不触碰目标、编码猜测需要确认。
- Windows 文件身份使用 `CreateFileW` + `FILE_FLAG_OPEN_REPARSE_POINT` + `GetFileInformationByHandleEx(FileIdInfo)`；测试验证同一文件身份稳定。
- 已有文件保存使用 `ReplaceFileW`，新文件使用 no-clobber persist；测试验证不覆盖新出现的目标。
- 保存失败错误分类：冲突、编码失败、I/O、需人工核对分别返回结构化结果。
- 状态栏编码面板已提供“重新解释为”（重新解码磁盘字节，要求文档无未保存修改）与“保存编码”（标记脏并在下次保存时用新编码重写）两类操作；不可编码字符仍通过 `encode_text` 报错而不写目标。

## 搜索 worker

- `search-worker` 为独立 binary，不依赖 GPUI。
- 进程级集成测试通过：
  - Hello/Ready 协议；
  - 标准 regex 找到 2 个命中；
  - PCRE2 `hello(?= world)` 前后查找找到 1 个命中；
  - `(hello)` + `$1!` 生成 26 字节 artifact，BLAKE3 校验一致；
  - 非法模式返回 `InvalidRequest` Failed 帧。
- 父进程监督器 `WorkerClient` 已实现 handshake、有界帧、取消/超时 kill+wait；当 `target/debug/search-worker.exe` 存在时测试通过。
- 应用查找条已接入 `PCRE2` 开关：查找与全部替换会把当前文本写入临时快照，由 worker 执行，再校验替换 artifact 的 BLAKE3 后回填编辑器。应用层测试覆盖 `foo(?=\d)` 与 `[$0]` 替换。
- 未设置 PCRE2 原生 match limit；依赖进程内存墙钟超时与 kill 作为硬边界。该限制在规格允许范围内，但需要后续用灾难性回溯 fixture 验证实际退出时间。

## 打开延迟（release，单次样本）

使用 `tests/fixtures/measure-open.ps1`，从 `Start-Process` 到窗口标题出现：

| 样本 | 字节数 | 行数规模 | 结果 |
| --- | --- | --- | --- |
| `sample-10mb.log` | 10,850,576 | 175,000 | 438 ms，Responding=True |
| `sample-100mb.txt` | 104,857,600 | 约 2.2M | 865–891 ms（2026-11-21 复测），Responding=True |

两个样本都超过“50K 行”后自动关闭折叠和软换行，否则 175K 行日志会出现 Not Responding。该降级阈值目前是 4 MiB 或 50,000 行（近似）。

超长单行：1 MB 单行可以打开并编辑；超过 2 MiB 单行的文件现在自动进入只读日志视图，而不是拒绝打开。`long-line-10mb.txt` 实测 492 ms 到标题、WorkingSet 约 79 MB。

这些是“打开到标题更新”的代理指标，不是完整首帧/输入延迟；未测量峰值内存、编辑延迟、撤销、保存和再打开。100 MB 输入 P95 目标仍需正式基准。

## 大日志

- `large-file` 23 个测试通过：稀疏索引、CRLF 跨块、UTF-16 视口、长行截断标记、tail、append/truncate、ReadAt，以及流式搜索（跨 1 MiB 块、UTF-16、GBK、行级 regex、取消、结果截断）。
- 只读日志 UI 已接入：打开超过 100 MiB 的文件或超过 2 MiB 的单行文件时自动进入日志模式；显示行号、分页（顶部/上一屏/下一屏）、跟随开关、刷新和后台稀疏索引进度。
- 日志内搜索：查找条按字面量或行级 regex 扫描，命中行高亮，F3/Shift+F3 循环；搜索在后台流式读取，不把文件载入编辑缓冲。
- `tests/fixtures/log-smoke.ps1` 实测 1 GB 样本：371–813 ms 到文档标题，WorkingSet 约 79–83 MB，Responding=True；3 秒后索引进度约 9% 且界面保持响应。
- 尚未实现：日志内替换（只读设计）、未索引远距离命中立即跳转（需等待后台索引）、横向滚动与超长行分段导航。

## 语法高亮、分屏与行操作

- Tree-sitter 增量高亮通过 GPUI-Kit `InputHighlighter` 接缝接入，语法树同时提供折叠区间；首批 grammar：Rust、C/C++、Python、JS/TS/TSX、JSON、YAML、TOML、Markdown、HTML/CSS、SQL、Shell。
- 同步解析预算 8 ms，超时后在停顿 180 ms 时重建一次；超过 2 MiB 不解析，保持纯文本输入路径。
- `crates/app/src/line_ops.rs` 纯函数 + 7 个测试覆盖多行 CRLF 复制/删除/移动/排序/去尾空格/注释/缩进，以及中文显示列多光标；所有行操作通过一次 `replace_all` 事务应用。
- 分屏为两个独立标签窗格，点击窗格切换当前文档；列选区使用 Kit 原生 Alt+点击/Alt+Shift 拖动，另有 `Ctrl+Alt+↑/↓` 多光标。
- 字体设置面板支持界面/编辑字体、备用字体链与字号，应用后即时重排编辑器和界面。

## 文件关联与安装包

- `notepust.exe --register-file-types` 实测写入 `HKCU\Software\Classes\Notepust.Document`、`Applications\notepust.exe` 和 29 个扩展的 `OpenWithProgids`；`--unregister-file-types` 只移除 Notepust 自己的键/值，Zed 的既有条目保持不变。
- 注册不改变默认打开方式、不写 `UserChoice`/HKLM，不需要管理员权限。
- `installer/notepust.nsi` 为每用户 NSIS 安装脚本，安装/卸载时调用上述命令；NSIS 未安装，因此安装包本身尚未实机构建验收。

## 单实例转发

`tests/fixtures/single-instance-smoke.ps1`（debug）实测：

```text
FirstTitle=hello.txt - Notepust
SecondExitCode=0
SecondExitMs=59
ForwardedTitle=small-mixed-eol.txt - Notepust
NotepustInstances=1
```

首进程创建 `Local\Notepust.SingleInstance.v1` mutex 与命名管道；第二进程只发送路径/行列 JSON 后退出。`--new-window` 跳过单实例占用。平台层两个真实双进程测试覆盖主进程服务与第二进程转发。

## GUI 冒烟

命令（PowerShell，真实启动桌面窗口）：

```powershell
.\tests\fixtures\smoke.ps1
```

实测输出（debug 与 release 均验证）：

```text
Configuration=release
PID=15016
Title=hello.txt - Notepust
Responding=True
```

说明：窗口创建、GPUI-Kit 初始化、文件打开、标签/标题更新、事件循环均到达首帧。已用 DPI-aware 截图脚本（`tests/fixtures/log-screenshot.ps1`）验证 Rust 语法高亮、字体设置面板和双窗格分屏布局；截图产物在 `tests/generated/`（gitignore）。未自动模拟键盘输入。

构建体积（本机 release）：`notepust.exe` 约 48 MB（含 15 个 Tree-sitter grammar），`search-worker.exe` 约 2.8 MB；debug 版 `notepust.exe` 约 37 MB。体积未做 strip/优化决策。

`cargo tree -d` 对 `ropey`、`gpui-base`、`gpui-pre`、`gpui-component`、`windows`、`regex`、`encoding_rs` 未发现重复版本。

## 测试与构建结果

```text
cargo fmt --all --check          通过
cargo clippy --workspace --all-targets -- -D warnings   通过
cargo test --workspace           155 passed, 0 failed
cargo build -p notepust-app      通过（debug）
cargo build --workspace --release   通过（3m24s，含 Tree-sitter grammar）
```

尚未执行：

- `cargo nextest`（未安装）
- `cargo deny`（未配置）
- release 100 MB 输入 P95 与语法高亮基准
- IME、DPI、NVDA、鼠标矩形选择真机脚本
- NSIS 安装包实机构建/安装/卸载验收
- 便携 ZIP 与代码签名

## 下一步（按规格优先级）

1. 文件树、命令面板与布局恢复；日志远距离命中跳转与横向导航。
2. 用 GPUI test-support 为查找定位、分屏和设置面板建立自动化交互测试。
3. 去重行、大小写转换、列插入删除与书签；补全/自动闭合 UI。
4. 便携 ZIP、图标/版本资源、代码签名与 NSIS 安装包实机验收。
5. 固定基准机上的 100 MB 输入 P95、内存、IME/DPI/NVDA 验收。
