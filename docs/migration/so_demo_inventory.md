# android_so_demo 脚本引擎替换评估 · 数据底座报告

> 采集日期：2026-11-21。采集方式：Explore 代理全量 grep + 抽读，行号为当时工作区实际行号。
> 采集对象：`D:\abc\rustrust\android_so_demo`（rhai 1.26.1 `sync` feature）。

## 1. 整体架构

### 1.1 .so 的暴露方式：纯 JNI

`.so`（`libnative_demo.so`）只被 Android App（`com.example.sodemo`）通过 JNI 加载，无 app_process / C ABI / 命令行入口：

- `native/Cargo.toml:10-12`：`crate-type = ["cdylib", "rlib"]`，`[lib] name = "native_demo"`。
- `app/.../NativeBridge.kt:21`：`System.loadLibrary("native_demo")`。
- 对外 JNI 符号（`native/src/lib.rs`）：

| JNI 符号（Rust 侧） | Kotlin external | 位置 |
|---|---|---|
| `JNI_OnLoad` | (系统自动) | lib.rs:70 |
| `..._initEngine(host: Any)` | initEngine | lib.rs:81 |
| `..._setRootPath(path)` | setRootPath | lib.rs:103 |
| `..._setRestricted(restricted)` | setRestricted | lib.rs:117 |
| `..._runScript(code, name) -> String` | runScript | lib.rs:127 |
| `..._runScriptArgs(code, name, argKeys, argValues) -> String` | runScriptArgs | lib.rs:147 |

### 1.2 引擎与缓存

- `native/Cargo.toml:44`：`rhai = { version = "1", features = ["sync"] }`（实际 1.26.1）。`sync` 是因为进程级跨线程复用：`ENGINE_CACHE`（按 root key，lib.rs:1658）、`AST_CACHE`（源码 hash，64 条，lib.rs:1662）。
- **脚本正文以字符串经 JNI 进入**（不是引擎读文件）。设备工作区 = `getExternalFilesDir`，脚本目录 `<root>/scripts/`（`ScriptHost.kt:78-101`）。`.push/`、`.tmp_push/rev_scripts/`、`docs/rerun/` 都是待推送/已推送的脚本源，不是运行时查找路径。
- 嵌套脚本：`run_script(name)` → Rust 回调 Kotlin `readScriptForRun` 取正文递归（lib.rs:1741-1793）。

### 1.3 执行入口与结果回传

- **无入口函数名约定**：整段脚本顶层语句流，`eval_ast_with_scope::<Dynamic>`（lib.rs:1870），**末值被丢弃**（lib.rs:1868-1869）。
- 回传协议：`OK\t...`（lib.rs:1882）/ `ERR\t<中文错误>`（lib.rs:1883）；Kotlin `classifyRunResult`（ScriptHost.kt:40-46）按前缀判成败。
- 过程输出：`log(s)` → JNI 回调 Kotlin `logLine` → Log.i + 落盘（lib.rs:709-721）。
- 嵌套失败记账：`SUB_SCRIPT_FAILURES` 线程本地（lib.rs:695-697），防"子脚本挂了父脚本绿灯"。

## 2. 宿主暴露函数完整清单（232 个重载 / 190 唯一名）

### 2.1 lib.rs（163 重载 / 128 名）

**输入/基础（12 名）**：`log(String)->()`:807、`toast(String)->bool`:815、`add(i64,i64)->i64`:818、`sleep(i64)`/`sleep(f64)` Throw:826/843、`is_cancelled()->bool`:1633、`wait_ms(i64)`/`wait_until_ms(i64)` Throw:1613/1618、`type_text(String)->bool`:1627、`root_type_text(String)->bool`:1629、`swipe(i64×4)`:1637 / `swipe(i64×5)`:881。

**点按/手势（9 名）**：`tap(i64,i64)`:860、`root_tap`:862、`root_swipe(i64×5)`:885、`swipe_human`:892、`root_swipe_human`:899、`pinch`:906、`long_press`:914、`root_long_press`:918、`root_key(i64)`:930 —— 全 bool。

