# SpaceSniffer-rs

A disk space visualizer for Windows, inspired by [SpaceSniffer](http://www.uderzo.it/main_products/space_sniffer/).

## Features

- Really fucking smooth and seamless animation
- Beautiful pixel dithering style UI
- Extremely low overhead rendering
- Lower RAM usage than almost any alternative
- Faster scan speed than almost any alternatives as well

## Usage

Start it and choose a drive, or type a path. You can also pass the folder on the command line:

```
spacesniffer-rs.exe D:\
```

To visualize how low the rendering overhead is, press Alt+V to toggle uncapped rendering, and see how many FPS you can get.

## Building

```
cargo build --release
```

The executable ends up in `target/release/spacesniffer-rs.exe`.
