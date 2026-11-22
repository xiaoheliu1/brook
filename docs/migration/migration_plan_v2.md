# 两宿主迁移方案 v2（rhai → brook）

> 2026-11-22 · 基于两个宿主的全量源码深度审查（rhai_demo ~23k 行 / so_demo native ~7k 行 + Kotlin 层），
> 引擎基线：数组 v1.2（ADR-018）+ D-046 三批加固 + D-047/48/49（contains 类型化、import 回退修复、性能护栏）。
> 取代 D-035 时代的 POC 结论中已过时部分；`so_demo_inventory.md` §5.5 的重载收敛规则继续有效。

## 一、总盘对比

| 项 | rhai_demo（PC） | android_so_demo（Android） |
|---|---|---|
| 宿主代码 | ~23,000 行 Rust（toolkit 19 文件） | native ~7,000 行（lib.rs 2106 + toolkit 7）+ Kotlin 50 文件（纯回调面，不执行脚本） |
| 注册函数 | 224 处 / **197 名**（Task 3 属性 + Page 30 方法） | 232 处 / **190 名** |
| 重载名 | 24（msg_box ×4、draw 族 10 名各 ×2 …） | 42 槽（wait_id/desc 各 ×6、click 族 ×3 ×4 …） |
| 零引用函数 | **35**（draw 族 12、表格族 3/4、log 族 4、随机族 4 …） | **110**（约半数注册面无人用；~105 可直接删） |
| 脚本 | 34 个（最大 browser_task_archive 1402 行） | 41 个（25 生产 + 16 探针/回归；最大 128 行级） |
| rhai 特性依赖 | **重**：try/catch ~250 处 / 方法调用 ~800 处 / map literal 67 行 / is_def_var 27 / 闭包 3 | **轻**：Map 索引 112（全由 API 返回值驱动）/ 方法调用 191 / try/catch 8 / map literal **0** / 闭包 0 / 插值 2 |
| 脚本引擎位置 | Rust（engine.rs 组装） | Rust native（`rhai features=["sync"]` + 全局引擎/AST 缓存）；Kotlin 仅 OK\t/ERR\t 报告解析 |

**两宿主共同的底层依赖只有三样**：Map 取字段（全由 API 返回值驱动——脚本从不构造复杂 Map）、
方法调用糖（字符串/数组基本操作 + Opaque 方法）、try/catch（"可能失败的调用 + 记录"模式）。
map literal（so_demo 0 次）、闭包（合计 3 处）、eval（0）、is_def_var（27 处可选参数模式）都不是结构性障碍。

## 二、迁移方案

### 2.1 数据类型映射（API 返回侧）

| rhai 形态 | brook 形态 | 适用 |
|---|---|---|
| Map{2-8 固定字段} | **元组**（位置访问 `.0/.1`） | img_find、img_crop、mouse_pos、gps_once、current_app、fs_list 行结构等 **~40 名** |
| Array<Map{固定字段}> | **元组数组** `[ (T,T,...) ]`（宿主 `Vec<(T,...)>` 直接过边界，v1.2） | fs_list、ocr_screen、img_find_all/bind、wait_id、screen_size 等 **~25 名** |
| Map{>8 字段} | **拆名**（8 处：device_info/app_info/ui_dump/date_parts/win_list/win_active/pick_*/fs_walk） | 分组元组或拆双函数 |
| 嵌套（Array<Array<Map>>、Map 内 Array<Map>） | **逐个重设计**（3 处最难：sheet_rows 二维表、ui_find_report、json_parse） | 见 §2.4 难点清单 |
| Dynamic（json_parse/json_str/rnd_pick） | 结构固定或拆名 | json 族见 §2.4 |
| Map 参数（exec/run_plugin 的 spec） | **位置参数 + 关键字序固定** | `run_plugin(argv, json_input, env, cwd)`——脚本 8 处调用全部带 `#{argv,…}`，重写为位置传参 |
| Opaque 对象（Task/Page） | **句柄 + 显式函数族**（POC 已验证 browser_page→Opaque 形态） | Page 30 方法 → `page_title(p)` / `pick_css(p,…)`；Task → `task_step(t,…)` |

### 2.2 rhai 语法 → brook 重写表（脚本侧）