**按键/Shell（9 名）**：`press_back/home/recents()->bool`:924-928、`shell(String)->String`:871、`root(String)->String`:875、`run_script(String)->String`:1567、`run_script_soft`:1571、`stop_script`:1576、`in_time(String)->bool`:1560。

**设备信息（15 名）**：`screen_size()->`**`Array`**`(i64)`:934、`orientation()->String`:945、`device_info()->`**`Map`**:949、`battery/battery_level/brightness->i64`:1469-1482、`screen_on/is_charging/power_save/gps_enabled->bool`:1473-1489、`net_online->bool`:986、`net_ip/net_type/front_app/bluetooth_state->String`:990-1494。

**网络/权限/存储（14 名）**：`http_get/http_post/http_download->String`:968-980、`request_root/a11y/overlay/capture/storage->bool`:995-1003、`ext_files_dir/sdcard_root/ws_path->String`:1222-1234、`has_all_files_access/open_all_files_settings->bool`:1228-1231、`dump_frame->String`:1034。

**OCR（3 名）**：`ocr_screen()->`**`Array<Map>`**{x,y,w,h,conf,text}:1008、`ocr_region(i64×4)->`**`Array<Map>`**:1015、`ocr_find_text(String)->`**`Array`**(i64):1022。

**画图（13 名 / 20 重载）**：`rgb/rgba->i64`:1045-1048；`draw_line/rect/rect_fill/circle/circle_fill/point/cross`（标量重载 bool）:1054-1114；`draw_text`:1120-1124；`draw_path/draw_polygon(`**`Array`**`)->bool`:1130-1142；`draw_clear/hide`:1148-1150。

**音频（3 名）**：`media_play/media_stop/media_beep->bool`:1162-1167。

**无障碍控件树（18 名）**：`find_text/find_id/find_desc/find_by(String)->`**`Array`**(i64×4):1171-1265、`ui_dump()->`**`Array<Map>`**:1248、`ui_find_report(String)->`**`Map`**{reason,truncated,nodes(**Array<Map>**)}:1437、`current_app()->`**`Map`**:1244、`click_text/click_id/click_node/long_click_node`（String / +i64 / +f64 重载）->bool:1269-1431、`scroll_up/scroll_down->bool`:1275-1277、`set_text_of(String,String)->bool`:1281。

**wait 控件族（5 名 / 23 重载，全返回 `Array`** [x,y,w,h] **或空）**：`wait_ui(String,i64/f64)`:1287,1292；`wait_id`（六种重载）:1304-1346；`wait_desc`（六种）:1308-1358；`wait_gone->bool`:1364-1371；`scroll_until`（三种）->**`Array`**:1377-1386。

**应用管理（6 名）**：`launch_app/installed/is_app_enabled->bool`:1445-1449、`launch_activity->bool`:1452、`app_list()->`**`Array<Map>`**:1457、`app_info(String)->`**`Map`**:1465。

**监听器上下文/开关 setter（15 名）**：`gps_once()->`**`Map`**{ok,lat,lng,acc,time,speed}:1497、`trigger_kind()->String`:1500、`trigger_json()->`**`Map`**:1502、`watch_running->bool`:1582、`rules_count->i64`:1584、`rules_list->String`:1586、`set_bluetooth/power_save/gps/wifi/airplane/silent(bool)->String`:1536-1548、`set_brightness/media_volume/ring_volume/call_volume/alarm_volume(i64)->String`:1546-1557。

**root 进程操作（4 名）**：`root_force_stop/pm_clear/install(String)->bool`:1591-1595、`root_kill_all()->bool`:1597。

**regex 纯 Rust（5 名）**：`regex_find->String`:1506、`regex_capture->`**`Array`**:1513、`regex_match->bool`:1520、`regex_replace->String`:1524、`regex_split->`**`Array`**:1528。

