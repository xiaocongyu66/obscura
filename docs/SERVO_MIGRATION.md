# obscura → Servo 大换血路线图(完整版)

原则:怎么完整怎么来,时间不重要,质量优先。不搞阉割版。

## 战略选择(已定)

**B1:整个 script crate 原样搬入(Servo 的 mozjs/SpiderMonkey + 绑定生成器全保留),
obscura 转型为 Servo embedder。**

理由:
- Servo 官方有嵌入先例(components/servo,`ServoBuilder`,且 `opts.multiprocess=false`
  支持单进程),适配层有官方骨架。
- script crate 已完成依赖倒置:只依赖 `script_traits`/`net_traits`/`layout_api`/
  `servo-constellation-traits`/`profile_traits`/`devtools_traits`/`storage_traits`
  (全部 traits 层)+ `timers`/`fonts`/`metrics`/`media`。
- script crate 的 DOM 生命周期(Dom/DomRoot/JSTraceable/reflector)与 SpiderMonkey GC
  深度耦合;换 V8 = 数万行研究级重写,丢掉"完整"。
- 本机 rustc 1.97.1 与 Servo rust-toolchain 要求(1.97.1)完全一致。
- B2(V8 + 绑定重写)降级为 fallback,仅在 mozjs 在 CI aarch64 无法编译时启动。

## 源码事实(agent1 已核实,/tmp/servo b095c0f)

- ScriptThread 主循环:`components/script/event_loop/script_thread.rs` `handle_msgs`
  (:1350),消息源 `ScriptThreadReceivers`(messaging.rs:424),crossbeam 通道,
  进程内喂消息只需替换 receivers 构造(:881)。
- 消息枚举 `MixedMessage`(messaging.rs:45):FromConstellation(ScriptThreadMessage
  ~50 变体)/ FromScript。WebDriverScriptCommand 已存在(CDP 可映射)。
- Event 派发:`dom/event/event.rs`(dispatch 算法 :540-650,path/invoke/inner_invoke
  :1267/:1354),监听器回调经 mozjs CallbackInterface。
- script 加载:`dom/html/scripting/htmlscriptelement.rs` → net_traits
  (CoreResourceMsg::Fetch,fetch/fetch.rs:874)。
- 工具链:rust 1.97.1 + crown lint(support/crown,GC 安全检查,仅 dev)。

## 阶段(每阶段独立可验证)

### 阶段 1:mozjs 可编译性 + workspace 合并(最大技术风险先消除)
1.1 在独立目录试验:`cargo new` 一个最小 crate 依赖 `mozjs 0.24`(Servo 锁定版),
    aarch64 Linux 编译通过(需要 cmake/clang/yasm?查 mozjs-sys build.rs 依赖)。
1.2 把 Servo 的这些组拷入 obscura(拷贝,非 submodule,便于改造):
    components/script、components/script_bindings、components/shared/*(traits 层)、
    components/fonts、components/timers、components/metrics、components/media、
    support/servo-malloc-size-of(及 trace 相关 support)、components/xpath、
    servo_atoms!(build 依赖,在 components/shared 或 support 里找)、
    components/devtools/lib.rs 依赖的 devtools_traits。
    webgpu/webxr/bluetooth feature 直接关掉(Cargo feature 裁剪)。
1.3 合并 workspace:Cargo.toml members + workspace.dependencies(Servo 的版本锁定
    整体搬,注意与 obscura 现有依赖冲突,冲突者以 Servo 版本为准——obscura 旧代码
    逐步适配或先在独立 feature 下隔离)。
1.4 `cargo check -p script`(先不接 embedder)通过 = 阶段 1 完成。CI 加
    aarch64 依赖(cmake 等)。

### 阶段 2:embedder 骨架 + ScriptThread 进程内驱动
2.1 照 components/servo(servo.rs:1429 ServoBuilder、webview_delegate.rs)抽一个
    `obscura-embedder` crate:创建 Servo 实例、WebView、驱动 compositor。
    单进程模式(opts.multiprocess=false)。
2.2 constellation 在进程内跑(Servo 官方单进程本来就有 constellation 线程)。
    不改 ScriptThread——完整版接受 Servo 自己的线程拓扑。
2.3 渲染输出:Servo compositor → 现有 screencast/CDP Page.captureScreenshot 对接
    (tiny-skia 软渲染 or Servo 的 surfman/software 路径)。

### 阶段 3:CDP 桥(替换 obscura-cdp 的自研 Page 后端)
3.1 导航/evaluate:Runtime.evaluate → Servo 的 Web Driver script eval
    (webdriver_server 或 ScriptThread 的 WebDriverScriptCommand)。
3.2 Input.dispatchMouseEvent/Key:直接进 Servo 的 compositor 输入管线
    (真 hit-test、真 trusted 事件、真默认行为)——之前 humanGesture/输入下沉的
    全部目标在 Servo 里天然成立。
3.3 Page.captureScreenshot、Network 域:net_traits 的 fetch observer 挂钩。
3.4 Turnstile 容器注入、RenderTurnstile 的 Go 侧协议不变(仍走 CDP)。

### 阶段 4:反检测资产迁移(在 Servo Rust 层做,比 JS 层更可信)
4.1 UA/ClientHints/时区/locale:obscura 的 geo → UA profile 逻辑接 Servo 的
    UA 字符串与 navigator 构建(代码改 Servo DOM Rust 层)。
4.2 bootstrap.js 精华(17699 行)逐项评估:能在 Rust 层做的(navigator 属性、
    canvas/webgl 指纹、权限、插件、语言)移入 Servo DOM;纯 JS shim 的(事件
    trusted、getBoundingClientRect 合成——不再需要,Servo 有真布局)删除。
4.3 指纹一致性回归:对照 Camoufox/Chrome 的 ProbeAPIs 结果集。

### 阶段 5:网络换血
5.1 obscura-net(stealth/wreq/代理/relay)实现 net_traits 的 ResourceThreads 语义
    (或以独立线程服务 CoreResourceMsg)。
5.2 TLS/HTTP2 指纹(参考 curl_cffi/rquest)在 obscura-net 层做。

### 阶段 6:下线旧引擎
6.1 删 obscura-js/runtime、bootstrap.js 主路径(仅保留 CDP runtime 域兼容层或
    由 Servo 提供)、taffy 自研渲染(由 Servo layout 替代)。
6.2 WPT 回归:obscura 直接跑 Servo 的 wpt 子集(质变)。
6.3 Go 注册机全链路(300031/300030 回归)。

## 风险与缓解
- mozjs aarch64 编译:Servo CI 已支持;若 blocking,尝试 Servo 的
  ports/servoshell Docker 依赖清单;实在不行启动 B2(绑定重写,用户已授权)。
- CDP 语义差异:Servo 的 WebDriver 覆盖 CDP 的常用子集;缺少的(screencast、
  captureScreenshot)用 Servo 的 compositor framebuffer 直出。
- 反检测缺口:Servo 的 navigator 有明显特征(如 UA 无 Chrome token),
  4.x 阶段定制;挑战对抗风险交给 300030 实测驱动。

## 当前状态
- [x] 阶段 0:依赖分析(agent1)
- [ ] 1.1 mozjs 独立编译试验
- [ ] 1.2-1.4 workspace 合并
- [ ] 阶段 2+ …
