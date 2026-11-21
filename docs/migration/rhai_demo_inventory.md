# rhai_demo 宿主与脚本精确清单（迁移评估数据底座）

> 采集日期：2026-11-21。采集方式：Explore 代理全量 grep + 抽读，行号为当时工作区实际行号。
> 采集对象：`D:\abc\rustrust\rhai_demo`（rhai 1.26.1，`Cargo.toml:77` `rhai = "1"` 默认特性）。

**总量口径**
- Rust 主程序 `src/`（不含 bin）：约 29.5k 行；`.rhai` 脚本 34 个（`scripts/` 33 个 + `scripts/lib/common.rhai`），共 6905 行。
- **`engine.register_fn(` 调用点共 229 个**：主宿主 224 个（`src/engine.rs` 2 + `src/toolkit/*` 222），教学 bin `src/bin/demo_tour.rs` 5 个（独立 Engine，与主程序无关）。`src/bin/check_scripts.rs` 无注册。
- 另有 `register_get_set/register_get` 属性 5 个、`register_type_with_name` 2 个（Task、Page）。
- 去掉同名重载后，主宿主暴露给脚本的**唯一函数名约 197 个**（27 个额外调用点为重载：draw_* ×10、msg_box ×3、show_table ×2、beep/flash/toast/swipe/sleep/browser_start/img_stable/stamp/date_parts/date_ms/clamp 各 ×1）。

## 1. 宿主暴露函数完整清单（按文件分组，全量不抽样）

返回类型标注：**`rhai::Map`**、**`rhai::Array`**、**`Dynamic`**、**`String`** 为对 brook 有直接影响的类型。所有 `Result<T, ScriptError>` 中的 `ScriptError = Box<rhai::EvalAltResult>`（`src/toolkit/mod.rs:367`）——错误传播走 rhai 异常通道，brook 无异常时这是结构性差异。

### src/engine.rs（2 个）
| 脚本名 | 参数 | 返回类型（脚本侧） | 行号 |
|---|---|---|---|
| `log` | 1: `&str` | `()`（副作用：打印+写日志） | engine.rs:42 |
| `print_raw` | 1: `&str` | `()`（原地刷新不换行） | engine.rs:69 |

另：`engine.on_print` 接管脚本 `print()`（engine.rs:56，非注册）。

### src/toolkit/mod.rs（1 个）
| `is_live` | 0 | `bool` | mod.rs:531 |

### src/toolkit/act.rs（27 个）——键鼠/等待/控制流
| 脚本名 | 参数个数:类型 | 返回 | 行号 |
|---|---|---|---|
| `tap` / `tap_right` / `double_click` | 2: i64,i64 | Result\<bool\> | 2439/2444/2449 |
| `swipe` | 4: i64×4 | Result\<bool\> | 2454 |
| `swipe`（重载） | 5: i64×5 | Result\<bool\> | 2462 |
| `move_to` | 2: i64,i64 | Result\<bool\> | 2470 |
| `move_rel` | 2: i64,i64 | `()`(Result\<()\>) | 2475 |
| `scroll` / `scroll_h` | 1: i64 | Result\<bool\> | 2478/2483 |
| `type_text` | 1: &str | Result\<bool\> | 2488 |
| `type_text_len` | 1: &str | Result\<i64\> | 2496 |
| `key` / `hotkey` | 1: &str | `()` | 2501/2504 |
| `hold_key` | 3: **NativeCallContext**, &str, **FnPtr(闭包)** | `()`(Result\<()\>) | 2511 |
| `hold_mouse` | 3: NativeCallContext, &str, **FnPtr(闭包)** | `()` | 2517 |
| `mouse_pos` | 0 | **Result\<Map\>**{x,y} | 2523 |
| `is_button_down` / `is_holding` / `key_tapped` / `combo_tapped` | 1: &str | Result\<bool\> | 2524-2527 |
| `is_cancelled` | 0 | bool | 2528 |
| `wait_ms` / `wait_until_ms` | 1: i64 | `()`(Result\<()\>) | 2529/2530 |
| `sleep` | 1: f64 | `()`(Result\<()\>) | 2540 |
| `sleep`（重载） | 1: i64 | `()` | 2541 |
| `stop_task` / `stop_blocked` | 1: &str | **抛不可捕获的 `EvalAltResult::Exit`**（act.rs:1900-1906），退出码 3/5 | 2544/2545 |

