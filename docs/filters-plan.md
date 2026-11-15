# Browse 类型化 Filters 实施方案

- 状态：阶段 1–5 已实现，自动验收完成；原生视觉、跨平台和 GB 级文件手工验收待执行。
- 日期：2026-11-15；实现更新：2026-11-16。
- 专用 bookmark：`feature/filters`。
- 每个阶段使用独立 jj change；阶段完成后将 bookmark 移到该阶段的 change，不自动推送。

## 1. 范围与语义

保留 Quick Search，新增独立 Filters 构建器。第一版仅支持顶层标量字段、AND 连接、最多 32 个条件。复杂 OR、嵌套字段和表达式继续使用 SQL。

执行顺序：完整源数据 → Quick Search 与全部 Filters → 全局排序 → Offset / Limit。

- Quick Search 是所有可搜索文本列的大小写不敏感子串 OR 搜索；包含 dictionary 解码后的文本类型。
- Quick Search 在 Apply 时 trim，纯空白表示不搜索。编辑时保留原始输入。
- Filters 的文本值不 trim，允许精确匹配空字符串和前后空格。
- contains / starts with / ends with 大小写不敏感；equals / not equals 大小写敏感。
- `%`、`_`、引号与反斜线都是普通输入字符，不解释为 LIKE 模式。
- `is null` 仅匹配 SQL NULL；`is not null` 包含空字符串；`equals ""` 仅匹配空字符串。
- 普通比较遵循 SQL NULL 语义：NULL 不通过比较条件。`!= "x" AND IS NOT NULL` 仍不包含 NULL；包含 NULL 需要 OR，第一版不支持。
- 无可搜索文本列时禁用 Quick Search，并显示「该文件没有文本列」。后端收到非空搜索仍返回明确的能力错误，不使用 WHERE false 隐藏这一情况。
- SQL 请求不读取或校验 Browse search/filters/sort/range，仍只接受现有单条 SELECT/WITH 契约；Mock SQL 仍限于明确预设。

## 2. 架构与数据契约

### Core：模型、类型能力与校验

新增 `crates/rowglide-core/src/filter/`，集中定义条件模型、Arrow 类型分类、解析、验证与规范化。按实际体量组织为模型、解析与 Mock 求值三个模块。

主要契约：

- `Query.filters: Vec<FilterCondition>`：已解析、已应用的 Browse 条件。
- `FilterCondition::Compare { column, op, value }` 与 `IsNull { column, negated }`：NULL 判断不携带无意义的值。
- `CompareOp`：Equals、NotEquals、GreaterThan、GreaterThanOrEqual、LessThan、LessThanOrEqual、Contains、StartsWith、EndsWith。
- `FilterValue`：Text、Integer(i64)、Unsigned(u64)、Float(f64)、Boolean，以及精确保留值的 Decimal、Date、Timestamp。
- Decimal、Date、Timestamp 在边界处严格解析；规范化后的模型保留精确值和必要类型信息，不通过 f64 或本地语言日期格式转换。
- `FilterError`：类型化的列无效、操作符不支持、值无效、溢出、精度损失、条件过多和 schema 过期等错误。包含条件位置/字段上下文，供 UI 定位并本地化。
- 共享 `classify / supported_operators / parse_value / validate_filters` 能力，供 UI、DuckDB 和 Mock 使用；后端必须独立验证请求，不信任 UI。

源 schema 独立于当前结果：`Source` 保存注册 schema，与 DuckDB `FileSource` 共享 Arrow schema 引用；SQL 结果可以拥有完全不同的 schema。

- 注册时的 `SchemaRef` 暴露在 `Source`，零行文件也能构建条件；不以当前表格、SQL 结果或展示用 type_label 推断类型。
- 加入明确的源 schema 修订标识，并由 Browse 请求携带；重载后拒绝旧修订，防止列索引仍合法却指向另一字段。
- Source id、schema 修订和 UI 请求 generation 各司其职；不得将 schema 修订当作查询请求序号。
- 重载成功清空旧 schema 对应的条件和草稿；关闭 tab 释放对应状态。
- Core 不引用 GPUI、不保存输入框 Entity、不解析用户 SQL 来实现 Filters。

### 类型规则

