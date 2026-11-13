# SpaceSniffer-rs

用 Rust 重写的 [SpaceSniffer](http://www.uderzo.it/main_products/space_sniffer/)：高性能并行扫描 + GPU 像素化 treemap + 单个 exe。

![UI](SpaceSniffer-UI2.png)

## 功能

- 选择盘符或路径开始扫描（也可以 `spacesniffer-rs.exe D:\` 直接扫）
- **实时增量扫描**：多线程遍历，边扫边长出二维 treemap，新发现的文件/目录以白热高亮出现并冷却为分类颜色
- SpaceSniffer 式嵌套 treemap（目录标题条 `名称 - 大小`），默认显示三层；单击文件夹块立即展开/收起（手风琴：每层只展开一个）；双击文件夹标题条进入目录
- 过渡动画由点阵本身完成：几何上只有相机缩放（不重排块），内容切换是一道从点击处扩散的像素波——旧点缩小消失、新点白热长出、边框逐格烧出、标签打字机式出现/擦除，被点击文件夹的标题作为共享元素连续移动
- 文件夹统一沙色，文件按类型着色
- 后退 / 前进（鼠标侧键）/ 上一级（Backspace）/ 根目录、工具栏面包屑导航
- F5 重新扫描当前视图（只重扫该子树）
- 过滤：`*.mp4; >100mb; <2gb; 关键字`（名称条件 OR，大小条件 AND），不匹配的块变暗
- 右键：进入、打开、在资源管理器中显示、复制路径、移到回收站（粒子消散动画）

## 渲染

- egui + wgpu（仅 Vulkan，需要支持 Vulkan 的显卡驱动）：surface 支持时用 `PresentMode::Mailbox` + 每帧 `DwmFlush()` 对齐合成器节奏（无 busy-wait），否则回退 Fifo。状态栏显示实际使用的模式
- 所有像素点阵 / 闪烁 / 辉光 / 粒子在一个 instanced WGSL shader 里完成，每个矩形 1 个实例

## 构建

```
cargo build --release
```

产物 `target/release/spacesniffer-rs.exe`（约 6 MB），静态 CRT，无额外 DLL；不内嵌字体，使用系统自带的 Segoe UI / 微软雅黑 / Segoe UI Symbol。CI 见 `.github/workflows/build.yml`。
