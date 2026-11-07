# 常驻进程生命周期

CodexMonitor 在 Windows、macOS 和 Linux 的交互式桌面会话中只运行一个普通用户权限进程；
不支持其他桌面操作系统。Tauri event loop、单实例、原生托盘、通知、自启动、浏览器打开
和平台清理属于 `codex-monitor-desktop`。可纯测试的启动解析、`HostCoordinator`、托盘视图
模型与生命周期状态机属于无 Tauri 的 `codex-monitor-host`。`ResidentEngine` 拥有 storage、
listeners、workers 和有序关闭。

## 启动方式

- 手动首次启动：创建常驻进程，等待 engine `Ready`，签发新的单次 Dashboard bootstrap URL，并交给默认浏览器。
- `--background`：启动相同常驻服务，不打开浏览器。登录自启动只登记这个参数。
- 手动第二实例：单实例回调把“打开 Dashboard”转发给已有进程，自身不获取第二份 engine 所有权。
- 转发的后台启动：忽略，不重复启动 engine。
- `--quit`：转发到已有进程并进入同一个幂等关闭路径。

所有命令先解析为闭合 `HostCommand`，再由 `HostCoordinator` 调用 `ResidentHostPort`。参数形状不受信任时直接拒绝。携带 credential 的 URL 使用 `Debug` 隐去内容的类型；browser opener 错误在进入诊断前折叠为稳定代码。

## Desktop setup

Tauri setup 首先把构建期 `frontendDist` 转换为有界、不可变的 `DashboardAssets`。缺少 `index.html`、路径不规范、MIME 非法、条目重复或 bundle 超限会在打开 storage 之前终止 setup。运行时不读取安装目录或工作目录中的展示文件，也没有 development server fallback。

随后创建一份 namespaced tray 和菜单。初始视图显示 `starting`，engine ready 之前只有 Quit 可用。Linux 无法创建 AppIndicator 时把 tray capability 设为 `unavailable`，监控服务继续启动；Windows 和 macOS 在获取 engine 资源前终止 setup，避免留下没有原生控制面的不可见进程。

系统通知 adapter 在启动时执行不显示通知、不请求权限的探测。Windows 每次启动先按当前
便携 EXE 路径自校准通知身份，再检查身份；Linux 检查用户会话通知服务，macOS 读取
`UNUserNotificationCenter` 设置。Windows EXE 移动或重命名后，从新路径手动启动即可恢复
通知身份。结果只发布 `available`、`permission_required` 或 `unavailable`。

## Engine ready 条件

`ResidentEngine` 只在以下顺序全部成功后发布 `Ready`：

1. 解析当前用户数据根，验证并应用嵌入式 SQLite migrations，启动唯一 writer actor。
2. 获取唯一 `SettingsRuntime`，写入首次运行语言，并根据持久 opt-in 协调登录自启动。可选原生 adapter 不可用时只关闭对应 capability。
3. 登记内置规则语义，使用一个 recovery timestamp 分批处理遗留 `started` actions，不调用 executor。
4. 验证已登记 action executor，构造 OTLP 来源并启动固定 loopback receiver。
5. 启动受监督的规则、action Outbox 和定时 retention worker。Retention 启动时执行一次，之后按小时或设置变化执行有界 transaction。
6. 使用已验证的展示 bundle 构造 authenticated Dashboard state，绑定 Dashboard loopback listener。

任何部分启动失败都会关闭已获得的所有资源。Dashboard bind 失败时先停止已经启动的 receiver 和 workers，再关闭 writer。稳定的 startup category 不携带路径、SQL 或原生错误文本。

## 启动取消

Quit 可以与启动并发。Migrations 和已经取得的 durable transaction 会原子完成；settings 协调、action recovery batch、receiver、worker 和 Dashboard bind 之间都有取消检查点。

Ready 与 cancellation 竞争同一个原子 phase claim。Cancellation 获胜后不会短暂发布 `Ready`；系统在下一处安全所有权边界停止创建资源、回滚已取得资源并发布 `Cancelled`。Ready 先获胜时，关闭请求转入正常 engine shutdown。

## 常驻状态与托盘

Ready 后，一个 presentation task 通过 `watch` monitor 观察 durable settings、OTLP receiver bind state、worker state和平台 capability，不轮询。它更新状态行、tooltip、菜单启用/勾选状态和 `en-US`/`zh-CN` 文案。

OTLP 端口冲突是非致命状态。Open Dashboard、Settings 和 Quit 继续可用，重试入口只在 receiver 报告支持时出现。Linux 无法替换已挂接菜单，因此语言变化通过保留的 menu item handle 原地更新。

登录自启动 capability 初始为 unavailable。Desktop adapter 管理完整 canonical 注册：Windows
使用带正确引号、指向当前 EXE 路径的 `HKCU` Run command，Linux 使用有界且 owner-only 的
XDG autostart 文件，macOS 使用参数分离的 owner-only LaunchAgent plist。文件注册原子替换，
每次读写都比较完整值并在写后复读。Windows 便携 EXE 移动或重命名后，通知身份可由手动
启动恢复，但登录自启动必须由用户关闭再重新开启。失败只使对应 capability unavailable。

Windows 便携 EXE 永久删除前，用户以该 EXE 运行
`--cleanup-platform-registration`，清理其平台注册；此命令不删除应用数据。

## Worker supervision

规则、action 和 retention worker 共用取消信号。任一 worker 非预期退出会取消 siblings、发布 `Failed`，并触发 host shutdown。Durable claim、fencing、recovery 与 transaction 语义保证崩溃后的工作可以安全重放或进入明确终态。

## 有序关闭

Tray Quit、`RunEvent::ExitRequested` 和平台退出事件进入同一个幂等请求。首次请求确定单调 host deadline：Windows 为 4 秒，macOS 与 Linux 为 15 秒。后续请求可以提高最终退出码，不能重启关闭或延长 deadline。

Engine 在等待任何组件前先封闭所有生产者，然后按以下顺序执行：

1. 停止接收新的 OTLP 请求并 drain receiver；
2. 在 durable work unit 边界取消并 join 规则、action 与 retention workers；
3. 停止 Dashboard listener 及 authenticated SSE streams；
4. 拒绝新的 settings mutation，等待已经取得 slot 的 mutation 完成；
5. 在所有生产者停止后 drain 并关闭 SQLite writer，join writer thread。

所有阶段共享一个 monotonic deadline。Engine 最多保留剩余预算的八分之一且不超过 1 秒，用于 abort 并 join 超时 async task；最多保留四分之一且不超过 2 秒，用于最终 writer drain、SQLite close 和 writer thread join。某一阶段失败或超时后，后续阶段仍会尝试。

Host 在独立受监督 task 中运行 engine shutdown，并额外保留 cancellation acknowledgement 尾部。Engine 所有权释放后，host abort presentation task、使排队 render 失效、在主线程隐藏 tray、移除 Tauri registry entry，并等待同一 removal state 完成。完整 host shutdown 只有在 engine owner 已释放且 tray cleanup 已确认后发布。

`RunEvent::Exit` 会在回调中同步隐藏并注销 tray，再等待已经存在或刚启动的同一 shutdown。Linux 安装 signal handler 后，第一个 `SIGTERM` 唤醒正常关闭，后续 `SIGTERM` 以 143 立即退出。Force Quit、`SIGKILL`、严重系统关闭与断电无法运行这套序列，数据一致性依赖 SQLite transaction 和 crash recovery。

关闭浏览器 tab 不参与进程所有权链，也不会停止监控。