### src/toolkit/path.rs（7 个，path.rs:218-224）
| `path_join` | 2: &str,&str | **String** |
|---|---|---|
| `path_parent` / `path_ext` / `path_name` / `path_stem` / `path_norm` | 1: &str | **String** |
| `path_is_abs` | 1: &str | bool |

### src/toolkit/fs.rs（20 个，fs.rs:1468-1533）
| `fs_read` | 1: &str | Result\<**String**\> |
|---|---|---|
| `fs_read_enc` | 2: &str,&str | Result\<**Map**\> |
| `fs_read_bytes` | 1: &str | Result\<**Array**\> |
| `fs_write_bytes` | 2: &str, **Array** | Result\<i64\> |
| `fs_write` / `fs_append` | 2: &str,&str | Result\<i64\> |
| `fs_write_enc` | 3: &str×3 | Result\<**Map**\> |
| `fs_exists` / `fs_is_dir` / `fs_is_file` | 1: &str | Result\<bool\> |
| `fs_list` | 1: &str | Result\<**Array**(of Map)\> |
| `fs_walk` | 3: &str,i64,i64 | Result\<**Map**\> |
| `fs_mkdir` / `fs_remove` / `fs_move` | 1~2: &str[,bool] | Result\<bool\> |
| `fs_size` / `fs_mtime` / `fs_copy` | 1~2: &str | Result\<i64\> / Result\<i64\> / Result\<i64\> |
| `fs_copy_dir` | 2: &str,&str | Result\<**Map**\> |
| `fs_remove_dir` | 2: &str,bool | Result\<bool\> |

### src/toolkit/task.rs（4 函数 + 类型）
`register_type_with_name::<Task>("Task")`（task.rs:40）；属性 `title`(get/set,String)、`steps`(get,i64)、`errors`(get,String)（task.rs:43-49）。
| `new_task` | 1: &str | **Task(不透明句柄)** | 52 |
|---|---|---|---|
| `step` | 方法(&mut Task, &str) | i64 | 53 |
| `fail` | 方法(&mut Task, &str) | `()` | 54 |
| `summary` | 方法(&mut Task) | **String** | 55 |

### src/toolkit/util.rs（21 个，util.rs:693-744）
| `rnd` | 2: i64,i64 | Result\<i64\> | 693 |
|---|---|---|---|
| `rnd_float` | 0 | f64 | 694 |
| `rnd_pick` | 1: **&mut Array** | **Dynamic** | 695 |
| `rnd_seed` | 1: i64 | `()` | 696 |
| `epoch_ms` | 0 | Result\<i64\> | 698 |
| `stamp` | 0 | Result\<**String**\> | 699 |
| `stamp`（重载） | 1: i64 | Result\<**String**\> | 700 |
| `date_parts` | 1: i64 | Result\<**Map**\>(UTC) | 704 |
| `date_parts`（重载） | 2: i64,i64 | Result\<**Map**\> | 705 |
| `date_ms` | 6: i64×6 | Result\<i64\>(UTC) | 706 |
| `date_ms`（重载） | 7: i64×7 | Result\<i64\> | 707 |
| `span_ms` | 2: &str,i64 | Result\<i64\> | 708 |
| `dur_str` | 1: i64 | Result\<**String**\> | 709 |
| `join` | 2: **&mut Array**, &str | **String** | 714 |
| `clamp` | 3: i64×3 | Result\<i64\> | 715 |
| `clamp`（重载） | 3: f64×3 | Result\<f64\> | 716 |
| `regex_find` | 2: &str,&str | **String** | 722 |
| `regex_capture` | 2: ImmutableString×2 | **Array** | 729 |
| `regex_match` | 2: &str,&str | bool | 736 |
| `regex_replace` | 3: &str×3 | **String** | 740 |
| `regex_split` | 2: ImmutableString×2 | **Array** | 744 |

