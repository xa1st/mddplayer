
# 东东播放器 (mddplayer) 🎵

![Rust Language](https://img.shields.io/badge/language-Rust-orange?style=flat-square\&logo=rust)
![Apache 2.0 License](https://img.shields.io/badge/Apache%202.0-Source-green)
[![GitHub stars](https://img.shields.io/github/stars/xa1st/mddplayer.svg?label=Stars&style=flat-square)](https://github.com/xa1st/mddplayer)
[![GitHub forks](https://img.shields.io/github/forks/xa1st/mddplayer.svg?label=Fork&style=flat-square)](https://github.com/xa1st/mddplayer)
[![GitHub issues](https://img.shields.io/github/issues/xa1st/mddplayer.svg?label=Issue&style=flat-square)](https://github.com/xa1st/mddplayer/issues)
![](https://changkun.de/urlstat?mode=github&repo=xa1st/mddplayer)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg?style=flat-square)](https://github.com/xa1st/mddplayer/blob/master/LICENSE)

一款基于 **Rust** 开发的轻量级终端音频播放器，主打「简洁高效」与「终端友好」，支持主流音频格式，提供灵活的播放控制与可视化反馈。

<img src="https://raw.githubusercontent.com/xa1st/mddplayer/master/assets/preview.webp" width="100%" />

## ✨ 核心特性

| 特性            | 说明                                    |
| ------------- | ------------------------------------- |
| 🎧 **多格式兼容**  | 完美支持 MP3、FLAC、OGG、AAC 音频文件，自动识别文件类型       |
| 📂 **灵活输入源**  | 支持「单个文件」「音乐目录」「TXT 播放列表」三种输入方式，满足不同场景 |
| 🔀 **多样播放模式** | 顺序播放（1）、倒序播放（2）、随机播放（3），搭配循环播放功能      |
| ⌨️ **终端快捷键**  | 全键盘控制（暂停 / 切歌 / 调音量），无需鼠标，专注听歌        |
| 📊 **实时可视化**  | 显示歌曲名、艺术家（读取 ID3 标签）、播放进度、音量，自适应终端宽度  |
| 🧹 **纯净模式**   | 可隐藏说明文本，仅保留播放进度，适合极简主义用户              |

## 🚀 快速开始

### 🔍 前提条件

* 安装 Rust 环境（推荐 1.60+）：通过 [rustup](https://www.rust-lang.org/tools/install) 一键安装

* 操作系统：Linux（任意终端）、macOS（Terminal/iTerm2）、Windows（PowerShell/CMD）

### 🛠️ 安装与运行

1. **克隆仓库**

```
git clone https://github.com/xa1st/mddplayer.git

cd mddplayer
```

 **编译项目**（Release 模式优化性能）

```
cargo build --release
```

**启动播放器**（选择以下任意一种方式）

* 播放单个文件

```
./target/release/mddplayer -f /path/to/your/song.mp3
```

* 播放目录下所有音频

```
./target/release/mddplayer -f /path/to/your/music/folder
```

* 播放 TXT 播放列表（一行一个文件路径）

```
./target/release/mddplayer <歌曲/文件夹/播放列表> [-r|-s|-l|-m]
```

## ⌨️ 命令行参数说明

|参数|简写|类型|说明|
|-|-|-|-|
|`--random`|`-r`|开关|是否随机播放，有就是随机播放，无就是顺序播放|
|`-simple`|`-s`|开关|是否为极简模式，有就是，没有就是完整模式|
|`--loop`|`-l`|开关|是否为循环播放，有就是循环播放，无就是单次播放|
|`--volume`|`-m`|数字（1-100)|设置播放音量|

## 🎮 终端控制指南

播放过程中，按下以下按键实现对应功能：

|按键|功能|快捷键提示|
|-|-|-|
| `P` / `p` | 暂停播放         | 🅿️ 暂停 |
| 空格键       | 恢复播放         | ␣ 继续   |
| `←` 键     | 切换到上一首       | ← 上一曲  |
| `→` 键     | 切换到下一首       | → 下一曲  |
| `↑` 键     | 增加音量（+5%/ 次） | ↑ 音量 + |
| `↓` 键     | 减少音量（-5%/ 次） | ↓ 音量 - |
| `Q` / `q` | 退出播放器        | 🅿️ 退出 |

## 🧩 技术栈揭秘

| 模块功能     | 依赖库         | 作用说明                              |
| -------- | ----------- | --------------------------------- |
| 音频播放与解码  | `rodio`     | 核心音频输出，支持多格式解码与播放控制（暂停 / 停止 / 音量） |
| 音频时长计算   | `symphonia` | 精准获取音频文件总时长，解决部分格式时长识别问题          |
| 终端控制与交互  | `crossterm` | 终端清屏、光标控制、实时按键监听，跨平台兼容            |
| 命令行参数解析  | `clap`      | 处理用户输入参数，支持参数组、默认值、帮助文档自动生成       |
| ID3 标签读取 | `id3`       | 提取音频文件的歌名、艺术家等元数据，优化播放显示          |
| 随机播放洗牌   | `rand`      | 随机播放模式下打乱播放列表，确保随机性               |

## 📄 许可证

本项目基于 **Apache License 2.0** 开源，可自由用于个人 / 商业项目，详见 [LICENSE](LICENSE) 文件。

## 💡 温馨提示

1. 若播放列表中部分文件无法播放，可能是格式不支持（目前仅支持 MP3/FLAC/OGG/AAC）

2. Windows 系统下若提示「终端不支持 ANSI 转义序列」，建议使用 PowerShell 或更新版 CMD

3. 如需添加更多音频格式支持，可在 `scan_audio_files` 函数中扩展后缀名判断逻辑
