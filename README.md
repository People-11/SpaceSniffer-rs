# SpaceSniffer-rs

用 Rust 重写的 [SpaceSniffer](http://www.uderzo.it/main_products/space_sniffer/)：高性能并行扫描 + GPU 像素化 treemap + 单个 exe。

![UI](SpaceSniffer-UI2.png)

## 功能

- 选择盘符或路径开始扫描（也可以 `spacesniffer-rs.exe D:\` 直接扫）
- **实时增量扫描**：多线程遍历，边扫边长出二维 treemap
- SpaceSniffer 式嵌套 treemap（目录标题条 `名称 - 大小`），默认显示三层；单击任意文件夹块立即展开（手风琴：每层只展开一个），0.3 秒内再点一次（双击）放大进入该文件夹；双击文件进入它所在的文件夹
- 过渡动画由点阵本身完成：几何上只有相机缩放（不重排块），内容切换是一道从点击处扩散的抖动像素波——波前像一道光，抬高经过处像素的亮度级，随后逐级冷却；标签打字机式出现/擦除，被点击文件夹的标题作为共享元素连续移动
- 文件夹统一铜色，文件按类型着色；悬停块或文件夹标题条会点亮对应的块
- 后退 / 前进（鼠标侧键）/ 上一级（Backspace）/ 根目录、工具栏面包屑导航
- F5 重新扫描当前视图（只重扫该子树；扫描进行中不可用）
- 过滤：`*.mp4; >100mb; <2gb; 关键字`（名称条件 OR，大小条件 AND），不匹配的块变暗
- 右键：进入、打开、在资源管理器中显示、复制路径、移到回收站（粒子消散动画）

## 渲染

- egui + wgpu（仅 Vulkan，需要支持 Vulkan 的显卡驱动）：surface 支持时用 `PresentMode::Mailbox` + 每帧 `DwmFlush()` 对齐合成器节奏（无 busy-wait），否则回退 Fifo。状态栏显示实际使用的模式
- 像素场是 5 级亮度的有序抖动（4×4 Bayer 矩阵），块之间靠抖动图案、亮度档和左上暗线区分；点阵、像素波和粒子都在一个 instanced WGSL shader 里完成，每个矩形 1 个实例
- 画面静止时不重绘，只在扫描、动画或输入时出帧

## 构建

```
cargo build --release
```

产物 `target/release/spacesniffer-rs.exe`（约 6 MB），静态 CRT，无额外 DLL；不内嵌字体，使用系统自带的 Segoe UI / 微软雅黑 / Segoe UI Symbol。CI 见 `.github/workflows/build.yml`。
