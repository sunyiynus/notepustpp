# 安装包、便携模式与文件关联

日期：2026-11-21。对应实现：

- 注册表模块：`crates/platform/src/file_assoc.rs`（公开 API 从 `notepust_platform` 重新导出）。
- NSIS 脚本：`installer/notepust.nsi`。
- 本文档。

全部注册表写入只发生在 `HKCU\Software\Classes`，不需要管理员权限，也不会抢占用户的默认打开方式。Windows 10/11 最终仍由用户在“默认应用”或“打开方式”界面确认。

## 当前状态与前置条件

| 项目 | 状态 |
| --- | --- |
| `file_assoc` 注册/注销/命令行 API | 已完成 |
| NSIS 安装、卸载、快捷方式、可选右键菜单 | 已完成（未实际运行 `makensis`） |
| `notepust.exe --register-file-types` / `--unregister-file-types` CLI | 已完成，在窗口和单实例逻辑之前处理并立即退出 |
| `windows` crate 的 `Win32_System_Registry` feature | 已在工作区启用 |
| 真实注册表冒烟 | 已执行：注册 29 个扩展 → 查询 ProgID/Applications/OpenWithProgids → 注销后仅移除 Notepust 自有值，Zed 条目保留 |

## 便携目录布局

便携模式由可执行文件同目录下的 `portable` 标记文件启用（文件名固定，内容不读取）。`notepust-platform::resolve_profile` 的解析结果如下：

```text
Notepust\
├─ notepust.exe
├─ search-worker.exe
├─ portable                  # 空标记文件；存在即使用便携 profile
└─ data\
   ├─ config\
   │  ├─ settings.toml       # 用户设置
   │  └─ layout.json         # 布局
   ├─ data\
   │  ├─ recovery.sqlite     # 恢复/会话数据库
   │  ├─ logs\               # 日志
   │  └─ backups\            # 保存前备份
   └─ cache\
```

说明：

- 没有 `portable` 时使用 `directories` 提供的当前用户配置/数据/缓存目录。
- 便携版不需要安装器，也不写卸载信息；直接解压 ZIP 使用。
- 便携目录必须可写；不可写时应用按现有 profile 逻辑报错，不会静默写到别处。
- 只有显式运行 `notepust.exe --register-file-types` 才会写注册表；此时 ProgID 和命令指向便携目录中的 exe。

## 构建 NSIS 安装包

要求：

1. Windows 10/11 x64、Rust stable MSVC 工具链。
2. NSIS 3.x（Unicode 版）；脚本使用 `Unicode true`，并以 UTF-8 BOM 保存，保证“使用 Notepust 编辑”等中文正确编译。
3. 先构建 release 产物：

```powershell
cargo build -p notepust-app -p search-worker --release
```

必需文件：

| 文件 | 来源 |
| --- | --- |
| `target\release\notepust.exe` | `notepust-app` |
| `target\release\search-worker.exe` | `search-worker` |

编译安装包（在仓库根目录执行）：

```powershell
makensis installer\notepust.nsi
```

输出：

```text
installer\Notepust-Setup-x64.exe
```

脚本使用 `${__FILEDIR__}` 定位同目录的 `..\target\release`，因此从仓库根目录或 `installer` 目录调用都可以；产物始终写在 `installer` 目录，发布时再移动到分发目录。本次改动没有运行 NSIS。

## 安装器行为

