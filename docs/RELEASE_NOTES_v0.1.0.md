# Notepust v0.1.0

首个公开版本，Windows x64 便携包。

## 主要功能

- 多标签编辑：打开、保存、另存为、全部保存、从磁盘重载、快速打开（Ctrl+P）、跳转行（Ctrl+G）、最近文件。
- 编码与换行：UTF-8/BOM、UTF-16 LE/BE、GBK、GB18030；保留 CRLF/LF/CR；状态栏编码面板可重新解释或更改保存编码。
- 查找替换：字面量、常规正则、PCRE2 高级正则（独立 `search-worker.exe`）、整词、大小写、全部替换、F3。
- 文件夹搜索、只读大日志视图、日志内流式字面量/行级正则搜索、分页与跟随。
- Tree-sitter 语法高亮与折叠，首批 Rust、C/C++、Python、JS/TS/TSX、JSON、YAML、TOML、Markdown、HTML/CSS、SQL、Shell。
- 分屏、行操作（复制/删除/移动/排序/去尾空格/注释/缩进）、多光标与列选区。
- 字体设置面板：界面字体、编辑字体、备用字体链与字号。
- SQLite 恢复检查点、会话、最近文件、保存前备份、外部修改检测。
- 单实例转发；`--register-file-types` 注册每用户文件关联。

## 安装与运行

解压 ZIP，运行 `notepust.exe`。压缩包带有 `portable` 标记，设置、恢复库和日志写在程序目录下的 `data\`，适合便携使用。

如需让资源管理器“打开方式”出现 Notepust：

```powershell
.\notepust.exe --register-file-types
```

移除：

```powershell
.\notepust.exe --unregister-file-types
```

## 已知限制

- 日志视图按设计只读，不提供日志内替换。
- 超过 2 MiB 的文档关闭 Tree-sitter 解析，保持纯文本输入性能。
- INI/XML 暂无专用 grammar，XML 暂用 HTML grammar。
- 发布包未签名；Windows 可能显示 SmartScreen 提示。
- NSIS 安装脚本已提供，但本版本未附安装包。
