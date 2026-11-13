# Rowglide 开发约定

@/Users/yousa-mirage/.codex/RTK.md

## 项目目标

Rowglide 是使用 Rust、GPUI 和 gpui-kit 构建的桌面 Parquet / CSV 查看器。核心体验是快速浏览表格、展开嵌套单元格、按列排序、筛选和执行基础 SQL。

开发路线与阶段验收见 [dev.md](dev.md)。已接入 DuckDB 真实后端，模拟后端保留用于测试。不要引入第二个真实查询引擎。

## Workspace 与依赖边界

采用三个 crate，程序入口合并到 UI：

```text
crates/rowglide-app/        核心模型、请求、结果及后端接口
crates/rowglide-ui/         GPUI 界面、交互状态及程序入口
crates/rowglide-core/       模拟后端和后续真实后端

rowglide-ui -> rowglide-core -> rowglide-app
rowglide-ui ------------------> rowglide-app
```

- `app` 不依赖 GPUI、gpui-kit 或具体查询引擎；可以依赖 Arrow。
- `core` 不依赖 UI，不持有 GUI 对象。
- `ui` 的入口负责构造并注入后端。组件和交互逻辑通过 `app` 接口工作，不直接调用具体引擎。
- 打开的文件使用 gpui-kit Folder Tabs 在顶部展示与切换，不使用左侧文件侧栏。
- 包之间不得出现循环依赖。`app` 存放共享模型与接口，程序入口仍位于 `ui`。
- 实现前核实 gpui-kit 的实际上游地址、API、版本和平台支持，不用其他组件库静默替代。

## 数据与交互约定

- 保留类型信息，优先以 Arrow schema / RecordBatch 表达表格结果；不要将整个数据集转换为字符串矩阵。
- 区分源文件信息、查询结果和当前视图状态。CSV 的 JSON 字符串不是原生嵌套类型，JSON 查看须保留原文。
- 嵌套查看支持列表、结构体和键值映射；Arrow dictionary encoding 不等同于键值映射。
- 表格和长列表使用虚拟化；数据加载与缓存也必须有界，不能只优化绘制。
- 文件读取、扫描、SQL、排序和筛选离开 GUI 线程。查询接口支持分批交付、背压或按需拉取、取消和明确结束状态。
- 使用请求标识阻止旧查询覆盖新结果。取消与错误路径均须释放后台资源。
- 排序和筛选作用于完整查询数据，不能只处理当前屏幕或已加载批次。
- Browse 使用显式 Limit / Offset 分页，Limit 是每页行数；不通过滚动自动递增 OFFSET。深分页可能重复扫描，排序并列值须有确定顺序。不假设任意 SQL 结果顺序稳定。
- Browse 不得静默截断用户的 Limit；超过资源预算时明确报错。SQL 完整结果与有界缓存属于独立实现阶段，不能将 Browse 分页参数应用到用户 SQL。
- SQL 构造必须正确引用标识符，并对筛选值使用参数绑定。不要通过字符串拼接插入文件路径或用户输入。
- 首屏预览不等待精确总行数；未知、估算与准确行数必须区别展示。
- 模拟 SQL 只演示明确的预设结果和状态，不自行实现 SQL 解析器，不伪装成真实执行。

## 版本管理：jj

- 使用 `jj` 管理版本；仓库已有 `.jj` 和 `.git` 时，`.git` 可以是 jj 的底层存储，不代表改用 Git 工作流。
- 开始修改前检查 `jj status`、`jj log -r 'ancestors(@, 5)'` 和必要的 `jj diff`，识别已有工作。
- 每个独立的修改都视为独立的 jj change。开始下一项独立修改前使用 `jj new`，不要把互不相关的修改混入同一个 change；同一修改所需的实现、测试和文档可以放在同一个 change 中。
- 使用 `jj diff` 审阅变更，使用 `jj describe -m "..."` 描述 change。
- 不使用 `git add`、`git commit`、`git checkout`、`git reset` 或 `git stash` 管理工作区。
- 不擅自执行 `jj abandon`、`jj restore`、历史重写或远端推送；尤其不得覆盖或丢弃用户已有修改。
- 默认不创建 bookmark、不推送；确有发布或协作需求时遵循用户授权。
- 文档和实现保持小步、可审阅的变更，描述说明实际目的，避免夹带无关重构。

## 验证与交付

- 根据改动执行必要检查。建立 Rust workspace 后，常规基线为 `cargo fmt --all -- --check`、`cargo check --workspace` 和相关测试。
- 对取消、过期结果、空值排序、嵌套路径和缓存边界等行为编写有价值的测试；纯布局调整优先实际运行检查。
- UI 验证覆盖空数据、加载、错误、取消、宽表、长文本和嵌套数据。自动检查通过不等于视觉验收通过。
- 未运行的检查必须明确说明，不能报告为通过。
- 改变架构、阶段范围或已确认选择时同步更新 `dev.md`。未经用户要求，不扩大到数据编辑、云端服务或完整数据库管理功能。

## 代码文档

- 每个结构体、枚举和具名函数都必须有准确的英文 `///` 文档注释，包括私有项、trait 方法、trait 实现和测试辅助函数。
- 说明职责、参数与返回值的含义；有实际影响时说明错误、取消、阻塞、所有权、状态变化和资源边界。简单访问器简明说明含义，复杂生命周期方法详细解释契约，不仅重复名称。
- 为状态字段和枚举分支说明具体语义，明确区分源数据、保留结果、分页范围、未知总数与用户输入草稿。
- 注释必须描述当前实现，不能把未来计划写成已有能力；改变行为时同步修改相关文档。
- 文档改动使用 `cargo doc --workspace --no-deps --document-private-items` 检查生成结果；必要时以 `RUSTDOCFLAGS="-D warnings"` 检查文档警告。


## 开发命令与严格检查

- 使用 `just dev` 启动开发程序，`just test` 运行测试，`just lint` 执行 Clippy，`just ci` 执行完整本地检查；`just` 列出所有命令。
- 项目代码和测试均禁止调用 `.unwrap()` 与 `.expect()`。通过 workspace Clippy lints 强制执行，各 crate 必须继承 `[lints] workspace = true`，不得用局部 allow 绕过。
- Clippy 使用 `--workspace --all-targets -- -D warnings`，所有警告视为错误。可恢复错误通过 Result 传播或明确处理，不能只将 unwrap / expect 改写为 panic。

- Workspace Rust lints 禁止 `unsafe_code`（forbid）、`unused_qualifications` 和 `unreachable_pub`（deny）；UI 内部共享项使用 `pub(crate)`，不通过 allow 绕过。