| rhai 语法 | 出现量 | brook 对应 |
|---|---|---|
| 方法调用 `x.f()` | 两宿主合计 **~1000 处** | 字符串/数组基本操作 brook 有内置（`len/contains/push/to_str`），语法改函数式；`.trim()/.replace()/.split()` 宿主注册补；Page/Task 方法改 `page_xxx(p)` 函数族 |
| try/catch { } | **~258 处**（rhai_demo 250 + so_demo 8） | 宿主可能失败的函数返回 `(ok, …)` 元组；脚本 `if !ok { push(fails, …) }`——so_demo smoke_ported 模式已验证 |
| map literal `#{…}` | 67 行（rhai_demo 11 文件） | 元组字面量（固定字段）或删（so_demo 0 次） |
| `import "x" as c; c.f()` | rhai_demo 命名空间式 | brook 平铺 `import "x"` + `f()`（撞名编译报错兜底） |
| `is_def_var("tol")` 可选入参 | 27 处（pdf_watch 16 / browser_task_archive 11） | 入口显式参数 `main(tol)` 或宿主 `has_arg("tol")` 查询函数 |
| 闭包 `\|x\| {…}` | 3 处（全是 hold_key/hold_mouse body） | 见 §2.4 难点：hold 块原语 |
| 字符串插值 | 4 处真实 | `+` 拼接（两宿主主流写法本来就是 `+`） |
| eval / switch / Dynamic | 0 处真实代码 | 无迁移面 |

### 2.3 宿主机制改造（两宿主共有的病，迁移时一次治好）

| 病 | 位置 | brook 迁移处置 |
|---|---|---|
| 进程级闸门/取消旗 vs 未来并发 | rhai_demo `LIVE/HELD/CANCELLED/SINK/SESSION`（20+ static）；so_demo `FS_GATE` 进程级 | **per-run 上下文**：live/cancel/vision 容差/模板缓存跟 run 走（RunContext 结构，注册闭包捕获 Arc<RunCtx>）。这是宿主改造最核心的架构动作，同时修掉 rhai_demo B-7/B-8 与 so_demo C1/C2 |
| vision 状态 thread_local | 两宿主同病（img_tol 跨线程静默失效） | 归入 per-run 上下文 |
| 整轮墙钟上限缺失 | so_demo C4（"谁来杀卡死脚本=没人杀"） | Kotlin 侧加单轮最大墙钟（可配置）；brook gas 只兜纯算术 |
| AST/引擎缓存 | so_demo `cached_engine`(64 位 hash 键无复核) | brook `Script` + 指纹缓存：键含 (len, hash, host_fp)；跨引擎复用由 D-046 3.8 指纹拒绝兜底 |
| 安全限制对齐 | rhai `set_max_operations(2M)/call_levels(64)/string(1MB)` | `with_limits` + `with_heap_limit`（口径对齐：so_demo MAX_OPERATIONS=1 亿 → gas 步数预算等价） |
| SEP(`\u0001`) 拼串协议 | so_demo Rust 8 调用点 + Kotlin 12 解析点 | brook 原生多参 → **整族拆除**（换引擎最大净收益之一） |
| ARGS 注入（rhai Scope 常量 Map） | so_demo runScriptArgs | brook 入口参数 `call(main, args)`；`is_def_var` 模式 → 显式参数或 `has_arg` 查询函数 |
| print 接管 / print_raw | rhai_demo on_print + print_raw | brook 无 print：脚本输出全走宿主 `log`/`print_raw` 注册（形态不变） |
| stop_task 不可捕获终止 | rhai_demo 退出码 3/5 依赖 | brook 宿主函数返回后由宿主主循环短路（RunCtx.cancelled 检查点已有机制承接） |

### 2.4 真正的难点清单（需要设计决策，共 6 个）

1. **hold_key/hold_mouse(name, 闭包)**（rhai_demo，脚本 3 处调用）→ 方案 A：宿主 `hold_begin(name)` / `hold_end()` 显式对（急停护栏 Drop 语义改为宿主轮次结束兜底）；方案 B：brook 加块作用域原语（引擎改动）。**推荐 A**（零引擎改动，脚本改写量 3 处）。
2. **exec/run_plugin 的 Map spec**（脚本 8 处，含嵌套 env Map）→ 位置参数 `run_plugin(argv: [str], json_input: str, env: [(str,str)], cwd: str)`；`json_input` 保持字符串透传（POC 已验证）。
3. **json_parse/json_str（Dynamic 进出）**→ JSON 是宿主侧事务：`json_parse(s) -> (ok, [ (path, kind, value_str) ])` 扁平键值数组？或按"脚本实际用到的 JSON 形状"（12 处字段访问）定义固定 record 序列。**推荐：宿主按需提供窄接口**（如 `json_get(s, path) -> (ok, value_str)`），JSON 树不过脚本边界——脚本只用 12 处固定字段，窄接口完全覆盖。
4. **sheet_rows 二维表（Array<Array<Map{3}>>）**→ 零引用之外的 1 处使用（冒烟自检）；改 `[ [(str,str)] ]`？元组的元组数组——brook 数组元素可为元组 ✓ 可表达，但三元信息（kind/value/text）需要第二数组平行或砍字段。**推荐：先砍零引用的 sheet_cell/range/names，sheet_rows 迁移时按实际字段重设计**。
5. **pick_css 11 字段 / date_parts 12 字段等 8 个 >8 记录** → 分组元组（如 pick → `(found, x, y, w, h, cx, cy, txt)` 8 元 + 剩余字段拆第二元组）或按调用点实际使用的字段裁剪（D-035 分析：每个调用点实际用 ≤8）。
6. **eval_js（任意 JS 注入面）** → 脚本侧只用了 3 类常量表达式（visibilityState/focused/scrolly）→ 改 3 个只读专用原语后**裁掉 eval_js**（安全净收益）。