| Arrow 类型 | 支持的条件 | 第一版输入与精度策略 |
| --- | --- | --- |
| Utf8 / LargeUtf8 / Utf8View | contains、starts with、ends with、=、!=、NULL 判断 | 原始文本，允许空字符串，不 trim |
| Int8/16/32/64 | 六种比较、NULL 判断 | 完整整数解析，验证目标位宽范围 |
| UInt8/16/32/64 | 六种比较、NULL 判断 | 独立 u64 路径，拒绝负数，验证目标位宽；覆盖大于 i64::MAX 的值 |
| Float32/64 | 六种比较、NULL 判断 | 输入必须为有限数，拒绝 NaN/Infinity 和溢出；源数据非有限值的比较以锁定 DuckDB 版本为准，Mock 通过对照测试保持一致 |
| Decimal | 六种比较、NULL 判断 | 十进制精确解析，按 schema 的 precision/scale 校验，不接受隐式舍入或科学计数法；无损补零可规范化 |
| Boolean | =、!=、NULL 判断 | 值选择器显示「是/否」；不要求输入 true/false。暂不重复提供等价的 is true/is false 操作符 |
| Date32 / Date64 | 六种比较、NULL 判断 | 严格 YYYY-MM-DD，验证实际日历日期；Date64 按日期语义处理 |
| Timestamp（无时区） | 六种比较、NULL 判断 | 严格 YYYY-MM-DDTHH:mm:ss[.fraction]，不接受时区后缀，不使用系统时区补全 |
| Timestamp（带时区） | 六种比较、NULL 判断 | 必须有 Z 或明确 UTC offset，规范化为 UTC 瞬间；不推断本地时区或 DST |
| Dictionary | 跟随解码后的值类型 | 递归分类、校验与解码，不作为 Map |
| List / Struct / Map 及其他暂不支持类型 | 不提供第一版条件 | 显示能力说明；嵌套字段引导使用 SQL |

Decimal256、负 scale、特殊 timestamp 单位/时区组合等必须先核对真实注册 schema 和锁定引擎的无损映射。超出引擎精度或无法精确表达的组合应返回明确错误/禁用提示，不能降级为浮点或静默丢弃小数秒。特别测试纳秒时间与带时区时间的精度边界。

### DuckDB：参数化下推

在 `duckdb/sql.rs` 保留查询分支和组合入口，将有体量的条件编译放入私有 `duckdb/filter.rs`。

- Quick Search 的 OR 项先用括号整体包裹，再与每个 Filters 条件 AND 连接。
- 列索引校验后，经 `identifier()` 引用源字段名；类型和 CAST 目标只能由受信任 schema 映射生成。
- 逐条件同步追加 SQL 与参数，保持顺序一致；所有用户值均参数绑定。
- 文本子串/前缀/后缀使用对应函数及 lower；精确比较直接比较原值，不使用 LIKE。
- NULL 使用 IS NULL / IS NOT NULL，无绑定参数。
- 整数使用匹配符号范围的绑定类型；Decimal/Date/Timestamp 经严格验证后使用精确类型参数或显式 CAST。
- 既有 Limit/Offset 参数在全部过滤参数之后追加，过滤发生在排序与分页之前。
- UI 校验之后，后台仍校验 schema 修订、列、操作符和值；直接构造非法 Query 也必须失败。

### Mock：同一规则，独立执行

Mock 不能继续通过 name/region 的特例承担新筛选语义，也不引入第二个真实引擎。

- 在完整模拟源的 Arrow 批次上，按共享类型规则计算行匹配，再全局排序、分页；不只筛选当前页。
- 覆盖模拟源的所有可筛选字段，dictionary 按值类型解码，明确处理 NULL 三值逻辑。
- 只增加谓词求值器，不实现通用 SQL 解析器。
- 修正目前 Mock 在识别 SQL 预设前先处理 Browse search/sort 的执行顺序，使 SQL 路径在入口就独立。
- 使用同一组小型 Arrow 测试数据进行 DuckDB 与 Mock 对照，覆盖 unsigned、decimal、时间、NULL、非有限浮点和 Unicode；对照测试证明语义，不仅比较 SQL 字符串。
- 不直接假定 Rust lowercase 与 DuckDB lower 对所有 Unicode 都一致；阶段验证明确大小写转换范围，发现差异时在实现中统一并加入回归数据。

## 3. UI 与状态

新增 `workspace/filters/` 管理条件草稿、控件及面板交互，沿用 gpui-kit，不引入另一套组件库。

