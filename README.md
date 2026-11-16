# SpaceSniffer-rs

极致流畅的 [SpaceSniffer](http://www.uderzo.it/main_products/space_sniffer/) 继任者：Rust 编写，并行扫描，GPU 渲染的像素化 treemap，缩放与像素波过渡全程丝滑，单个 exe 即开即用。

## 功能

- 选择盘符或路径开始扫描（也可以 `spacesniffer-rs.exe D:\` 直接扫）；从选盘界面进入扫描、退回选盘界面都有同款缩放 + 像素波过渡，退回途中点盘可直接进入
- **实时增量扫描**：多线程遍历，边扫边长出 treemap；线程数取核心数的一半、优先级低于正常，扫描时不拖慢系统
- 盘符立即列出，各盘容量在后台读取：响应慢或已断开的网络驱动器不会卡住界面
- SpaceSniffer 式嵌套 treemap（目录标题条 `名称 - 大小`），默认显示三层；单击任意文件夹块立即展开（手风琴：每层只展开一个），0.3 秒内再点一次（双击）放大进入该文件夹；双击文件进入它所在的文件夹
- 过渡动画由点阵本身完成：几何上只有相机缩放（不重排块），内容切换是一道从点击处扩散的抖动像素波——波前像一道光，抬高经过处像素的亮度级，随后逐级冷却；标签打字机式出现/淡出，被点击文件夹的标题作为共享元素连续移动
- 动画可以随时打断，不重置也不跳过：新的过渡叠在正在进行的上面（旧波继续跑完）；镜头保持当前速度，按缩放结尾同样的缓动减速停下，再转向新目标
- 文件夹统一铜色，文件按类型着色；悬停块或文件夹标题条会点亮对应的块，点击后高亮保持到像素波扫过为止
- 后退 / 前进（鼠标侧键）/ 上一级（Backspace）/ 根目录、工具栏面包屑导航
- F5 重新扫描当前视图（只重扫该子树；扫描进行中不可用）
- 右键：进入、打开、在资源管理器中显示、复制路径、移到回收站（粒子消散动画）

## 渲染

- egui + wgpu（仅 Vulkan，需要支持 Vulkan 的显卡驱动，优先独显）：`PresentMode::Fifo`（Vulkan 的 Fifo 是真正阻塞等待垂直同步，不像 OpenGL 在 NVIDIA 上会忙等）
- 像素场是 5 级亮度的有序抖动（4×4 Bayer 矩阵），块之间靠抖动图案、亮度档和左上暗线区分；小到画不出暗线的文件统一收进所在文件夹右下角的一块，这一块共用一条左上暗线；点阵、像素波和粒子都在一个 instanced WGSL shader 里完成，每个矩形 1 个实例
- 每个点阵单元只算一次：先按「一个单元一个像素」渲染到离屏纹理，再放大贴到屏幕，4K 下着色量只有全分辨率的 1/9–1/16；不覆盖任何单元中心的块（缩小到看不见、或移出屏幕的）直接剔除
- 画面静止时不重绘，只在扫描、动画或输入时出帧；状态栏显示显卡、帧率和每帧 CPU 耗时
- 拖动标题栏只移动窗口、不重绘：纯移动的 `WM_WINDOWPOSCHANGED` 不交给 winit（见 `win::setup_window`），否则每一步都要出一帧并等垂直同步，窗口会跟不上鼠标

## 构建

```
cargo build --release
```

产物 `target/release/spacesniffer-rs.exe`（约 4.8 MB），静态 CRT，无额外 DLL；需要支持 AVX2 的 CPU（x86-64-v3：Intel Haswell / AMD Zen 及以后，不含没有 AVX2 的 Pentium/Celeron/Atom），见 `.cargo/config.toml`；不内嵌字体，使用系统自带的 Segoe UI / 微软雅黑 / Segoe UI Symbol，工具栏图标用 Segoe Fluent Icons（Windows 10 上用 Segoe MDL2 Assets）。CI 见 `.github/workflows/build.yml`。

`cargo build --release --features perf` 构建带控制台的性能调试版，每秒输出各阶段的帧耗时。

体积相关：
- 窗口图标由 `build.rs` 把 `assets/icon.ico` 编进 exe 资源，运行时从资源加载（Windows 自己解码），不在程序里解码 PNG；GNU 工具链需要 `windres`（随 MinGW gcc 提供），否则没有图标。
- eframe 只为图标依赖 `image`（png/deflate/crc32），剪贴板库 arboard 也只为图片剪贴板依赖它，所以用 `[patch]` 换成了 `stub/image` 存根：图片复制粘贴不可用，文本剪贴板不受影响。升级 eframe/arboard 时若存根编译失败，按报错补齐签名。