**Scope 常量注入（非函数）**：lib.rs:1827-1833 注入 9 个颜色 i64 常量；lib.rs:1835-1841 注入只读 Map `ARGS`（全仓 0 处使用）。

### 2.2 toolkit/fs.rs（16 名 / 18 重载，全 Throw；root=<filesDir>）

`fs_read->String`:530、`fs_write/fs_append->i64`:533-536、`fs_exists->bool`:539、`fs_list->`**`Array<Map>`**{name,rel,dir,size}:542、`fs_is_dir/fs_is_file->bool`:545-548、`fs_mkdir->bool`:551、`fs_size/fs_mtime->i64`:554-557、`fs_copy->i64`:560、`fs_move->bool`:563、`fs_remove->bool`:566、`fs_remove_dir(&str,bool)->bool`:569、`fs_read_bytes->`**`Array`**(i64):574、`fs_write_bytes(&str,`**`Array`**`)->i64`:577、`fs_destructive()->bool`:582-583。

护栏：相对路径挂 root 且禁 `..` 出根（mod.rs:163-177）；删/移需 `fs_destructive(true)` 开闸（mod.rs:181-195）；每轮复位（lib.rs:1811-1814）；受限线程禁写 scripts/、shared_prefs/、triggers.json、工作区外（fs.rs:115-149）。

### 2.3 toolkit/path.rs（7 名，纯字符串）

`path_join->String`:180、`path_parent/ext/name/stem/norm->String`:181-185、`path_is_abs->bool`:186（光杆斜杠故意返回 false，mod.rs:71-85）。

### 2.4 toolkit/task.rs（4 名 + 1 类型）

`register_type_with_name::<Task>("Task")`:44；属性 `title`（读写）、`steps`（只读）、`errors`（只读）:47-53；`new_task(&str)->Task`:56、`step(&mut Task,&str)->i64`:57、`fail(&mut Task,&str)->()`:58、`summary(&mut Task)->String`:59。脚本用法仅 `scripts/smoke_ported.rhai:61-71` 一处。

### 2.5 toolkit/util.rs（13 名 / 16 重载）

`rnd(i64,i64)->i64 Throw`:460、`rnd_float()->f64`:461、`rnd_pick(&mut Array)->Dynamic Throw`:462、`rnd_seed(i64)->()`:463、`epoch_ms->i64 Throw`:465、`stamp()->String`:466、`stamp(i64)->String`:467、`date_parts(i64)->`**`Map`** 12 字段:471、`date_parts(i64,i64)->`**`Map`**:472、`date_ms(i64×6/×7)->i64`:473-474、`span_ms(&str,i64)->i64 Throw`:475、`dur_str(i64)->String Throw`:476、`join(&mut Array,&str)->String`:480、`clamp`（i64/f64 双态）:481-482。

### 2.6 toolkit/vision.rs（22 名 / 24 重载，几乎全 Throw）

命中约定：命中 Map = `{x,y}`（模板中心，vision.rs:259-263），不给 score；未命中 = 空 Map；多命中 = Array<Map> 带 `name`。

| `img_tol/img_interval->i64`:1196-1197、`img_region(i64×4)->bool Throw`:1198、`img_unload_all->i64`:1199 |
|---|
| `img_has->bool Throw`:1202、`img_load/img_info->`**`Map`**{w,h,masked,bytes}:1207-1211、`img_unload->bool`:1215 |
| `img_find->`**`Map`** Throw:1221、`img_find_at->`**`Map`**:1226、`img_find_all->`**`Array<Map>`**:1234、`img_find_many(&mut Array)->`**`Array<Map>`**:1242、`img_find_bind(&str,`**`Array`**`[name,dx,dy],i64)->`**`Array<Map>`**（+带 region 重载）:1250-1257 |
| `img_wait/img_wait_at->`**`Map`**:1285-1291、`img_gone->bool`:1299、`img_screen_size()->`**`Map`**{w,h,monitors}:1305 |
| `img_color(i64×5)->`**`Array<Map>`**:1311、`img_color_at->`**`Map`**{r,g,b}:1319、`img_shot->`**`Map`**:1324、`img_crop->`**`Map`**:1329、`img_stable->`**`Map`**{ok,changed,watched,tries}:1338-1342 |

