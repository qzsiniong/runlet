# Runlet

常驻系统托盘的**脚本管理器**：用 JavaScript / TypeScript 写脚本，脚本能读文件、做编解码与压缩、
把文本**逐键敲进当前焦点窗口**、截屏识别二维码、弹系统确认框、显示进度条。

典型用途：在只有「键盘」这一条通路的场景里（VDI / 远程桌面 / 云桌面），用脚本把数据一段段
敲进远程窗口；或者把远程屏幕上显示的二维码读回本地。

```js
// `$`（正式名 Runlet）是能力命名空间；sleep 是引擎提供的全局（要 await）
const bytes = await $.read('~/data.bin')      // 读真实文件（只接受绝对路径）
const parts = $.chunks($.zstd(bytes), 1024)   // 先压缩，再按 1 KiB 分片
$.type_str(`共 ${parts.length} 片\n`, 10)     // 逐键打进当前焦点窗口
for (let i = 0; i < parts.length; i++) {
  $.type_str(`${i}\n${$.base32_nopad(parts[i])}\n`, 10)
  await sleep(200)
}
```

---

## 目录

- [快速开始](#快速开始)
- [界面与托盘](#界面与托盘)
- [脚本目录](#脚本目录)
- [运行脚本](#运行脚本)
- [能力一览](#能力一览)
- [文件系统的三条规则](#文件系统的三条规则)
- [Console 面板](#console-面板)
- [编辑器](#编辑器)
- [TypeScript 支持范围](#typescript-支持范围)
- [配置](#配置)
- [深链接](#深链接)
- [安全说明](#安全说明)
- [项目结构](#项目结构)
- [构建](#构建)
- [测试](#测试)

---

## 快速开始

要求 Rust stable（edition 2021）+ Node.js 18+ + pnpm。

```bash
pnpm install

# 开发（Tauri 自动起 Vite dev server + 编译 Rust + 托盘应用）
pnpm tauri dev

# 发布打包
pnpm tauri build

# macOS：打包成可双击的 .app（推荐；启动无终端窗口）
scripts/package-macos.sh              # 产物在 src-tauri/target/release/bundle/macos/
scripts/package-macos.sh --install    # 额外安装到 /Applications
```

**macOS 权限**（系统设置 → 隐私与安全性，授权后重启程序生效）：

| 权限 | 用途 |
|---|---|
| 辅助功能 | 模拟键盘输入（`$.type_str`）与识别焦点窗口 |
| 屏幕录制 | 截屏解码二维码（`$.scan_qr`） |
| 通知 | 任务完成 / 失败提示 |

---

## 界面与托盘

程序常驻菜单栏 / 系统托盘（macOS 平时不占 Dock，脚本窗口可见期间才临时出现 Dock 图标，
方便 Cmd+Tab 切回编辑器）。

**托盘菜单**：

```
状态：空闲 / 运行脚本…
────────────────
脚本编辑器…        ← 打开脚本窗口
开机时启动         ← 勾选项，写系统登录项
────────────────
退出 Runlet
```

**窗口**：应用只有一个主窗口（脚本编辑器），没有总览页和设置页。

| 窗口 | 说明 |
|---|---|
| 脚本窗口 | 1100×760：左侧脚本列表，右侧「工具栏 + 编辑器 + Console」。工具栏可置顶、刷新列表、打开脚本目录 |
| 待命窗口 | 首次键盘输出前弹出，等你把焦点切到目标窗口 |
| 进度窗口 | `$.progress*` 的渲染目标，附带「中止」按钮 |

启动即打开脚本窗口；如果这次启动来自登录项（开机自启），则静默只留托盘图标。
关窗 = 隐藏到托盘，不退出进程。

---

## 脚本目录

```
macOS   ~/Library/Application Support/Runlet/scripts/
Windows %APPDATA%\Runlet\scripts\
Linux   ~/.config/Runlet/scripts/
```

- **首次启动写入四个内置示例**：`01-quick-start.js`、`02-ts-demo.ts`、`03-pack-and-shard.ts`、
  `04-file-transfer.ts`。**已存在的文件永不覆盖** —— 你可以放心改示例，下次启动不会还原。
- **类型声明每次启动都覆盖**：`engine.d.ts`、`runlet.d.ts`、`tsconfig.json` 是随二进制走的
  产物，必须与运行期一致。有了它们，你在任意编辑器里打开脚本目录写 `.ts` 都有提示。
- 支持的扩展名：`.js` `.mjs` `.cjs` `.ts` `.mts` `.cts`。
- 脚本是用户数据，放在系统配置目录（与 `config.json` 同级），升级 / 重装不会碰到它。

---

## 运行脚本

打开脚本窗口 → 选择或新建脚本 → 点「运行」（`⌘⏎` / `Ctrl+Enter`）。有未保存改动时会先问一次。

**待命窗口是惰性的**：脚本不一定注入键盘事件（例如只读文件、算摘要的脚本），所以 ——

- 脚本执行到**第一次** `$.type_str`（或静态分析就能看出它会输出）时才弹待命窗口，等你点击目标窗口；
- 纯计算脚本**不会有任何待命提示**，直接跑完；
- 同一次运行里只提示一次，后续 `$.type_str` 不再重复打扰。

待命窗口有 10 秒倒计时，超时即取消任务。进度窗口显示「已敲入 N/M 字符 + 当前输出片段」，
按 **Esc**（或点「中止」）随时停下 —— 中止热键只在任务运行期间注册，空闲时不拦截任何键。

### 焦点锁定与重新确认

一次待命成功 = **锁定**一个目标窗口。之后每次输出前都会校验这个锁（打字过程中每 250ms 一次）：

- **焦点没变** → 直接继续打，不再打扰；
- **焦点换了窗口** → 立刻停手，稍等复查一次（吸收系统通知这类瞬时抖动），确认真的换了就
  **自动重新弹出待命窗口**；你重新点好目标窗口后，输出**从断点继续**（已输入的字符不会重复）；
- 脚本要**中途**换输出目标（例如先打进浏览器、再打进记事本）时，显式调用
  `$.request_focus("请点击远程记事本")`：立刻重开一轮待命并显示这句提示（焦点没变也会弹）；
- 待命窗口的**「暂停检测」**用于「要点好几次才能把焦点切到位」的场景：暂停期间点击任何窗口都
  不会开始输出，倒计时与 10 秒超时一起挂起（「取消」与 Esc 始终可用）。

焦点识别是**窗口级**的：macOS 走系统级 AX API（拿不到时回退到「前台应用」级别），Windows 走
`GetForegroundWindow`；两者都不看窗口标题，所以浏览器切标签、文档改名不会被误判成换窗口。
其它平台不做检测。

---

## 能力一览

凡是接收二进制的能力都接受三种入参：`string`（按 UTF-8 编码）、`ArrayBuffer`、任何
`ArrayBufferView`（`Uint8Array` / `DataView` …，按 `byteOffset` + `byteLength` 取）。

`$` 是 `Runlet` 的别名，指向同一个冻结对象（脚本改不了能力，`$.md5 = null` 会直接报错）。
不要写 `const $ = Runlet`（会报重复声明）。

| 能力 | 说明 |
|---|---|
| `$.bytes(data)` | 统一成 `ArrayBuffer`（字符串按 UTF-8） |
| `$.str(data, encoding?)` | 字节 → 文本（默认 `utf-8`，支持全部 WHATWG 标签） |
| `$.chunks(data, chunkSize?)` | 分片（默认 1024）：字符串按**码点**切返回 `string[]`，二进制按**字节**切返回 `ArrayBuffer[]` |
| `$.hex` / `$.hex_upper` / `$.hex_decode` | 十六进制编解码（解码大小写均可） |
| `$.base32` / `_nopad` / `_lower` / `_lower_nopad` + 对应 `*_decode` | RFC4648 Base32，**严格**校验大小写与填充 |
| `$.base64` / `_nopad` / `_url` / `_url_nopad` + 对应 `*_decode` | 标准与 URL-safe 盘表，各自带 / 不带填充 |
| `$.md5(data, format?)` | MD5：`hex`（默认，**小写**）/ `hex_upper` / `base32`（26 字符）/ `base32_lower` / `base64`（22 字符） |
| `$.crc32(data, format?)` | CRC-32（大端）：`hex`（默认，**大写**）/ `hex_lower` / `base32` / `base32_lower` / `base64` |
| `$.zstd(data, level?)` | Zstandard 压缩（默认级别 3）；只压缩不分片 |
| `$.gzip` / `$.gunzip` | gzip 压缩（默认 6）与解压 |
| `$.brotli` / `$.unbrotli` | Brotli 压缩（默认质量 5）与解压 |
| `$.lzma` / `$.unlzma` | xz（LZMA2）压缩与解压，产物兼容 `xz` / `7z` / `tar -J` |
| `$.read` / `$.read_text` | 读文件（字节 / 带编码的文本），只读不询问 |
| `$.exists` / `$.stat` / `$.list` | 存在性、元信息（大小 / 类型 / 修改时间）、目录条目 |
| `$.write` / `$.write_text` / `$.append` / `$.append_text` | 写 / 覆盖、追加（**会先询问**；父目录需已存在） |
| `$.mkdir` / `$.remove` / `$.rename` / `$.copy` | 建目录 / 删除 / 改名移动 / 复制（目录递归），**都会先询问** |
| `$.basename` / `$.dirname` / `$.extname` / `$.stem` | 路径字符串计算（同时认 `/` 与 `\`，不读盘） |
| `$.format_duration(seconds)` | 秒 → `45s` / `1m23s` / `1h2m3s`（**单位是秒**，毫秒先 `/1000`） |
| `$.format_bytes(bytes)` | 字节 → `512B` / `1.5KB` / `2.00MB` / `1.50GB` |
| `$.type_str(text, delayMs?, onProgress?)` | 把文本逐键打进**当前焦点窗口**；`onProgress(done, total)` 报告输出进度（时间节流：首尾必达，中间最多每 1 秒一次）；被中止时抛异常终止脚本 |
| `$.request_focus(hint?)` | 请求用户把焦点切到目标窗口并等待确认（中途换目标时用） |
| `$.confirm(message)` | 弹**系统原生**确认框：是 → `true`，否 → `false`，取消 / 中止 → 抛异常（不设超时） |
| `$.pick_path(prompt?, kind?)` | 弹**系统原生**选择框挑文件或文件夹（`kind`：`"file"` 默认 / `"dir"`），返回绝对路径或 `null` |
| `$.scan_qr()` | 截屏**一次**并解出画面里的第一条二维码文本；没扫到或环境不支持时返回 `null`。不轮询、不超时、不认识任何协议 |
| `$.progress(total?, msg?)` / `$.progress_inc` / `_set` / `_total` / `_msg` / `_finish` / `_abandon` | 脚本进度条（每任务一条，重复调用 = 重置；任务结束宿主自动收口） |
| `console.log/info/debug/warn/error` | 写到脚本窗口底部的 Console 面板（按等级着色） |
| `sleep(ms)` | 异步等待（**必须 `await`**）；等待期间被中止会立即返回 |
| `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` | 定时器，只属于本次运行，脚本结束时统一清理 |
| `performance.now()` / `structuredClone(v)` | 单调时钟 / JSON 语义深拷贝 |
| `atob` / `btoa` | WHATWG 语义的 base64（只处理 Latin-1 字符串） |
| `TextDecoder` / `TextEncoder` | 支持全部 WHATWG 编码标签（`gbk` / `gb18030` / `big5` / `shift_jis` …） |

脚本里可以直接使用**顶层 `await`**，不需要包 `async` IIFE。

---

## 文件系统的三条规则

1. **只接受绝对路径**。`~`（主目录）、`C:/x`、`C:\x`、git-bash 的 `/d/x`、Cygwin 的
   `/cygdrive/d/x` 都算绝对路径；相对路径直接报错 —— 脚本的工作目录取决于宿主怎么启动，
   放行相对路径等于埋雷。
2. **修改要问一次**。只读操作（`read` / `read_text` / `exists` / `stat` / `list`）不问；
   写、追加、建目录、删除、改名、复制每次都会弹一个三按钮系统框：
   `允许一次` / `本次运行内该目录都允许` / `拒绝`。「本次运行内都允许」记在**本次运行**里，
   运行结束即失效。
3. **选路径不等于放行**。`$.pick_path` 只把路径字符串给你：`read` 本来就不询问，
   写 / 删 / 改名 / 复制依旧会弹授权框（第 2 条）。

---

## Console 面板

脚本窗口底部是 Console 面板，`console.log/info/debug/warn/error` 出现在这里：

- 按等级着色（error 红、warn 琥珀、debug 灰），每行带时间；
- 面板里还有运行状态行：`▶ 运行 xxx`、`✓ 完成(输出 N 个字符,用时 Xms)`；脚本报错时把引擎的
  错误原文（含文件名与行列）写成 error 行，中止时写 warn 行；
- 每次运行前清空 —— 面板只属于**这一次**运行；
- 上限 1000 行，可按等级过滤、可手动清空；
- **高度可调**（拖分隔条，方向键也能微调），高度记在 localStorage；
- **可折叠 / 可最大化**：⌄ 折叠成一条标题栏，⤢ 让 Console 占满编辑区（编辑器不会卸载，
  撤销历史与光标位置都还在）；
- **自动跟随最新输出**：手动往上翻时暂停跟随，滚回底部自动恢复；
- 日志存在后端，关闭再打开脚本窗口时自动恢复。

---

## 编辑器

脚本窗口里的编辑器用 CodeMirror 6（高亮 / 缩进来自 lezer），**语义能力全部来自一个进程内的
TypeScript 语言服务** —— 补全、悬停、参数信息、诊断问的是同一个服务、同一份虚拟文件表：

| 能力 | 说明 |
|---|---|
| 补全 | `Runlet.` / `$.` 列出全部能力（带签名与 JSDoc），变量后列出成员，标识符位置列出作用域内变量 |
| 悬停 | 显示类型签名 + JSDoc，例如 `(method) Runlet.md5(data: BinaryInput, format?: ...): string` |
| 参数信息 | 调用括号里显示签名并高亮当前参数 |
| 诊断 | 类型不匹配、拼错方法名、用了运行时不存在的 API 都会标红 / 标黄 |
| 编辑体验 | 括号匹配 / 自动闭合、自动缩进、搜索、`⌘⏎` 运行、`⌘S` 保存 |

### 快捷键

脚本窗口右上角的 ⌨ 按钮给出完整清单（内容与编辑器真实绑定共用同一份键位串，不会漂移）。
macOS 显示 ⌘/⇧/⌥/⌃，Windows/Linux 显示 Ctrl/Shift/Alt：

| 分组 | 动作 | macOS | Windows / Linux |
|---|---|---|---|
| 运行与保存 | 运行脚本 | `⌘⏎` | `Ctrl+Enter` |
| 运行与保存 | 保存脚本 | `⌘S` | `Ctrl+S` |
| 编辑 | 撤销 / 重做 | `⌘Z` / `⌘⇧Z` | `Ctrl+Z` / `Ctrl+Y` |
| 编辑 | 注释 / 取消注释 | `⌘/` | `Ctrl+/` |
| 编辑 | 增加 / 减少缩进 | `⌘]` / `⌘[` | `Ctrl+]` / `Ctrl+[` |
| 编辑 | 整行上移 / 下移 | `⌥↑` / `⌥↓` | `Alt+↑` / `Alt+↓` |
| 编辑 | 删除整行 | `⇧⌘K` | `Shift+Ctrl+K` |
| 查找 | 查找 | `⌘F` | `Ctrl+F` |
| 查找 | 下一个 / 上一个匹配 | `⌘G` / `⌘⇧G` | `Ctrl+G` / `Ctrl+Shift+G` |
| 查找 | 选中下一个相同词 | `⌘D` | `Ctrl+D` |
| 查找 | 跳到行 | `⌥⌘G` | `Ctrl+Alt+G` |
| 补全与提示 | 触发补全 | `` ⌥` ``（Ctrl-Space 在 mac 常被输入法占用） | `Ctrl+Space` |
| 补全与提示 | 接受候选 / 关闭面板 | `⏎` / `Esc` | `Enter` / `Esc` |

TypeScript 语言服务是**懒加载**的：首次打开脚本窗口时后台预热，就绪前补全退回「能力清单」版本、
悬停不弹框；加载失败也不影响编辑与运行。

编辑器里的类型来自两个声明文件（`crates/script-engine/src/spec/engine.d.ts` 与
`crates/runlet-scripting/src/spec/runlet.d.ts`），它们同时被 `pnpm typecheck:scripts`
（显式列举）与 `pnpm typecheck:examples`（`crates/tsconfig.json`，按通配覆盖 seed）使用，
因此**编辑器里的断言与命令行检查一致**。

### 脚本窗口里的确认框

切换脚本 / 新建 / 删除会丢改动，以及「有未保存修改时是否先保存再运行」，统一走
`@tauri-apps/plugin-dialog` 的 `ask()`，也就是**系统原生 Yes/No 弹框** —— 与脚本里
`$.confirm` 的观感一致。这里不能用 `window.confirm()`（Tauri 会把它接管成插件调用，
而权限里只放开了 `dialog:allow-ask`）。

---

## TypeScript 支持范围

`.ts` / `.mts` / `.cts` 会被自动转译（oxc，进程内，**不需要 Node / tsc**）：

| 支持 | 说明 |
|---|---|
| 类型注解 / `interface` / `type` / `as` / 泛型 | 直接剥掉 |
| `enum` / `namespace` | 转成运行时对象 |
| 顶层 `await` | 脚本按 module 解析，因此允许 |
| 现代 JS 语法 | 原样保留，不做 ES 降级 |

| 不支持 | 说明 |
|---|---|
| JSX / TSX | 没有 React 运行时，`.tsx` 会明确报错 |
| 装饰器等需要 helper 的语法 | 明确报错，而不是生成跑不起来的代码 |
| ESM 的 `import` / `export` 值导入 | 运行时是「全局 + async eval」，`import type` 会被移除 |
| source map | 运行期报错的行列号指向转译后的 JS；语法错误不受影响 |

---

## 配置

没有设置界面。可调项分两类：

| 位置 | 项 | 说明 |
|---|---|---|
| 托盘菜单 | 开机时启动 | 勾选状态读系统登录项（你在系统设置里关掉它，菜单也会跟着取消勾选） |
| `config.json` | 中止热键、逐键间隔、settle、进度显示、按键真实发送 | 见下表，手改后重启生效 |

```json
{
  "stop_hotkey": "Esc",
  "key_delay_ms": 3,
  "settle_ms": 200,
  "progress_display": "floating",
  "send_real_keys": true
}
```

| 字段 | 默认 | 说明 |
|---|---|---|
| `stop_hotkey` | `Esc` | 任务运行期间注册的中止键（空闲时不注册任何键） |
| `key_delay_ms` | 3 | `$.type_str` 每敲一个字符后的等待（0~100） |
| `settle_ms` | 200 | 开始打字前等修饰键抬起（0~5000） |
| `progress_display` | `floating` | `floating` 悬浮条 / `tray` 托盘图标 / `both` 两者同时 |
| `send_real_keys` | `true` | 关掉后只走进度不注入真实按键（演练用） |

文件位置：`<系统配置目录>/Runlet/config.json`（macOS 为 `~/Library/Application Support/Runlet/`）。
文件缺失或损坏时回退默认值；手改出的越界值（如 `key_delay_ms: 100000`）会在读入时夹回
合法区间，免得每次按键睡两分钟。

---

## 深链接

应用注册了 `runlet://` 协议，可以从浏览器、终端、其他应用里直接把动作交给 Runlet：

| 链接 | 动作 |
|---|---|
| `runlet://open` | 唤起（或聚焦）脚本窗口 |
| `runlet://scripts` | 唤起窗口并聚焦脚本列表 |
| `runlet://edit/<脚本名>` | 唤起窗口并选中该脚本（不运行） |
| `runlet://run/<脚本名>` | **弹系统确认框**，确认后才运行 |

```html
<!-- 网页里一行链接就能打开对应脚本 -->
<a href="runlet://edit/01-quick-start.js">在 Runlet 中打开</a>
```

```bash
# 终端（macOS / Linux）
open "runlet://run/01-quick-start.js"
```

几个要点：

- **运行一定确认**：链接可能来自任意网页，而脚本能读文件、能往当前焦点窗口逐键打字。
  所以 `runlet://run/...` 一律先弹原生确认框，把脚本名与来源 URL 摆出来，确认后才执行；
  其余三种动作只影响界面，直接执行。
- **脚本名走白名单**：与脚本目录同一套规则（拒 `..`、路径分隔符、非法扩展名），
  链接里的 `%E4%B8%AD` 这类转义会被解码，中文/带空格的文件名可用。
- **冷启动去重**：应用被链接拉起时，启动路径可能重复投递同一个 URL，3 秒内的重复会跳过。
- **平台差异**：macOS 只有打包并安装到 `/Applications` 后系统才会把 `runlet://` 交给应用，
  `pnpm tauri dev` 下测不到；Windows/Linux 开发时会在启动阶段临时注册 scheme（`register_all`），
  可以直接在浏览器地址栏输入 `runlet://open` 试。
- 已有实例在跑时，第二次启动会转交给它（单实例插件），不会开出第二个 Runlet。

## 安全说明

脚本拥有**与 Runlet 同等的本机权限**：`$.read` 可以读任意**绝对路径**，`$.type_str` 会把
内容敲进当前焦点窗口。读操作没有沙箱也没有提示；写 / 删 / 改名 / 复制 / 建目录每次都会弹系统框
确认（可以放行「本次运行内该目录」）。请只运行你信任的脚本。

---

## 项目结构

```
crates/script-engine/          可嵌入的脚本引擎：QuickJS + oxc 进程内转译、标准全局
                               （sleep / console / TextDecoder / 定时器 / atob…）、
                               扩展机制、命名空间配置（名字由使用方给）
crates/runlet-scripting/     使用方能力集：编解码 / 摘要 / 压缩 / 文件系统 / 宿主交互，
                               ScriptHost 接口、脚本目录与内置示例、类型声明
src-tauri/                     Tauri 2 应用：托盘、窗口、命令、Worker 任务、
                               Typer 键盘注入、待命门闩、Console 缓冲、配置与热键
src/                           前端（Vue 3 + Vite + TS + shadcn-vue）：脚本窗口 + 进度/待命小窗
```

引擎本身**不认识**键盘 / 文件 / 压缩，**也不给能力命名空间起名字**：它建一个匿名对象，
名字（`Runlet` / `$`）由使用方在 `RuntimeOptions::namespace` 里配置，能力再由使用方通过
`ScriptExtension` 注入（新增能力只要实现 `register` + `spec` 两个方法，并在
`src/spec/runlet.d.ts` 补声明 —— 三个真源，缺一个都会被测试或类型检查抓到）。

---

## 构建

要求 Rust stable（edition 2021）+ Node.js 18+ + pnpm。

```bash
pnpm install
pnpm tauri dev      # 开发
pnpm tauri build    # 发布打包
```

平台依赖：

- **macOS**：Xcode Command Line Tools；键盘模拟走 CoreGraphics
  （`CGEventKeyboardSetUnicodeString`，任意线程可用），通知走 Notification Center；
- **Windows**：MSVC 工具链；键盘模拟走 enigo，通知走 Toast。

### 发布新版（改版本号 + 打 tag）

版本号的**真源是 `src-tauri/tauri.conf.json`**（安装包版本就是它，`release` 工作流也用它校验 tag）：

```bash
scripts/release.sh patch                 # 改版本号 + 跑本地门禁，然后交互问要不要提交/打 tag/推送
scripts/release.sh 0.2.0                 # 指定版本（也接受 0.2.0-beta.1 这类预发布串）
scripts/release.sh minor --dry-run       # 只打印将要改什么，不写文件
```

会改：`src-tauri/tauri.conf.json`、`package.json`、`src-tauri/Cargo.toml`、
`crates/{script-engine,runlet-scripting}/Cargo.toml`，再跑一遍 `cargo check` 让 `Cargo.lock` 跟上；
随后执行与 CI 相同的门禁（`fmt` / `clippy -D warnings` / `cargo test --workspace` / `pnpm lint` /
`typecheck:scripts` / `test:editor` / `build`，`--no-check` 可跳过）。

---

## 测试

```bash
# 整个 workspace
cargo test --workspace

# 三个不依赖图形栈的 crate 是 clippy 干净的（src-tauri 有一批历史告警）
cargo clippy -p script-engine -p runlet-scripting --all-targets -- -D warnings

# 只跑引擎 / 能力集（第一次编译 QuickJS 的 C 代码较慢，之后很快）
cargo test -p script-engine
cargo test -p runlet-scripting

# Tauri 侧
cd src-tauri && cargo test

# 前端
pnpm build
pnpm typecheck:scripts      # 示例与声明文件
pnpm typecheck:examples     # 编辑器项目：按通配覆盖 seed 下的示例
pnpm lint
pnpm test:editor            # 编辑器语义功能与快捷键格式化的单测
```

`cargo test -p script-engine` 覆盖 `sleep` / 取消 / 定时器 / `atob`·`btoa` / `structuredClone`、
`TextDecoder` 的 GBK·BOM·fatal、扩展机制的注册与重名、命名空间校验、TS 转译与端到端；
`cargo test -p runlet-scripting` 覆盖入参多态、编码往返与严格解码、md5·crc32 与 Rust 侧独立实现
对齐、压缩往返、文件读写与**授权策略**（拒绝 / 本次运行放行 / 跨运行失效）、`type_str` 与
`confirm` 的宿主语义、脚本目录与内置示例。

`cargo test -p runlet-scripting --test transfer_e2e` 是**文件通道的端到端守卫**：跑真正的
`04-file-transfer.ts`，再用独立重写的解析器把宿主敲出的帧解回原始字节、与源文件逐字节比对
（覆盖正常完成、续传含中间空洞、读不到反馈时降级盲发、页面报错四条路径）。

### 在浏览器里调前端

Tauri 的 webview 才会注入 `window.__TAURI_INTERNALS__`，所以 dev 模式会自动装一层 Tauri mock
（`src/dev/tauri-mock.ts`）：

```bash
pnpm dev
# 路由是 hash 模式，与 Tauri 窗口 URL 一致
http://localhost:5173/            # 脚本窗口（`/` 重定向到 `/scripting`）
http://localhost:5173/#/scripting # 脚本窗口
http://localhost:5173/#/progress  # 进度（裸布局）
http://localhost:5173/#/standby   # 待命（裸布局）
```

mock 覆盖不到的（要真机验证）：真实键盘注入、文件系统授权、屏幕截取与二维码识别、系统原生弹框。