- 当前用户安装：`RequestExecutionLevel user`，默认目录 `%LOCALAPPDATA%\Programs\Notepust`，目录页可修改。
- 卸载信息写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Notepust`，安装路径同时写入 `HKCU\Software\Notepust\InstallDir`。
- 必需组件：复制 `notepust.exe`、`search-worker.exe`、`uninstall.exe`，然后执行：

```text
"<安装目录>\notepust.exe" --register-file-types
```

- 可选组件（默认不勾选）：开始菜单快捷方式、桌面快捷方式、资源管理器右键菜单。
- 右键菜单使用 `SystemFileAssociations\.<ext>\shell\Notepust`，只为受支持扩展增加“使用 Notepust 编辑”动词；不改 `.ext` 默认值，不写 `UserChoice`。
- 卸载顺序：删除右键菜单动词 → 执行 `"<安装目录>\notepust.exe" --unregister-file-types` → 安全清理 Notepust 自有键值 → 删除快捷方式和程序文件 → 删除安装/卸载元数据。
- 用户设置、恢复数据库、日志、备份和用户文档一律保留；便携目录不受安装器影响。

`--register-file-types` / `--unregister-file-types` 必须由应用在打开窗口之前处理并立即退出；否则 `ExecWait` 会等待 GUI 关闭，卸载时还会占用 `notepust.exe`。

## 注册表项

### 写入（`register_file_associations`）

| 键 | 值 | 类型/数据 |
| --- | --- | --- |
| `HKCU\Software\Classes\Notepust.Document` | `(默认)` | `REG_SZ` = `Notepust Document` |
| `...\Notepust.Document\DefaultIcon` | `(默认)` | `REG_SZ` = `"<当前 exe 路径>",0` |
| `...\Notepust.Document\shell\open\command` | `(默认)` | `REG_SZ` = `"<当前 exe 路径>" "%1"` |
| `HKCU\Software\Classes\Applications\notepust.exe` | `FriendlyAppName` | `REG_SZ` = `Notepust` |
| `...\Applications\notepust.exe\SupportedTypes` | 每个扩展名 | `REG_NONE`，空数据 |
| `...\Applications\notepust.exe\shell\open\command` | `(默认)` | `REG_SZ` = `"<当前 exe 路径>" "%1"` |
| `HKCU\Software\Classes\.<ext>\OpenWithProgids` | `Notepust.Document` | `REG_NONE`，空数据 |

- 受支持扩展名共 29 个：`.txt .log .md .markdown .json .jsonc .toml .yaml .yml .ini .xml .html .htm .css .js .mjs .cjs .ts .tsx .jsx .py .rs .c .h .cpp .hpp .sql .sh .bash`。
- 返回值 `AssociationReport { prog_id, extensions }`，其中 `extensions` 为本次登记的扩展名数量。
- `association_command_line()` 返回供文档/打包脚本使用的 `"<exe>" "%1"` 字符串；实际写入注册表时使用原生 UTF-16 路径，不经过有损字符串转换。
- 可选的安装器右键菜单还会写 `HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell\Notepust`（默认值、`Icon`、`command`）。

### 删除（`unregister_file_associations`）

- `RegDeleteTree` 删除 `HKCU\Software\Classes\Notepust.Document` 整棵子树。
- `RegDeleteTree` 删除 `HKCU\Software\Classes\Applications\notepust.exe` 整棵子树。
- 对每个扩展名，只删除 `HKCU\Software\Classes\.<ext>\OpenWithProgids` 下的 `Notepust.Document` 值；`OpenWithProgids` 键本身保留（可能属于其他程序），缺失时忽略。
- 安装器卸载阶段额外删除 `SystemFileAssociations\.<ext>\shell\Notepust` 子树、卸载信息键和快捷方式。

### 明确不会写入或修改

- `HKCU\Software\Classes\.<ext>` 的默认值。
- `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.<ext>\UserChoice`。
- `HKLM` 下的任何键；也不直接写 `HKCR`（`HKCU\Software\Classes` 会合并进 `HKCR` 视图）。
- 其他程序的 `OpenWithProgids` 值、`Applications` 条目或 ProgID。

## Windows 10/11 的默认应用行为

`OpenWithProgids` 是有意采用的“非劫持”方案：

- Notepust 会出现在资源管理器“打开方式”和“选择其他应用”列表中。
- 系统不会自动把 Notepust 设为默认程序；用户勾选“始终使用此应用打开 .xxx 文件”后，由 Windows 自己写入受哈希保护的 `UserChoice`。
- 用户也可以进入“设置 → 应用 → 默认应用 → 按文件类型指定默认应用”选择 Notepust。
- 因此安装完成后首次双击受支持文件时，Windows 仍可能弹出“你要如何打开此文件？”。
- 移动或重装程序后，重新运行 `notepust.exe --register-file-types` 即可刷新 ProgID/命令中的 exe 路径。

## 验证

```powershell
cargo fmt --all
cargo clippy -p notepust-platform --all-targets -- -D warnings
cargo test -p notepust-platform
```

单元测试只校验扩展名列表（无重复、全部以 `.` 开头、小写 ASCII）和命令行格式，**不会写真实注册表**。

手工检查：

```powershell
.\target\release\notepust.exe --register-file-types
reg query "HKCU\Software\Classes\Notepust.Document" /s
reg query "HKCU\Software\Classes\Applications\notepust.exe" /s
reg query "HKCU\Software\Classes\.md\OpenWithProgids"
.\target\release\notepust.exe --unregister-file-types
```

预期：`.md` 的 `OpenWithProgids` 含 `Notepust.Document`；注销后 Notepust 自有键值消失，其他程序的值和用户设置仍保留。

## 仍需手动完成

1. 图标、版本资源、代码签名和最终发布目录尚未配置。
2. `installer/notepust.nsi` 的扩展名列表与 `SUPPORTED_EXTENSIONS` 必须同步；当前两边都是 29 项，后续增删需同时修改（可考虑加脚本级一致性检查）。
3. 便携 ZIP 打包脚本尚未提供；当前便携模式只需手动放置 `portable` 标记和两个 exe。
4. `docs/BUILD_RELEASE_TESTS.md` 写的脚本位置是 `packaging/windows`；本次按需求放在 `installer/notepust.nsi`，后续统一文档或目录即可。
5. NSIS 未安装，安装包尚未实机构建/安装/卸载验收。