### src/toolkit/json.rs（2 个，json.rs:30/35）
| `json_parse` | 1: &str | Result\<**Dynamic**(实为 Map/Array/f64/bool/String)\> |
|---|---|---|
| `json_str` | 1: **Dynamic** | Result\<**String**\> |

### src/toolkit/vision.rs（22 个，vision.rs:1254-1331）——找图族
| `img_find` / `img_find_at` | 1 / 5: &str[,i64×4] | Result\<**Map**\>{x,y,w,h,…} | 1254/1317 |
|---|---|---|---|
| `img_has` | 1: &str | Result\<bool\> | 1257 |
| `img_wait` / `img_gone` | 2: &str,i64 | Result\<**Map**\> / Result\<bool\> | 1261/1265 |
| `img_wait_at` | 6: &str,i64×5 | Result\<**Map**\> | 1323 |
| `img_find_all` | 2: &str,i64 | Result\<**Array**\> | 1268 |
| `img_find_many` | 1: **&mut Array** | Result\<**Array**\> | 1331 |
| `img_color` | 5: i64×5 | Result\<**Array**\> | 1273 |
| `img_region` / `img_tol` / `img_interval` | 4: i64 / 1: i64 | Result\<bool\> / i64 / i64 | 1274-1277 |
| `img_load` / `img_info` | 1: &str | Result\<**Map**\>{w,h,masked,bytes} | 1281/1284 |
| `img_unload` | 1: &str | Result\<bool\> | 1291 |
| `img_unload_all` | 0 | i64 | 1292 |
| `img_screen_size` | 0 | Result\<**Map**\>{w,h,monitors,dpi_aware} | 1295 |
| `img_stable` | 1: i64 | Result\<**Map**\> | 1300 |
| `img_stable`（重载） | 2: i64,i64 | Result\<**Map**\> | 1302 |
| `img_color_at` | 2: i64,i64 | Result\<**Map**\> | 1305 |
| `img_shot` | 1: &str | Result\<**Map**\>{file,w,h,bytes,marked} | 1308 |
| `img_crop` | 5: &str,i64×4 | Result\<**Map**\> | 1311 |

### src/toolkit/window.rs（6 个，window.rs:611-618）
| `win_list` | 0 | Result\<**Array**(of Map)\> |
|---|---|---|
| `win_active` | 0 | Result\<**Map**\> |
| `win_wait` | 2: &str,i64 | Result\<**Map**\> |
| `win_gone` | 2: &str,i64 | Result\<bool\> |
| `win_activate` / `win_restore` | 1: &str | Result\<bool\> |

### src/toolkit/clip.rs（2 个，clip.rs:292-293）
| `clip_read` | 0 | Result\<**String**\> |
|---|---|---|
| `clip_write` | 1: &str | Result\<bool\> |

### src/toolkit/net.rs（6 个，net.rs:285-291）
| `http_get` / `http_post` | 1 / 2: &str[,body] | **String**（原始响应体） |
|---|---|---|
| `http_download` | 2: &str,&str | **String** |
| `net_online` | 0 | bool |
| `net_type` / `net_ip` | 0 | **String** |

### src/toolkit/ocr.rs（8 个，ocr.rs:84-96）——签名见 ocr.rs:378-496
| `ocr_screen` | 0 | Result\<**Array**(of Map)\> |
|---|---|---|
| `ocr_region` | 4: i64×4 | Result\<**Array**\> |
| `ocr_file` | 1: &str | Result\<**Array**\> |
| `ocr_find_text` | 1: &str | Result\<**Map**\> |
| `ocr_wait` | 2: &str,i64 | Result\<**Map**\> |
| `ocr_wait_at` | 6: &str,i64×5 | Result\<**Map**\> |
| `ocr_ready` | 0 | Result\<bool\> |
| `ocr_langs` | 0 | Result\<**Array**\> |