工具栏保持紧凑：

```text
[Filters (2)]  Limit [1000]  Offset [0]  [Apply] [Reset]     [File info] [SQL]
```

点击 Filters 后在工具栏下展开面板；Quick Search 移入面板，作为独立于条件表的快捷搜索入口。Cmd/Ctrl+F 在 Browse 模式展开面板并聚焦 Quick Search；没有文本列时展示说明，不将焦点交给禁用控件。

```text
Quick Search  [Search all text columns…]

Column                 Operator           Value
city · Text            contains           [London]       [×]
amount · Decimal       >=                 [100]          [×]
note · Text            is null                          [×]

[+ Add condition]  [Clear filters]                 [Cancel] [Apply]
```

- 列选择器只列出支持的顶层标量列，显示名称和类型。面板说明嵌套字段使用 SQL；无支持列时禁用 Add condition。
- 操作符随所选列变化；改变列/操作符时保留兼容草稿，移除不再适用的值状态，不隐式应用。
- 数值输入保留原始字符串，以便表达尚未完成的负号、小数等编辑状态；不要将输入控件的浮点结果作为真实值。
- 布尔使用选择器，日期时间显示格式与时区提示，NULL 操作符不显示值控件。
- 每条草稿有稳定本地 ID，删除/插入不复用其他行的输入状态。
- 面板有高度上限并滚动，最多 32 条；重用现有焦点遍历、可访问名称和中英文翻译。
- Filters 数字为已应用条件数；草稿另标记「未应用」，避免把草稿误报成当前结果条件。
- Applied 摘要显示已应用条件，正确表示引号、空字符串和 NULL；长值摘要截断但完整内容可查看，不使用原始 SQL 作为用户说明。
- 状态栏显示过滤条件数和现有行范围，不声称知道匹配总数。
- 空结果区分：源文件为空、搜索/筛选零匹配、指定 Offset 超出当前结果范围。Offset > 0 的空页不能宣称全源没有匹配行，可引导返回第一页。

### Draft / pending / applied 生命周期

- `TabState.browse` 保留最后成功应用的 Query。
- `BrowseDraft` 扩展为 search、条件原始草稿、Limit 和 Offset；每个 tab 保存草稿及面板展开状态，切换 tab/模式后恢复。
- pending Browse 请求由 workspace 候选 Query 与后台闭包内的候选/草稿/请求标识快照持有；不可先覆盖 applied Query 再清空旧表格。
- Apply 先对所有条件、Quick Search、Limit/Offset 和 schema 做完整校验。失败时保留旧结果、旧 applied 状态和可编辑草稿，不启动数据查询。
- 搜索/条件相对 applied 发生变化时将候选 Offset 归零；只改 Limit/Offset 时尊重用户输入。规范化后等价的数值/搜索不误判为条件变化。
- 后台成功得到完整 Browse 页后，原子更新 Query、结果、页信息和 applied 摘要；取消、执行失败或过期响应均保留旧成功结果，并明确显示本次未应用。
- pending 时暂停分页；编辑尚未应用或校验无效时同样暂停分页。分页只使用 applied Query。
- Apply 期间继续编辑的内容不得被完成回调覆盖；比较提交时的完整草稿快照（包含稳定行 ID）判断是否可回写规范化值。
- Clear filters 只清空条件草稿，保留 Quick Search，点击 Apply 后生效。
- Cancel 恢复当前 tab 的整个 BrowseDraft 到 applied 状态，包括 search、filters、Limit/Offset，并关闭面板；它不是查询取消按钮。
- Reset 清空 search/filters 并恢复默认 Limit/Offset，通过同一校验/提交路径加载第一页。
- 排序保留 applied 条件并将 Offset 归零；未应用草稿不参与排序，也不能被排序回调覆盖。
- SQL 状态和执行路径不参与上述 Browse 提交流程；源文件重载成功时重建源 schema 和 Browse 状态。

## 4. 实现阶段与验收

### 阶段 1：共享契约、源 schema 与解析

- [x] 增加条件/值/操作符/错误类型、32 条上限、Arrow 能力分类和严格解析。
- [x] 为 Source/Query 接入源 schema 与修订标识，调整 DuckDB/Mock 注册和重载契约。
- [x] 测试整数边界及 UInt64 大值、精确 Decimal、有效/无效日期、时区/精度、dictionary、嵌套类型和非法组合。
- [x] 用锁定 DuckDB 版本的小型探针确认 Decimal、Timestamp 和非有限浮点策略；确定所需直接依赖后再引入，优先复用现有生态依赖。

