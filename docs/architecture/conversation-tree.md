# 对话树、消息编辑与 Fork

Status: accepted. 会话事件日志保持 append-only；树、当前 head 和关系索引均由日志投影。
层次与 Public API 遵循 [核心架构](overview.md)；相关职责见 [Session](session.md)、[Desktop](desktop.md)，安全模型见
[Conversation tree TLA+](tla/conversation-tree/README.md)。

## 用户行为

- 编辑仅作用于用户消息：从该消息的父节点继续，在**原 session** 中提交新分支并发送新输入。原分支留在日志中；当前对话只显示新路径。
- Fork 只显示在已完成且安全的助手回复下。子 session 继承至这条回复，打开时输入框为空，不自动发送。新标题在完整源标题后追加同项目内最小可用的 ` (1)`、` (2)` 等后缀；活动和归档标题都参与判重。
- 顶部仅在存在跨 session 关系时显示父、子或组合图标；本地编辑不显示图标。弹出列表支持多个父/子项，显示相对时间和归档标识。点击关系按目标当前 head 打开；归档目标进入归档页，须显式恢复。不可用的父项保留保存的标题但不能跳转。
- 子会话在继承消息与本地消息之间显示一次 `Continued from chat` 链接；空子会话显示在输入框上方。链接指向直接父 session，包括已归档的父 session；父 session 不可用时隐藏。
- 不提供树浏览器、分支预览、继承标记或 Fork 来源横幅。编辑暂存普通草稿，取消或提交成功后恢复；失败时保留编辑内容。切换会话时草稿按 session 保存。

## 日志与路径

`ConversationTree` 为 `InputAttached`、`RequestSnapshot`、`ToolResultAttached` 和
`CompactionStarted` 建立稳定的 session 局部节点；节点父指针一经提交不可修改。
当前路径由 head 沿父指针回溯得到，不能按物理 `seq` 截断日志。
`ConversationHeadSelected { head }` 只改变当前路径，不修改旧节点。
`SessionMachine` 是唯一语义校验者，并为节点保留可重建的共享上下文检查点；切换路径时
恢复对应上下文，包括压缩边界，不继承离开分支的模型消息。
模型、权限与工具配置仍取 session 的当前配置，历史工具副作用不重放。

分支操作须在 writer 下重新加载并验证 `expected_revision` 与 `expected_head`。
只有无活动运行、未决工具、恢复任务和 pending 输入的可写 session 可以修改 head 或 Fork。
编辑在一个事务中提交 head 选择、新输入及 run/turn/step 建立与附加；提交结果不明时
用同一操作 ID 核实，不能另建分支盲目重试。运行 owner 确认提交后发布事件并发起模型请求，
界面消费已提交事件重建路径。低层显式选头只提交选择事件，不发送消息。崩溃恢复不能重执行历史工具。

## Fork 存储

Fork 与源 session 共用项目和工作目录，但拥有独立日志。子 session 的首个
`SessionForked { origin, events }` 事件包含选定路径的自包含证据：消息、已落定请求与工具
结果、压缩记录及使用量；不含其他分支、pending 输入或外部执行句柄。
导入事件重新编号并合并事务；若截取点留有未闭合的运行边界，使用纯恢复规划器补齐，
再由 `SessionMachine` 校验。继承节点在子 session 内重新分配节点 ID，随后可正常编辑与压缩。
来源引用记录 session、revision、点击位置和继承 head；源 session 删除或归档不影响子会话重放。

单个 SQLite 事务校验源 revision，并写入子元数据、seed、搜索与来源索引。
稳定的子 session ID 使模糊结果重试幂等；源 head 和 revision 不变。
标题编号也在此事务内分配。提交成功才发布子会话；桌面异步回调通过稳定 `ProjectId`
重新定位项目，并以选择 generation 防止过期结果抢走当前会话。

## 投影与兼容

`SessionDocument` 保留全部事实，Chat 与 Trajectory 显示当前路径；路径变化重建视图，
普通追加增量更新。重建显示路径时须额外保留本 session 尚未附加的 `InputSubmitted`
和 `InputPrioritized`，以便重启后取消、优先发送或附加；这些输入不进入 Fork seed。
搜索覆盖当前路径和 pending 输入，不混入旧分支。底部消耗统计累计本 session 自身
所有分支的请求，排除 Fork 导入的使用量；路径上的历史详情仍可显示导入用量。
关系列表由 catalog 查询，归档会话保留直接关系但不出现在普通侧栏。

SQLite schema 2 增加来源索引，schema 3 增加命令账本；支持从 schema 1/2 升级，不改写原日志。
事件/JSONL 格式为 4、搜索提取器为 2、机器语义为 2。旧客户端不得忽略新事件后
将多分支混成一个模型上下文。JSONL 导出保留全日志，可独立重建子 session。

## 验证边界

Rust 测试覆盖树重放、上下文与工具配对、原子编辑、Fork 幂等及源删除、归档关系、
搜索和统计；桌面测试覆盖草稿、导航、重新打开 pending 输入和异步项目变化。
[TLA+ 模型](tla/conversation-tree/README.md) 检查有界树、路径隔离、原子编辑/Fork、
回执发布和崩溃恢复；队列、工具、压缩及界面细节由相应模型和 Rust 测试验证。