线程本地状态：`TOLERANCE/REGION/INTERVAL_MS/TEMPLATES`（vision.rs:88-106）。取帧经 `Source` trait 由 `JniSource` 回调 Kotlin `grabFrame`（lib.rs:460-517）。

## 3. 脚本侧特性使用统计（全部 50 个 .rhai）

分布：`scripts/` 25、`.push/` 9、`.tmp_push/rev_scripts/` 7、`docs/rerun/` 8、`build/` 1。最大 3 个：`smoke_ported.rhai`(128 行)、`_find_speed.rhai`(85)、`bind_verify.rhai`(84)。

| 特性 | 文件数 | 次数 | 代表片段 |
|---|---|---|---|
| **Map 字面量 `#{...}`** | **0** | 0 | — |
| **Map 索引 `m["k"]`**（读宿主返回值） | 17 | ~60 | `bind_verify.rhai:20` `sz["w"]`；`smoke_ported.rhai:86` |
| Map 点号访问 | 1（Task 对象） | 8 | `smoke_ported.rhai:62` |
| **数组字面量** | 22 | ~100 | `bind_verify.rhai:44`（嵌套） |
| 数组 `.push()` | 4 | 8 | `z_rev_loop.rhai:2` |
| 数组索引 `[0]` | 11 | ~26 | `bind_verify.rhai:47` |
| 字符串 `.to_string()` | 16 | 147 | `bind_verify.rhai:23` |
| 字符串 `.len()` | 17 | 31 | `peek_pref.rhai:2` |
| 字符串 `.split()` | 3 | 4 | `peek_pref.rhai:2` |
| 字符串 `.contains()` | 2 | 5 | `display_toggle.rhai:31` |
| 其余字符串方法 | 0 | 0 | — |
| 字符串 `+` 拼接 | ~20 | 41 | `t_wifi.rhai:4` |
| **反引号模板串** | 4 | 4 | `z_rev_gate.rhai:3` |
| **try/catch** | 2（实质） | try 8 / catch 10 | `smoke_ported.rhai:54` |
| **throw** | 1 | 1 | `z_rev_tab.rhai:1` |
| **闭包/匿名函数** | **0** | 0 | — |
| **switch** | **0** | 0 | — |
| **for-in 迭代** | 15 | 28 | `smoke_ported.rhai:85` |
| while | 2 | 2 | `z_rev_loop.rhai` |
| **函数定义 fn** | 6 | 13 | `display_toggle.rhai:29-37` |
| 顶层 `return` | 5 | 5 | `bind_verify.rhai:15` |
| let | 39 | 237 | — |
| if | 20 | 48 | — |
| `+=` | 8 | ~12 | `smoke_ported.rhai:19` |
| 末表达式作返回值 | 4 | — | `z_rev_ok.rhai:4` |
| 使用 `ARGS` | **0** | 0 | — |
| `log()` 调用 | ~40 | 173 | 最高频原语 |

频度 Top10：epoch_ms 62、img_find 18、img_tol 14、img_region 12、request_capture 11、img_screen_size 10、wait_ms 9、swipe 7、img_find_bind 7、img_crop 6。

一句话结论：动态能力集中在 **Map 索引取宿主返回值 + Array（字面量/push/迭代/索引）+ 字符串 to_string/len/split + try/catch（测错误路径）+ fn 定义 + 反引号模板串**；闭包、switch、Map 字面量、Map 点访问、ARGS 均为零使用。

## 4. .so 的调用方与传参