### 阶段 2：DuckDB 参数化筛选

- [x] 编译 Quick Search 分组及全部 AND 条件，接入真实查询路径。
- [x] 使用真实文件测试各类型、空字符串/NULL、空格、Unicode、特殊标识符、注入式输入、参数顺序和过期 schema。
- [x] 证明筛选先于排序/分页、无文本列明确报错、SQL 不受 Browse 参数干扰。

### 阶段 3：Mock 语义与后端对照

- [x] 实现完整模拟源的类型化谓词计算和所有文本列搜索。
- [x] 独立 SQL 预设入口，维持全局筛选/排序/分页顺序。
- [x] 对同一组测试数据验证 Mock 与 DuckDB 的匹配行集合和分页结果一致，包含取消边界。

### 阶段 4：Browse 原子提交与每 tab 草稿

- [x] 扩展草稿模型及 pending 请求，保留旧结果直至新的完整页成功。
- [x] 实现条件变化归零、手工 Offset、Reset/Cancel/Clear、排序和分页规则。
- [x] 测试无效输入不派发请求、执行失败/取消保留旧页、切换 tab/模式、编辑期间完成、旧请求及源重载。

### 阶段 5：可展开 Filters 面板

- [x] 接入列/操作符选择、类型化值控件、增删/清空/应用/取消和条件数量限制。
- [x] 接入 Quick Search、禁用说明、Applied 摘要、条件计数和精确空结果提示。
- [x] 加入英文/中文、焦点遍历、键盘行为和可访问名称；保证窄窗口和多条件下布局有界。
- [x] 用 GPUI headless 事件测试新增条件、选择类型、校验错误、应用、Cancel、分页、模式隔离和快捷键焦点。

### 阶段 6：整体验收与记录

- [x] fmt、workspace check/test、严格 Clippy、包含私有项的文档检查。
- [x] 使用 20 万行真实 Parquet 验证稀疏过滤、排序/分页及取消；其余自动测试覆盖空文件、无文本列、特殊列名、宽表、嵌套能力与 schema 重载。
- [ ] GB 级真实文件耗时、内存和滚动体验手工验收。
- [x] 验证现有 SQL、文件拖放、最近记录、变更检测、复制和元信息行为。
- [x] 更新 README、changelog.md 与本地 dev.md，将计划项改为实际能力；不提前宣称功能已完成。
- [ ] 原生窗口视觉与跨平台验收；本轮未使用 Computer Use。

## 5. 实现记录

- Core：`filter/{mod,parse,evaluate}.rs` 集中模型、能力、严格解析和 Mock 求值；`Source.schema/schema_revision` 与 `Query.filters/schema_revision` 明确源列身份。
- DuckDB：`duckdb/filter.rs` 同步编译谓词与绑定参数，`sql.rs` 保留 Browse/SQL 分支。后端重新校验；源重载拒绝过期请求。
- UI：`workspace/filters/{mod,render,tests}.rs` 管理面板、输入与交互；`state.rs` 保留各 tab 草稿，`query.rs` 在成功时提交候选页面，未成功前保留旧页。
- 首版能力边界：Float16 禁用；Decimal 支持精度 1–38、非负且不超过 precision 的 scale；带时区时间最多微秒，无时区按源单位保留至纳秒。非法精度在 UI 提交和后端共同校验。
- Mock 浮点比较读取原始 Float32/64，再按 DuckDB 参数提升规则比较；不通过展示字符串丢失有效精度。Dictionary 解码 NULL 与键 NULL 均按 SQL NULL 处理。
- 测试包含 UInt64 最大值、Decimal 大数、纳秒和时区、空字符串/NULL、Unicode、引号列名、注入式文本、参数顺序、SQL 隔离、完整 Mock 源、20 万行稀疏 Parquet、取消及 GPUI headless 控件/焦点/草稿生命周期。
- 130 项 workspace 测试通过；已完成 fmt、workspace check、严格 Clippy 和 private-item 文档检查。没有进行原生窗口、屏幕阅读器、跨平台及 GB 级文件手工验收。
- 按阶段保留独立 jj changes，bookmark 为 `feature/filters`；未推送远端。