### 2.5 重载收敛（已定三规则，落点见 so_demo_inventory §5.5）

- so_demo 42 槽 → ~15 签名（数字合并 11 名、可选参超集 19 名、SEP 拆参）
- rhai_demo 24 名 → ~12 签名（msg_box 4→1、draw 族 10 名 2→1、sleep/stamp/date_ms/clamp 合并）
- 同名注册冲突在**启动时**暴露（`already registered`），漏改不静默

## 三、宿主修改程度报告

### 3.1 so_demo（Android）——**建议先行**（脚本轻、Kotlin 零改动、POC 已验证）

| 改造面 | 程度 |
|---|---|
| native/lib.rs | `run_rhai` → `run_brook`：引擎组装 + 报告协议 **保持 OK\t/ERR\t 不变**；AST 缓存换 brook Script 缓存（键 + 指纹）；ARGS 注入改入口参数。约 **600-800 行重写** |
| toolkit 7 文件 | 函数实现逻辑基本不动；**签名/返回改造 ~60 个函数**（Map→元组 13、Array→Vec 13、重载合并 ~15、SEP 拆参 7）；**删除零引用 ~105 个注册**（190 → 实际注册 ~85 名） |
| Kotlin 侧 | **零改动**（报告协议、NativeBridge 5 个 external、回调面全不变）；caps.txt 同步为 brook 形态 |
| 脚本 41 个 | 25 生产脚本重写（多为 <50 行小改：Map 索引→元组索引 112 处、方法糖 191 处、插值 2 处）；16 探针/回归脚本按需重写或退役；horn_blow 三元 bug 顺手修 |
| 预估 | native **~1 周** + 脚本 **2-3 天** + 真机回归 **1-2 天**（perf_run_script/冒烟脚本即验收器） |

### 3.2 rhai_demo（PC）——**第二步**（try/catch 与 Page 族是大头）

| 改造面 | 程度 |
|---|---|
| engine.rs 组装 | rhai Engine → brook Engine：`with_limits` 对齐 + ADR-017 import 替换 FileModuleResolver + per-run 上下文注入。~300 行 |
| toolkit 19 文件 | 注册收敛（删 35 零引用、重载合并 26、Map→元组 ~27、>8 拆 8）；**Page 30 方法 → page_* 函数族**（POC 已验证形态）；exec/run_plugin spec → 位置参数；json 族窄接口；hold 闭包 → hold_begin/end。实现逻辑大多保留。~1.5 周 |
| 脚本 34 个 | **主体工作量**：try/catch ~250 处 → (ok,…)+fails.push 模式；方法调用 ~800 处 → 函数式；browser_task_archive（1402 行）单独 2-3 天重构；diag/pdf_watch/archive_flow 各 1 天级。合计 **1.5-2 周** |
| rhai_demo_gui | 编辑器补全清单换 brook 内置函数源（build.rs gen_fn_signatures 换 brook 的 caps 导出）；不执行脚本，改动小 |
| 预估 | 宿主 **~1.5 周** + 脚本 **1.5-2 周** + 回归 **2-3 天** |

### 3.3 宿主"不适合/没用"函数处置清单（授权删除项）

- **so_demo 删 ~105 名**：add（演示残留）、battery_level/img_load（别名）、rgb/rgba（改常量）、全部零引用 UI/设备/网络族（device_info、ui_dump、http_get/post/download、press_*、root_* 家族、regex 五件套、media_* 等）
- **rhai_demo 删 35 名**：draw 族 12（仅 3 名在用）、表格族 3、log 族 4、随机族 4、show_html/show_table/show_text、open_with/launch、click_dialog 族 2、fs_walk/fs_copy_dir/fs_write_bytes、toast
- **改安全裁剪**：eval_js → 3 个只读原语；rhai 内置 eval 在 brook 天然不存在
- **保留但标注**：so_demo 的 shell/root（升权面，S2——产品形态，建议加轮级白名单）、rhai_demo 的 exec/run_plugin（live 闸已管）

## 四、宿主问题清单（两宿主合并，迁移时处置）

