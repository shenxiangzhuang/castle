# 核心架构：SDK、Harness 与交互层

Status: accepted / implemented。本文件是层次边界、Public API 和设计取舍的唯一总纲。
下面代码对应当前接口；完整字段和错误类型以各 crate 的定义为准。

## 层次与所有权

```text
Desktop / CLI → harness → agent（SDK）
```

依赖单向向下。更换交互层不需要修改 harness 或 SDK，不使用全局事件总线。

| 层 | 拥有 | 不拥有 |
| --- | --- | --- |
| SDK | 领域状态、事件校验与重放、轮次推进、上下文、压缩与恢复规则 | 数据库、网络客户端、工具进程、异步任务、UI |
| Harness | session 运行 owner、命令串行化、持久化、模型连接、工具执行、审批策略、取消与收尾、共享产品配置 | 渲染、输入草稿、选中项 |
| Desktop / CLI | 用户意图、审批交互、事件展示投影、界面偏好 | 可变核心状态、写入权限、运行协调 |

SDK 的“静态”指不绑定环境。审批合法性由 SDK 校验，策略由 harness 执行，用户决定由交互层收集。
每个 session 的运行只能持有一个 writer capability，跨 handle 和进程的冲突由存储写入权限拒绝。
内部 `Agent` 在空闲 owner 与执行任务间转移所有权；没有共享可变状态或第二个领域校验器。

## 层内模块

目录按职责归属组织，纯计算与执行/渲染边界在模块内保持。模块入口只重导出必要接口；
实现默认私有，内部协作用 `pub(super)` 或 `pub(in ...)`，不使用通配重导出扩张公共 API。

| 层 | 模块 | 职责 |
| --- | --- | --- |
| SDK | `session/{event,machine,transition,tree}` | 事件、校验/重放、候选转换、会话树 |
| SDK | `context` | 上下文构造与压缩规划 |
| Harness | `runtime/{protocol,handle,owner}` | 客户端契约、连接与关闭、唯一命令 owner |
| Harness | `runtime/execution/{model,tools,commit,compaction,control}` | 同一执行 owner 的内部实现；提交与恢复集中在 `commit` |
| Harness | `session/store`、`model`、`tools` | 会话存储、具体模型连接与工具实现 |
| Harness | `config`、`project`、`app_store` | 共享产品配置、项目目录与应用数据 |
| Desktop | `bootstrap`、`app` | 启动/退出、窗口状态、跨功能导航与命令转发 |
| Desktop | `session` | Harness 连接和唯一会话展示投影，供 Chat / Trajectory 共用 |
| Desktop | `chat`、`trajectory`、`workspace`、`settings` | 各功能的界面、交互与局部展示实现 |
| Desktop | `rendering`、`platform` | 共享渲染原语、操作系统集成 |

Harness 的可变执行上下文与控制能力只在 `runtime` 子树可见。Desktop 直接通过
`harness::config` 等入口读取共享配置，不设置同名转发文件。
Desktop 的 GPUI window entity 仍是 `DesktopApp`：功能文件中的 `impl DesktopApp` 是同一实体的
界面实现，不代表独立服务或独立生命周期。`app/connections` 保留跨会话选择与连接缓存协调；
本次结构整理不拆分 Entity、任务、订阅或重新分配可变状态。
`rendering` 不依赖功能模块或 `DesktopApp`，HTML 预览因参与聊天命令和窗口交互而属于 `chat`。
纯投影、布局、公共渲染与 crate 依赖边界由 Desktop 的 `architecture_tests.rs` 检查。

## Public API

### SDK：状态与输入产生候选转换

入口见 [transition.rs](../../crates/agent/src/session/transition.rs) 和 [machine.rs](../../crates/agent/src/session/machine.rs)。
时间和关联 ID 由宿主提供，转换不读取时钟、不执行 I/O：

```rust
let transition = state.transition(
    AgentInput::Start {
        input: Some((input_id, text)), selected_head: None,
        run: run_id, turn: turn_id, step: step_id,
    }, tx_id, observed_at,
)?;
let (batch, effect) = transition.into_parts();
// 宿主持久提交 batch.events() 并验证回执后：
state.apply_batch(batch)?;
match effect {
    AgentEffect::RequestModel { step } => { /* 宿主执行模型请求 */ }
    AgentEffect::Finished => { /* 宿主收尾 */ }
}
```

`AgentInput::StepCompleted` 决定 steer、工具续轮、queue 和结束的优先顺序。
流式观察由 `plan_batch` 校验，取消/失败由 `plan_termination` 原子关闭，恢复由 `plan_recovery` 规划；
它们共用同一个 `SessionMachine`。
候选批次绑定来源状态，不能用于另一状态或旧版本。SDK 不承诺持久化，也不认识 SQLite 回执。
不额外复制一套 State/Transition 校验器或 provider DTO。

### Harness：命令产生事实，连接提供快照

入口见 [runtime.rs](../../crates/harness/src/runtime.rs)。构造和连接是普通方法，修改行为是类型化命令：

