# ADR：编辑内核与 gpui-base 换行补丁

状态：已采纳；2026-11-21。  
范围：普通编辑内核、唯一文本所有权、CRLF 保真补丁。  
相关规格：[ARCHITECTURE.md](../ARCHITECTURE.md)、[EDITOR_UI_SPEC.md](../EDITOR_UI_SPEC.md)。

## 背景

首版要求：

- 常规编辑只保留一份权威文本和一套撤销历史。
- 打开和保存不能自动把 CRLF/LF/CR 归一化。
- 100 MB 编辑路径不能每次按键复制全文。
- 应复用 GPUI-Kit 的输入、选区、撤销、渲染能力，而不是另写编辑器。

GPUI-Kit 0.10.0 的 `EditorState` 持有 `Rope`、`UndoManager`、`DisplayMap` 和选区。`set_value` 会清空历史并重置选区/滚动，因此只能用于首次加载或用户明确接受的重新加载。`value()` 会构造全文 `String`，因此保存、恢复和后台搜索才有资格调用，不能放在每次按键或每帧的路径上。

发布源码中 `replace_text_in_ranges` 仍是 `pub(crate)`，应用没有公开的“一次原子批量编辑”入口。这是本项目后续需要继续处理的上游缺口。

## 决策

1. **唯一权威文本**：每个普通文档只有一个 `Entity<EditorState>`。应用不创建第二份可写 `String`/`Rope`，不维护应用级文本撤销栈。
2. **文档元数据分离**：路径、编码、BOM、磁盘 stamp、修订号、dirty、恢复状态保存在 app/documents 层；视图选区由 Kit 状态持有。
3. **保存快照**：保存时读取 `value()` 一次，生成不可变文本快照并在后台写入；保存完成后只把该修订标为已保存。
4. **修订与任务**：`InputEditsEvent` 触发应用 `DocRevision` 递增；后台搜索结果携带 revision，过期结果不得定位或替换。
5. **换行保真**：保留 CRLF/LF/CR 在权威文本中。针对 Kit 的 Enter 硬编码 `\n`，维护 `vendor/gpui-base` 最小补丁：
   - 新增 `PreferredLineEnding { Lf, CrLf, Cr }`。
   - 新增 `set_preferred_line_ending`。
   - Enter 的多光标、括号拆分和普通插入路径使用该值。
   - 默认仍为 LF，未改变其他 Kit 行为。
6. **脏状态**：输入即标脏；≤2 MB 文档在后台把当前全文与保存后的文本比较，撤销回保存文本时清除 dirty。>2 MB 文档保持保守 dirty，直到下一次保存。
7. **外部字节**：打开时保留原始 source bytes 与 BLAKE3。未修改另存为直接输出 source bytes；有修改才重新编码。

## 为什么不用应用级补丁替代

尝试过的替代方案：

- **每次编辑后 `set_value` 同步两份文本**：会清历史、重置选区、全文复制，明确违反规格。
- **按键后修正 `\n` 为 `\r\n`**：多光标/IME/撤销边界不可靠，且会产生第二个撤销单元或额外全文操作。
- **把文本归一化为 LF 后保存时再转回**：会破坏原样保存、搜索偏移和字节保真。
- **fork 整个 gpui-component**：范围过大；只补一个 Enter 接缝可以保持依赖版本和可复现性。

## 后果

优点：

- 单一文本与撤销历史。
- CRLF 文档按 Enter 不会新增 LF 行。
- 后续搜索、恢复、保存都消费同一套 UTF-8 偏移语义。

限制与待办：

- `vendor/gpui-base` 是固定源码副本；升级 Kit 时必须重新应用补丁并重跑 EOL/输入回归。
- Kit 的行索引仍以 LF 为主。CRLF 文件行号正确；**CR-only 文件的行号、上下移动和部分选择行为尚未验收**。
- 没有公开批量编辑 API，全文替换暂用 `replace_all`；百万级替换应改走 worker `ReplacementArtifact` + 适配层导入。
- 同一文档独立双视图未启用；不能把两个 `EditorState` 用 `set_value` 互相同步来绕过所有权约束。
- IME 预编辑与多光标组合行为保留真机验收。

## 验证

- `cargo build -p notepust-app` 通过。
- `cargo test --workspace` 124 个测试通过。
- GPUI headless 真实键盘测试：CRLF Enter、LF Enter、Enter 后 Ctrl+Z，含 `中😀` 输入。
- 桌面窗口启动并成功显示 `hello.txt - Notepust`；release 下 100 MB 文本打开到标题约 752 ms（见 `docs/M0_RESULTS.md`）。
- 未执行：微软拼音真机、CR-only 行号、100 MB 输入 P95/撤销/保存延迟、峰值内存。