### src/toolkit/notify.rs（13 个，notify.rs:1071-1105）
| `msg_box` | 1/2/3: &str… / 4: &str×3,i64 | Result\<**String**(按钮名)\> | 1071-1080 |
|---|---|---|---|
| `beep` | 0 / 1: &str | Result\<bool\> | 1087/1088 |
| `flash` | 0 / 1: i64 | Result\<bool\> | 1090/1091 |
| `toast` | 2: &str,&str | Result\<**Map**\> | 1093 |
| `toast`（重载） | 3: &str×3 | Result\<**Map**\> | 1096 |
| `toast_setup` / `toast_status` / `toast_uninstall` | 0 | Result\<**Map**\> | 1103-1105 |

### src/toolkit/draw.rs（24 个，draw.rs:642-727）——屏幕浮层
| `rgb` | 3: i64×3 | i64 | `rgba` 4: i64×4 | i64 | 642/643 |
|---|---|---|---|---|---|
| `draw_line` / `draw_rect` / `draw_rect_fill` / `draw_circle` / `draw_circle_fill` / `draw_point` / `draw_cross` / `draw_text` / `draw_path` / `draw_polygon` | 各 2 个重载（4~6 参：i64 / **Array** 坐标 / String 文本） | Result\<bool\> | 646-721 |
| `draw_clear` / `draw_hide` | 0 | Result\<bool\> | 726/727 |

### src/toolkit/sheet.rs（4 个，sheet.rs:350-361）
| `sheet_names` | 1: &str | Result\<**Array**\> |
|---|---|---|
| `sheet_rows` | 2: &str,&str | Result\<**Array**(of Map)\> |
| `sheet_cell` | 4: &str,&str,i64,i64 | Result\<**Map**\> |
| `sheet_range` | 7: &str,&str,i64×5 | Result\<**Array**(of Array of Map)\> |

### src/toolkit/system.rs（11 个，system.rs:1276-1341）
| `device_info` | 0 | **Map**（非 Result，system.rs:1276） |
|---|---|---|
| `open_with` | 1: &str | Result\<bool\> |
| `exec` | 1: **Map**{argv,cwd,env,…} | Result\<**Map**\> |
| `run_plugin` | 1: **Map** | Result\<**Map**\> |
| `launch` | 1: &str | Result\<bool\> |
| `show_html` | 2: &str,&str | Result\<**Map**\> |
| `show_table` | 2/3/4: **Array**,&str[,note[,**Array**]] | Result\<**Map**\> |
| `show_text` | 2/3: &str… | Result\<**Map**\> |

### src/toolkit/log.rs（5 个，log.rs:113-119）
| `log_begin` | 1: &str | Result\<**Map**\> |
|---|---|---|
| `log_info` / `log_warn` / `log_error` | 1: &str | `()` |
| `log_end` | 0 | bool |