唯一调用方 = App 进程内 Kotlin。全部脚本入口（最终汇到 `ScriptHost.runScriptEx`，ScriptHost.kt:476-571）：编辑器运行按钮（EditorFragment.kt:564）、任务卡片（TasksFragment.kt:353）、悬浮球（FloatButtonService.kt:309,647）、HTTP `/run`（可带 args，HttpServerService.kt:861-899）、HTTP `/push`（只落盘）、MacroDroid/Tasker 广播（ExternalReceiver.kt）、监听器触发（ScriptRunService.kt:260-265）、脚本内 `run_script(name)`（ScriptHost.kt:2770）。

传参：无命令行/环境变量。HTTP args 拆成 `\u0001` 分隔两串 → `parse_arg_pairs`（lib.rs:1697-1703）→ 注入只读 Map `ARGS`。脚本→宿主方向：只有 `OK\t/ERR\t` 报告串；数据落地靠 fs_write/img_shot/log/http_post。

`bin/uinput_probe.rs`：独立可执行（feature 门控，默认不构建），root 探针——`ioctl /dev/uinput` 造虚拟触摸屏验证替代无障碍输入的路线，与本 .so 无关。

## 5. 替换为 brook 的约束清单

### 5.1 构建/编译约束

1. cdylib 必须保留；rlib 为桌面测试。交叉编译只有 aarch64-linux-android 一条目标（`.cargo/config.toml:4-5`）；**Windows 主机不产 so**。
2. Gradle 与 Cargo 零集成：构建链 = `scripts/build_android_demo.ps1:44-48` 手工 cargo build → 拷 `libnative_demo.so` → `app/src/main/jniLibs/arm64-v8a/`。只打 arm64-v8a。
3. 依赖面：`jni 0.21`（纯 Rust）、`pixhunt`（本地 path）、`image 0.25`（png）、`regex 1`；release lto+strip。

### 5.2 引擎行为契约（替换必须逐条对齐）

| 契约 | 位置 | 说明 |
|---|---|---|
| `OK\t`/`ERR\t` 返回前缀 | lib.rs:1821,1857,1880-1884；ScriptHost.kt:40-46 | 末值丢弃，成败只看前缀 |
| try/catch 接得住宿主错误 | mod.rs:197-235 | 工具错误必须是 `ErrorRuntime` |
| `Result<T,ScriptError>` = 脚本异常 | fs/vision/util | brook 无异常 → 改错误值/元组语义 |
| 取消检查点 | lib.rs:794-802（set_max_operations 1 亿次 + on_progress 每 65536 次回调 Kotlin `cancelled`） | **死循环必须可中断**；rhai 靠进度回调协作取消 |
| panic 安全网 | lib.rs:39-50,137-141 | panic 穿 JNI 会 abort 进程 |
| 引擎/AST 进程级缓存 | lib.rs:1654-1693 | **跨线程复用**（sync）——brook `!Send`，需每线程 Engine 或线程本地缓存 |
| 每轮复位语义 | lib.rs:1801-1815 | 顶层轮清失败账本、复位 fs 闸门 |
| Scope 常量注入 | lib.rs:1826-1841 | 9 颜色 + ARGS（ARGS 0 使用可砍；颜色可改 9 个零参函数） |
| 方法调用式注册 | util.rs:478-482, task.rs | `arr.join("-")`、`6.clamp(1,5)`、`t.summary()`——brook 只能普通函数 |
| 线程本地状态 | lib.rs:684-703、fs.rs:31-37、util.rs:69-77、vision.rs:88-106 | SCRIPT_STACK/RESTRICTED/RNG/模板缓存/容差/区域/间隔 |
| 受限写围栏 | fs.rs:40-164；JNI setRestricted | 按调用线程生效 |

### 5.3 数据类型层面（数组已进语言 · ADR-018 更新）