```rust
let harness = Harness::default();
let setup = SessionSetup::new(model, instructions, session, cwd);
let handle = harness.attach(setup, project_id, Some(sessions_dir), config);
let mut connection = handle.connect()?; // 原子快照与后续订阅
let command_id = TxId::random();
handle.send_with_id(command_id, SessionCommand::Submit {
    id: InputId::random(), text, mode: SubmitMode::Start,
}).await?; // 返回代表输入已持久接受
```

`SessionSetup` 只有构造配置，不可启动运行。`SessionCommand` 包含提交/排队/steer、编辑、Fork、
审批、配置、连接刷新、重命名、压缩、停止和恢复 pending 输入。
`SessionUpdate` 只有 `Committed`、`Changed` 和带关联 ID 的 `CommandResult`。
SDK 可变状态、事务写入权限、任务句柄不暴露给交互层；共享领域数据类型选择性重导出。
模型凭据是连接配置，命令账本仅记录模型描述和语义配置，不复制密钥。

### Desktop / CLI：投影事实与发送意图

```rust
view.replace(connection.snapshot);
loop {
    match connection.events.recv().await {
        Ok(SessionUpdate::Committed(batch)) => view.apply(batch),
        Ok(SessionUpdate::Changed(snapshot)) => view.set_runtime(*snapshot),
        Ok(SessionUpdate::CommandResult { id, result }) => view.acknowledge(id, result),
        Err(RecvError::Lagged(_)) => {
            connection = handle.connect()?;
            view.replace(connection.snapshot);
        }
        Err(RecvError::Closed) => break,
    }
}
harness.shutdown().await;
```

GPUI `SessionConnection` 只保留展示文档、观察任务和等待确认的交互状态。输入确认与当前选中的会话无关。
`Harness` 持有应用级取消和任务跟踪；应用的 Quit 动作先等待收尾再调用平台退出。
系统终止通知也会触发关闭回调，但受 GPUI 的 200 ms 退出期限约束；强制终止仍依赖 journal 恢复。

## 执行与消息契约

1. **规划 → 提交 → 安装候选状态 → 发布事实 → 执行副作用**。提交失败不推进核心、不启动 effects。
   模糊的 journal 提交结果使用原事务 ID 查询；已派发但结果未知的工具保留 `UnknownSideEffects`，不自动重试。
2. `try_send` 只确认进入有界队列，返回的 receiver 等待语义结果；`send` / `send_with_id` 直接等待结果。
   Submit 成功表示输入被持久接受，Stop / Shutdown 成功表示任务已 join。领域终止事件后仍处于 Settling。
3. 命令先持久预留 ID，结果持久化后再确认。同 ID、同语义内容返回原结果，不同内容拒绝。
   预留与具体操作是两个事务：中间崩溃会留下 **outcome unknown**，该 ID 不会自动重执行。
   这选择了避免重复副作用，放弃此窗口中的透明自动重试；调用者必须检查会话事实后再决定下一步。
4. 审批和停止绑定随机运行标识，从 Creating 到 Settling 保持不变；旧运行的决定不能影响新运行。
   策略先串行持久化再生效，不撤销已派发工具。模型与推理配置仅在空闲时改变。
5. 快照读取、订阅建立和发布共用同一短锁。公共命令队列和观察队列有界；慢观察者得到明确 Lagged，
   必须从快照重连。观察者断开不取消执行。历史是共享持久向量，不为每条快照复制整段历史。
6. 重启不自动消费 pending 输入；恢复执行需要命令。运行配置、输入、授权和副作用意图持久化，展示状态不入日志。

## 取舍与验证

| 选择 | 收益 | 代价 |
| --- | --- | --- |
| 无 I/O SDK | 可确定性测试和重放 | SDK 单独不能执行请求 |
| 每 session 单写者、先提交后执行 | 状态与恢复边界明确 | 提交串行、增加持久化延迟 |
| 命令与事件隔离 UI | GUI / CLI 共用运行行为 | UI 必须处理异步确认、拒绝与重新同步 |
| 持久命令预留与结果 | 跨重启去重 | 崩溃窗口返回未知，账本随会话库增长 |
| SQLite、Tokio、Responses 数据类型 | 少抽象与转换 | 不承诺远程执行、多存储或任意模型协议 |
| 三个内部 crate，均 `publish = false` | 编译期依赖边界 | 不提供独立稳定的外部 crate API |

发布工作流只构建 desktop 资产。SQLite schema 3 从 schema 1/2 原地增加命令账本，不改写事件历史。
不同时更换模型协议、工具实现或会话事件格式。代码减少来自删除 UI 重复协调，不以总行数为验收标准。

无 GPUI 的 [harness 客户端测试](../../crates/harness/src/runtime/tests.rs) 覆盖提交、审批、取消、恢复、
断连、溢出重连、失败配置、重开去重和 shutdown；既有 journal/工具/展示回归继续运行。
[TLA+ harness 模型](tla/harness-connection/README.md) 检查命令预留、快照边界、溢出和 join 安全性；
模型通过不等于证明 Rust 实现正确。

[Session](session.md) 定义领域事务与恢复，[Desktop](desktop.md) 定义展示投影，
[App storage](app-storage.md) 定义数据布局，[Conversation tree](conversation-tree.md) 定义分支与 Fork。