### src/toolkit/browser.rs（39 个 + 类型，browser.rs:3359-3582）
`register_type_with_name::<Page>("Page")`（browser.rs:3359，不透明句柄，方法首参 `&mut Page`）。
| `browser_start` | 0 / 1: &str | Result\<bool\> | 3367/3371 |
|---|---|---|---|
| `browser_connect` / `browser_connected` / `browser_download_dir` | 0~1: &str | Result\<bool\> | 3375/3378/3582 |
| `browser_targets` | 0 | Result\<**Array**(of Map)\> | 3382 |
| `browser_page` / `browser_open` / `browser_wait_new_page` | 1: &str / 2: **Array**,i64 | Result\<**Page**\> | 3384-3395 |
| `title` / `url` | 方法(&mut Page[,]) | Result\<**String**\> | 3400/3401 |
| `text` / `exists` / `find` | 方法(&mut Page,&str) | Result\<**String**\> / bool / **Map**{found,hits,x,y,w,h,text} | 3402-3409 |
| `pick_css` / `pick_xpath` | 方法(&mut Page,&str) | Result\<**Map**\> | 3437/3440 |
| `pick_all` | 方法(&mut Page,&str) | Result\<**Array**\> | 3443 |
| `hit_at` | 方法(&mut Page,i64,i64) | Result\<**Map**\> | 3446 |
| `wait` / `wait_gone` / `wait_url` / `wait_css` / `wait_xpath` | 方法(&mut Page,&str[,spec],i64) | Result\<bool\> | 3451-3481 |
| `click` / `click_css` / `click_xy` / `scroll_to` / `type_css` / `press` / `close` | 方法(&mut Page,…)，受 live 闸门 | Result\<bool\> | 3491-3524 |
| `click_css_ready` / `click_xpath_ready` | 方法(&mut Page,&str,i64) | Result\<bool\> | 3497/3502 |
| `download` / `download_css` | 方法(&mut Page,&str,i64) | Result\<**Map**\> | 3530/3538 |
| `click_dialog` / `click_css_dialog` | 方法(&mut Page,&str,bool,i64) | Result\<**Map**\> | 3547/3552 |
| `select_css` | 方法(&mut Page,&str,&str) | Result\<bool\> | 3560 |
| `eval_js` | 方法(&mut Page,&str,i64) | Result\<**Map**\>{ok,value/error,ms}（JS 值经 `json_to_dynamic` 转 Dynamic，browser.rs:796） | 3567 |
| `screenshot` | 方法(&mut Page,&str) | Result\<**Map**\> | 3573 |

### src/bin/demo_tour.rs（5 个，独立教学 bin，非主宿主）
`double(i64)->i64`(:57)、`deposit(&mut Account,i64)->()`(:87)、`get_balance(&mut Account)->i64`(:88)、`tap(i64,i64)->()`(:112)、`find_image(&str)->bool`(:115)；另有 `register_get_set("name")`、`register_get("balance")`（:81-86）。

**Map/Array/Dynamic/String 返回函数统计**：主宿主 224 个注册中，返回 `rhai::Map` 的约 45 个（vision/window/ocr/notify/system/fs_enc/browser 大族）、返回 `Array` 的约 22 个、参数或返回吃 `Dynamic` 的 3 个（json_parse/json_str/rnd_pick）、返回裸 `String` 的约 20 个、**入参吃 Map 的 2 个（exec/run_plugin）+ Array 的 6 个（fs_write_bytes/img_find_many/rnd_pick/join/draw_path/draw_polygon/show_table/browser_wait_new_page）**。不透明句柄类型 2 个（Task、Page）+ FnPtr 闭包 2 处（hold_key/hold_mouse）。

## 2. Engine 构建配置

**构建点唯一**：`src/engine.rs:15` `pub fn build(cfg: &Config) -> Engine`。
- `Engine::new()`（engine.rs:16）——**不是** `new_raw`；无 `register_global_module`、无 `register_static_module`、无自定义包。
- 工具注册单一入口 `toolkit::register_all`（engine.rs:25 → mod.rs:524-618），模块顺序：act/path/fs/task/util(FROZEN)/json/vision/window/clip/net/ocr(FROZEN)/notify/draw/sheet(FROZEN)/system/log/browser。
- **模块解析器**：`engine.set_module_resolver(FileModuleResolver::new_with_path(cfg.script_lib_dir))`（engine.rs:78-80）→ 支持 `import "common" as lib;`。
- **安全限制**（engine.rs:83-86）：`set_max_operations(2_000_000)`、`set_max_call_levels(64)`、`set_max_expr_depths(64,64)`、`set_max_string_size(1 MiB)`。
- `on_print` 接管脚本 print（engine.rs:56）；类型注册：`Task`（task.rs:40，含 get/set 属性）、`Page`（browser.rs:3359）。
- 未启用裁剪特性（`rhai = "1"`，锁定 1.26.1）。闭包、Map、Array、try/catch、import 全部可用且实际被脚本用到。