### P1（迁移前或迁移中修）
| # | 宿主 | 问题 | 处置 |
|---|---|---|---|
| B-1 | rhai_demo | `adb shell "rm -f \"{rel}\""` 引号逃逸注入（validate_rel_path 不拦引号/分号/$()） | 改参数数组传 adb（与自家铁律一致）；或过滤 `";$` 反引号 |
| B-2 | rhai_demo | rhai FileModuleResolver 接受**绝对路径 import**（"模块=scripts/lib"边界名不副实） | brook ADR-017 天然修复（相对/白名单/逃逸防护）——迁移即消除 |
| S2 | so_demo | 本地脚本可无条件 shell/root（总闸开时） | 记录为产品升权面；建议 per-run 白名单 |
| S1 | so_demo | `/sdcard/x` 被 fs_* 挂进影子树（意图反转） | 迁移时路径护栏改为"拒绝 looks_absolute"（显式报错优于静默挂树） |

### P2（迁移中随架构项一起修）
| # | 宿主 | 问题 | 处置 |
|---|---|---|---|
| B-7/C1 | 两宿主 | 进程级闸门跨轮武装/失效 | per-run 上下文（§2.3 第一行） |
| B-8/C2 | 两宿主 | vision 状态 thread_local 与引擎生命周期错位 | 同上 |
| C3 | so_demo | native 硬崩 → done:false 死局无看门狗 | Kotlin 加单轮墙钟 + 启动时清理陈旧 done:false |
| C4 | so_demo | 无整轮墙钟 | 同 C3 |
| B-4 | rhai_demo | stdout 断开时 `print!` panic 杀整个 REPL（与"写失败吞掉"注释相反） | `write!` + 忽略错误 |
| B-3 | rhai_demo | 默认口令 `sodemo` 明文 + 全链 http | 强制首启改口令或文档明示 |
| S3 | so_demo | ServerSocket 绑 0.0.0.0（lanOnly 只是 IP 过滤） | lanOnly 时绑私网地址 |
| S7 | so_demo | JNI 读串失败静默空串 → 空脚本返回 OK | 空脚本显式 ERR |
| C5 | so_demo | AST 缓存 64 位 hash 无复核 | brook 指纹缓存（键含 len+hash+fp） |

### P3（随手/低危）
rhai_demo：B-5 命名管道默认 DACL、B-6 未关 eval、B-9 cancel 分支不 join、B-10 配置错误静默、B-11 CLI 未知开关静默、B-12 runner 全量打印末表达式、B-13 draw 线程重试+SCENE 无上限、B-14 test.rhai 必炸冒烟、B-17/18 超长函数与 9 份脚本复制 check/dump（迁移时入 lib/common.bk 一份）。
so_demo：S4 keystore 弱口令、S5 全局明文 HTTP、S6 权限画像宽、S8 i64→i32 截断、C6 /push 非原子写、C7 模板缓存无 mtime、C8 flatten_pts 静默跳过、C9 deviceLock 死等、D4 注释过期。
**脚本侧存量坏件**：rhai_demo `test.rhai` 必炸；so_demo `horn_blow.rhai:54` 三元（rhai 不支持，现在就编译不过）；9 份脚本抄写 check/dump → 迁移进 lib/common.bk。

## 五、执行计划建议

| 阶段 | 内容 | 验收门 |
|---|---|---|
| 0 | 决策点拍板（见下） | — |
| 1 | **so_demo native 迁移**（lib.rs 引擎层 + 签名收敛 + 删零引用） | caps.txt 重新对账；perf_run_script/冒烟脚本过；真机 PLR110 全量回归 |
| 2 | **so_demo 脚本重写**（25 生产 + 探针退役） | 41 脚本全部 OK\t；既有任务产物 diff 对账 |
| 3 | **rhai_demo 宿主迁移**（engine.rs + toolkit 收敛 + Page 族 + json 窄接口 + per-run 上下文） | 276 引擎测试 + check_scripts 移植版；diag.rhai 全绿 |
| 4 | **rhai_demo 脚本重写**（try/catch 模式改写 + browser_task_archive 重构） | 34 脚本全绿；browser_task_archive 行为对账 |
| 5 | GUI 补全源切换 + 文档（TUTORIAL 增量） | — |

**决策点（需要你拍板）**：
1. **hold_key/hold_mouse**：推荐方案 A（hold_begin/end 显式对，零引擎改动）——接受吗？
2. **json 族**：推荐窄接口（json_get(path)），JSON 树不过脚本边界——接受吗？
3. **零引用删除授权**：so_demo ~105 名 + rhai_demo 35 名，删除后 caps.txt/api_list 同步——确认？
4. **GUI 范围**：rhai_demo_gui 补全清单是否纳入本次迁移（建议纳入，工作量半天）？
5. **执行顺序**：so_demo 先（建议）还是 rhai_demo 先？