232 个重载中：**返回 Map 19 个**（device_info、current_app、ui_find_report、app_info、gps_once、trigger_json、date_parts×2、img_load/img_info、img_find、img_find_at、img_wait、img_wait_at、img_screen_size、img_color_at、img_shot、img_crop、img_stable×2）；**返回 Array ~55 个**（screen_size、wait_ui×2、wait_id×6、wait_desc×6、scroll_until×3、ui_dump、ocr_screen/region/find_text、find_text/id/desc/by、app_list、regex_capture/split、fs_list、fs_read_bytes、img_find_all/many/bind×2、img_color）；**参数收 Array 8 个**（draw_path×2、draw_polygon×2、fs_write_bytes、img_find_many、img_find_bind×2、join、rnd_pick）；嵌套结构：ui_find_report（Map 内 Array<Map>）、fs_list/ui_dump/ocr_*/app_list（Array<Map>）。

脚本侧恰以 `m["key"]`（17 文件）、数组（22 文件）、for-in（15 文件）为最高频消费。**ADR-018（数组进入语言）之后本节方案分层更新**：
- 固定记录 → **N 元组**（桥接层已扩至 2-8 元，D-035）：`img_screen_size -> (w,h)`、`img_crop -> (x,y,w,h)`、`wait_ui -> (ok,x,y,w,h)`；
- 变长列表 / 记录集 → **原生数组**（v1.2 新能力）：宿主 `Vec<T>` 直接过边界（HOST_GUIDE §7.1）——`wait_id -> [(i64,i64,i64,i64)]`、`fs_list -> [(str,str,bool,i64)]`、`find_text -> [str]`；脚本侧 `for r in rows` + `r.0` 原生消费（D-045 之前的 POC 用 Opaque+getter，现在仅剩"宿主内可变状态"场景仍走 Opaque：Task 对象、BindList 这类带内部可变性的）；
- Map（19 个固定字段记录）→ 元组（字段 ≤8 全部覆盖）；动态键查表调用点为零（ADR-018 Map 暂缓的依据）。

### 5.4 路径硬编码

Rust 侧无设备路径硬编码（root 由 Kotlin `setRootPath` 注入）；JNI 符号名硬编码包名 `com.example.sodemo.NativeBridge`；设备路径硬编码只在脚本内容里（`/sdcard/abc/tools/DisplayToggle.dex` 等）。`uinput_probe` 硬编码 `/dev/uinput`（探针属性）。

### 5.5 同名重载的收敛规则（brook：名字 → 唯一签名）

brook 的注册表是**名字 → 唯一签名**：同名注册第二个在注册时即报错 `host function ... is already registered`（不静默覆盖、不运行时重载分派）。这是设计取舍（ADR-013）：静态类型下调用点按名字解析到固定的 `CallHost` 槽位号，签名进 sigs 表与缓存指纹——重载解析会把 typecheck / 字节码 / 指纹 / 错误栈整条链打上"按参数签名区分"的键，持久复杂度换不来自动化脚本的真实需求。**232 个重载按三条规则收敛，引擎零改动**：

1. **数字重载 → 合并签名**：宿主边界有 i64→f64 隐式提升（ADR-006），签名写 `f64` 可同时接住脚本传的 `5` 与 `5.0`。适用：`wait_ui`(i64/f64)、`click_node`(+i64/+f64)、`wait_id`/`wait_desc` 的数字重载对。⚠️ 反向不成立：签名写 `i64` 接不住 `1.5`（精度语义由签名决定）。
2. **可选参数重载 → 取超集签名**：`click_text(String)` / `click_text(String,i64)` → 注册 `click_text(String,i64)`，脚本侧少参调用点补显式默认值（迁移本就是重写，顺路改，无隐式默认值机制）。
3. **类型族不同 → 拆名**：宿主注册名与脚本调用名一起换，风格沿用 rhai 作者自己的习惯（`img_find` / `img_find_at` / `img_find_all` / `img_find_bind` 本来就是拆开的）——如 `find_text`(String) 与 `find_text`(Array) 若同仓出现， Brook 侧注册 `find_text` / `find_text_many`。

POC（tests/migration_so_demo.rs）全程"一名一签"跑通，无一例需要引擎侧改动。宿主侧迁移 checklist：注册完所有名字后跑一次全量编译，任何漏改的重载都会以 `already registered` 在**启动时**暴露（而非静默错绑）。