**脚本发现/加载/执行流程**：
1. `main.rs:470` `real_main` → `config::find_root`/`scaffold`（出厂仅内嵌 `scripts/demo.rhai`，config.rs:28 `include_str!`）→ `config::load`。
2. 脚本发现：`Config::resolve_script`（config.rs:693-702，名字→`scripts/<name>.rhai`）；`Config::list_scripts`（config.rs:725）。
3. 入口分岔：`decide_entry`（main.rs:272）→ 单次跑（`execute`）或常驻控制台（`repl::run`）；REPL 不做表达式级 eval。
4. `main.rs:307` `execute()`：重置取消旗 → `toolkit::set_live(cfg.live)` → `engine::build` → `runner::compile` 编译一次成 AST（runner.rs:17-27）→ `repeat` 轮循环调 `runner::run` → 每轮后查 `act::cancel_kind()` 决定退出码 0/3/5。
5. **入口函数名：不存在。** 整个 `.rhai` 文件即程序，top-level 语句直接执行；宿主变量以 `BTreeMap<String,String>` 经 `Scope::push` 注入（runner.rs:37-41）。
6. 辅助 bin：`check_scripts.rs`（静态扫描不执行）、`demo_tour.rs`。`phone.rs` 是 adb 遥控链路（PC 侧不建 rhai Engine）。

## 3. 脚本侧 rhai 特性使用统计（全 34 个 .rhai）

| 特性 | 用到的脚本数 | 总次数 | 示例（文件:行） |
|---|---|---|---|
| Map 字面量 `#{}` | **11** | 71 | `archive_flow.rhai:65`；`common.rhai:31` `return #{}` |
| Map 点号取值 `m.x` | ~22 | 大量 | `screen.rhai:64` `hit.x, hit.y` |
| Map 括号取值 `m["k"]` | **1**（仅 browser_task_archive） | 14 | `browser_task_archive.rhai:1259-1310` |
| 数组字面量 `[...]` | 17 | — | `diag.rhai:29` |
| `.push(` | 14 | 233 | `diag.rhai` 独占 182 次 |
| `.len()` | 19 | 115 | `browser_task_archive.rhai:1358` |
| `.filter(闭包)` | **1**（diag） | 4 | `diag.rhai:820` |
| `.map/.for_each/.reduce` | **0** | 0 | — |
| 字符串 `.split(` | 13 | 16 | `archive_flow.rhai:22` |
| `.to_string()` | 18 | 115 | `browser_task_archive.rhai:589` |
| `.trim()` / `.replace(`（**原地改、返回 unit，不能链式**） | 5 / 5 | 10 / 23 | `archive_flow.rhai:31-40`（九连 replace） |
| `.parse_int/parse_float` | 9 | 30 | `pdf_watch.rhai` 11 次 |
| `sub_string/starts_with/ends_with` | 6 | 25 | `net_probe.rhai` 12 次 |
| `.contains(` | 11 | 70 | `browser_write.rhai` 14 次 |
| `to_upper/to_lower`（原地改） | 2 | 2 | `archive_flow.rhai:34` |
| 宿主 `join(arr,sep)` | 11 | — | `browser_pick.rhai:186` |
| **try/catch** | **22/34** | 253 个 try 块 | `diag.rhai` 62+65；`archive_flow.rhai:55` |
| **闭包 `\|x\|` / `\|\|`** | **3** | 7 | `act.rhai:131,175`（hold_mouse/hold_key）；`diag.rhai:820,977,1013,1042` |
| `switch` | **0** | 0 | — |
| `for-in` | **22** | — | `archive_flow.rhai:169`。**对 Map 迭代 0 处** |
| `fn` 顶层函数定义 | 17 | — | 全为无类型标注的动态参数 |
| top-level 直跑语句（无 main） | **34/34** | — | act.rhai:203 以尾表达式作返回值 |
| `import` | 5 | — | `diag.rhai:5` `import "common" as lib;` |
| `export` | **0** | 0 | 模块函数默认公开 |
| `throw` | 3 | 3 | `diag.rhai:1095` |
| `exit()` / `debug()` / `type_of()` | 0 | 0 | — |
| 取消/控制通道 | — | — | `stop_task/stop_blocked` 抛不可捕获 `Exit`；diag/pdf_watch 用 `is_cancelled()` |

## 4. 对 Map/Array/字符串方法依赖最重的 5 个脚本

1. **`scripts/diag.rhai`（1097 行）** —— 全 API 自检矩阵：62 个 try/catch、182 次 push、4 个闭包、182 行字符串报告拼装、`throw` 作失败出口。**它是行为回归网，任何引擎替换后都要逐条重写**。
2. **`scripts/browser_task_archive.rhai`（1402 行，最大）** —— 采样任务单全流程。唯一用 `m["k"]` 括号索引的脚本（14 处）；52 try/55 catch；Page 句柄方法链 + Map 字段访问；`fails/skipped` 数组累积 + join；17 个顶层 fn；`to_string()` 27 次。
3. **`scripts/archive_flow.rhai`（500 行）** —— 35 个 Map 字面量（全仓最多）；Windows 文件名清洗 `clean_name` 依赖 **trim/replace/to_lower 的原地改语义**（archive_flow.rhai:28-47，迁移到纯函数字符串 API 时语义反转，是必踩的坑）；`fs_list` 返回的 Map 数组上做字段访问与 mtime 比较。
4. **`scripts/pdf_watch.rhai`（826 行）** —— 11 次 `parse_int`（key=value 入参解析）；`wait_hotkey` 轮询依赖"2 百万步预算能挂 50 小时"这条引擎计量假设（pdf_watch.rhai:44-46 明文依赖 engine.rs 的 set_max_operations）；probe 诊断模式多层字符串拼接 + `join`；`throw` 校验归档回读。
5. **`scripts/browser_*` 家族整体（8 个，约 1500 行）** —— 共同形态：每个 CDP 调用包 try/catch、`p.find(...)` 返回 Map 上取 `.found/.hits/.x/.y`、`fails` 数组收集断言、尾部 `fails.join(" / ")` 汇总。

次一级：`scan.rhai`/`screen.rhai`/`ocr_probe.rhai`/`ocr_rs*.rhai`（Map 字段访问密集）、`act.rhai`（hold_key/hold_mouse 闭包域唯一样本）。

## 5. 宿主消费脚本返回值的方式

`runner::run`（runner.rs:32-55）是**唯一消费点**：
1. `let result: Dynamic = engine.eval_ast_with_scope(&mut scope, ast)?`（runner.rs:45）——**只取类型，不匹配内容**。
2. 消费方式 = **纯打印**：`result.is_unit()` → 打 `()`，否则 `result.to_string()`，一行 `=> 脚本返回: X (耗时 N 毫秒)`。**不做 serde_json 序列化、不当控制流**。
3. 真正的控制流走两条带外通道：
   - `stop_task/stop_blocked` 抛 `EvalAltResult::Exit` → 退出码 3/5；
   - 普通错误 `Err` → 退出码 1。
4. 脚本→宿主的"数据"通道**只有 stdout 字符串和 output/ 落盘文件**；返回值只需"能 `to_string()`"。

**对 brook 的直接推论**：
- 宿主侧不依赖 Dynamic 的多态——但 224 个注册里 45+22+2 个返回/接收 Map/Array/Dynamic，且 11/34 个脚本用 `#{}` 字面量构造 Map 传给宿主或承接 Map 结果取字段。换成"i64/f64/bool/str/Opaque/元组"后，这些返回值要么改 Opaque 句柄 + 每字段 getter（字段种类 100+ 键），要么元组化（ADR-013 的 2 元组限制已由桥接层扩展解除，见 D-035）。
- try/catch（22/34 脚本，253 个块）是"预期内失败也要继续"的惯用法；无异常的引擎需要宿主把 Result 变成值（如 `(ok, payload)` 元组）+ 脚本重写全部分支。
- FnPtr 闭包只有 3 个脚本 7 处使用，唯一宿主消费者是 `hold_key/hold_mouse`——可用"具名函数 + 轮询 is_cancelled"替代，面积小。
- `import "common" as lib` 仅 5 个脚本用；字符串原地改方法（trim/replace/to_lower）与 `max_operations` 计量（pdf_watch 的长挂依赖）是两处隐蔽语义点。
