//! 东东播放器 —— 主模块（核心播放逻辑）
//!
//! # 整体架构
//!
//! 程序采用「双层循环 + 异步预加载」的结构：
//!
//! ```text
//! ┌─ 外层循环 'outer（播放列表迭代）─────────────────────────┐
//! │  1. 通过 mpsc 通道等待一首"预加载完成"的歌曲             │
//! │  2. 装入 rodio Sink 开始播放                             │
//! │  3. 立即启动下一首的后台预加载线程                       │
//! │  ┌─ 内层循环 'inner（单首歌曲播放中）──────────────────┐ │
//! │  │  · 每秒刷新一次进度显示                             │ │
//! │  │  · 以 100ms 为周期非阻塞轮询键盘事件                │ │
//! │  │  · 歌曲自然播完 / 手动切歌时跳出                    │ │
//! │  └─────────────────────────────────────────────────────┘ │
//! │  4. 根据退出原因更新索引（自动播放 +1 / 手动切歌 ±1）    │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! # 异步预加载
//!
//! 播放当前曲目的同时，后台线程已经在为下一首做准备工作：
//! 打开文件（仅一次）→ 读取标签（lofty）→ 克隆句柄探测总时长（symphonia）
//! → 原句柄解码（rodio Decoder，流式按需读盘）。
//! 结果通过 mpsc 通道送回主线程，因此「切歌」几乎零等待。
//! 加载失败（文件损坏 / 格式不支持 / 等待超时）会显示错误信息并自动跳过该曲。
//!
//! # 线程间通信
//!
//! - `Sender<PreloadResult>`（tx）：克隆给每一个预加载线程
//! - `Receiver<PreloadResult>`（rx）：由主循环独占
//! - 通道中可能残留"过期"的预加载结果（对应已被跳过的旧索引），主循环会直接丢弃

// 声明模块
mod cli;
mod utils;
mod metadata;

// 从各个模块引入所需的项
use clap::Parser;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::time::{Instant, Duration};
use std::{fs::File, io::{self, BufReader, Seek, Write}};
use std::sync::mpsc::{channel, Sender, Receiver};
use std::path::PathBuf;
use std::thread;

use rand::seq::SliceRandom;
use unicode_width::UnicodeWidthStr;

// 从 cli 模块引入常量和参数结构体
use cli::{Args, NAME, VERSION, URL};
// 从 utils 模块引入所有公共函数
use utils::{get_playlist_from_input, truncate_string, format_duration};
// 从 metadata 模块引入元数据获取函数
use metadata::{get_title_artist_info, get_total_duration};

// 终端交互库：用于控制终端（raw mode, 键入事件, 光标/清屏）
use crossterm::{
    // KeyModifiers：用于识别 Ctrl 等修饰键（修复 Ctrl+C 误触问题的关键）
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode, ClearType, SetTitle, SetSize},
    cursor,
};

// --- 常量定义 ---

/// 手动切歌（←/→）的最小间隔，防止快速连按导致索引跳多首
const MIN_SKIP_INTERVAL: Duration = Duration::from_millis(250);
/// 音量调节步长（↑/↓ 每按一次调整 1%）
const VOLUME_STEP: f32 = 0.01;
/// 进度条刷新频率（每秒一次，避免高频重绘导致终端闪烁）
const UPDATE_INTERVAL: Duration = Duration::from_millis(1000);
/// 加载失败/超时后，错误提示的停留时间
const ERROR_WAIT_DURATION: Duration = Duration::from_secs(1);
/// 暂停/静音等"开关型"按键的防抖间隔，过滤 crossterm 可能上报的重复事件
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(200);

// ===============================================
// 异步预加载数据结构
// ===============================================

/// 一首歌曲"预加载完成"后的完整数据，可直接装入 Sink 播放。
struct PreloadedData {
    /// 已解码的音频流（rodio Decoder 直接实现了 Source trait）
    decoder: rodio::Decoder<std::io::BufReader<std::fs::File>>,
    /// 歌曲标题（来自 lofty 标签，无标签时回退到默认文案）
    title: String,
    /// 艺术家（同上）
    artist: String,
    /// 音频总时长（来自 symphonia 探测，获取失败时为 0，显示为 ??:??）
    total_duration: Duration,
}

/// 预加载线程 → 主循环的消息。
/// 索引字段用于让主循环判断"这份结果是否还有效"（是否对应当前要播的曲目）。
enum PreloadResult {
    /// 加载成功：(数据, 预加载的歌曲在播放列表中的索引)
    Success(PreloadedData, usize),
    /// 加载失败：(索引, 错误信息类型, 文件名)
    Failure(usize, String, String),
}

/// 统一的退出清理逻辑，确保终端恢复正常（退出 raw mode、显示光标）。
/// 所有退出路径（Q / Ctrl+C / 播放完毕）都必须经过这里。
fn graceful_exit(stdout: &mut io::Stdout) -> Result<(), Box<dyn std::error::Error>> {
    // 清空当前行并回到行首，保证退出文案从行首开始打印
    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
    println!("👋 播放器退出。");
    disable_raw_mode()?;
    execute!(stdout, cursor::Show)?;
    Ok(())
}

/// 在后台线程启动指定歌曲的预加载。
///
/// 线程内依次执行：
/// 1. 读取标签（标题/艺术家）
/// 2. 打开文件（仅一次），克隆句柄探测总时长，原句柄归零后交给解码器
/// 3. WAV 的时长由 hound 解码器直接提供（`decoder.total_duration()`），
///    优先采用；其余格式使用 symphonia 的探测结果
///
/// 任何一步失败都通过 `tx` 发送 `Failure`，成功则发送 `Success`。
/// 发送失败（`.is_err()`）仅说明主线程已退出（接收端已销毁），静默忽略即可。
fn start_preloader_thread(
    path: PathBuf,
    index: usize,
    tx: Sender<PreloadResult>,
) {
    // 预先取好文件名用于错误提示；取不到文件名时回退为完整路径
    let filename_display = path.file_name().map_or_else(
        || path.as_os_str().to_string_lossy().into_owned(),
        |os_str| os_str.to_string_lossy().into_owned(),
    );

    thread::spawn(move || {
        // 失败时的统一上报（闭包借用 tx 与文件名，发送失败表示主线程已退出，忽略即可）
        let send_failure = |err_type: &str| {
            if tx.send(PreloadResult::Failure(index, err_type.to_string(), filename_display.clone())).is_err() {}
        };

        // 1. 读取 ID3/Vorbis 等标签信息
        let (title, artist) = get_title_artist_info(path.as_path());

        // 2. 打开文件 —— 时长探测与解码共用这一次打开
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_e) => {
                send_failure("无法打开或读取");
                return;
            }
        };

        // 3. 先在克隆句柄上用 symphonia 探测总时长（覆盖 mp3/flac/ogg/aac/m4a）。
        //    对 WAV 探测会立即失败返回 0（symphonia 不再编译 WAV 容器支持），开销可忽略；
        //    WAV 的时长随后由 hound 解码器直接给出。
        let probed_duration = match file.try_clone() {
            Ok(clone) => get_total_duration(path.as_path(), clone),
            Err(_e) => {
                send_failure("无法打开或读取");
                return;
            }
        };

        // try_clone 出的句柄与原句柄共享文件位置，探测结束后把原句柄归零再交给解码器
        if file.rewind().is_err() {
            send_failure("无法打开或读取");
            return;
        }

        // 4. 解码（流式：音频数据按需从磁盘读取，不整曲载入内存）
        let decoder = match Decoder::new(BufReader::new(file)) {
            Ok(d) => d,
            Err(_e) => {
                send_failure("解码失败");
                return;
            }
        };

        // 5. 合并时长来源：WAV(hound) 解码器自带准确时长，优先采用；
        //    其余格式解码器拿不到（None），使用 symphonia 探测结果
        let total_duration = decoder.total_duration().unwrap_or(probed_duration);

        let data = PreloadedData{decoder, title, artist, total_duration};
        if tx.send(PreloadResult::Success(data, index)).is_err() {
            // 主线程已退出，忽略发送失败
        }
    });
}

/// 启动指定索引的预加载（如果索引在播放列表范围内）。
/// 对边界做一次收口，避免调用处到处写 `if index < len` 判断。
fn start_preload_if_valid(
    playlist: &[PathBuf],
    index: usize,
    tx: &Sender<PreloadResult>,
) {
    if index < playlist.len() {
        let path = playlist[index].clone();
        start_preloader_thread(path, index, tx.clone());
    }
}

/// 在当前行显示"跳过歌曲"的错误信息，停留 1 秒后清除。
/// 使用 eprint!（不带换行）+ 清行，保证不破坏单行进度界面。
fn display_error_and_wait(
    stdout: &mut io::Stdout,
    current_index: usize,
    total_tracks: usize,
    err_type: &str,
    filename: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
    let track_info = format!("[{}/{}]", current_index + 1, total_tracks);
    let error_msg_truncated = truncate_string(filename, 30);
    eprint!("{} [错误:{}]: {} -> 跳过...", track_info, err_type, error_msg_truncated);
    thread::sleep(ERROR_WAIT_DURATION);
    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
    Ok(())
}

/// 刷新单行进度显示。
///
/// 布局（从左到右，右侧以空格补齐到终端宽度）：
/// ```text
/// [01][随|循][MP3][歌名-艺术家][03:25/04:10][75%]
///  ↑    ↑     ↑        ↑            ↑        ↑
///  曲目 播放  格式   自适应宽度    进度     音量
///  序号 模式        的信息区域
/// ```
///
/// 歌名区域的宽度 = 终端宽度 - 其余固定字段宽度，因此能自适应任意终端尺寸。
/// 中日韩字符按 unicode-width 的「显示宽度」计算，保证中英混排时不错位。
fn update_progress_display(
    stdout: &mut io::Stdout,
    current_index: usize,
    total_tracks: usize,
    is_random: bool,
    is_loop: bool,
    title: &str,
    artist: &str,
    track_path: &str,
    current_time: Duration,
    total_duration: Duration,
    volume: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let current_time_str = format_duration(current_time);
    let total_duration_str = format_duration(total_duration);
    let track_count_str = format!("[{}/{}]", current_index + 1, total_tracks);
    // 从文件路径中提取扩展名作为格式标识（如 MP3 / FLAC）
    let ext = track_path.split('.').last().unwrap_or("未知").to_uppercase();
    let random_str = if is_random { "随" } else { "顺" };
    let loop_str = if is_loop { "循" } else { "单" };
    let play_mode_str = format!("{}|{}", random_str, loop_str);

    // 第一遍：先用空信息占位（`[]`），算出固定字段的显示宽度
    let mut display_text_unpadded = format!(
        "{}[{}][{}][][{}/{}][{:.0}%]",
        track_count_str, play_mode_str, ext, current_time_str, total_duration_str, volume * 100.0
    );

    let terminal_width = terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize;
    let current_unpadded_width = display_text_unpadded.as_str().width();
    // 固定字段之外的剩余宽度，全部留给歌名区域
    let music_info_width = terminal_width.saturating_sub(current_unpadded_width);
    let music_info_content = format!("{}-{}", title, artist);
    // 剩余宽度太小放不下"歌名-艺术家"时，退化为只显示歌名
    let music_info = if music_info_width < 15 {
        truncate_string(title, music_info_width)
    } else {
        truncate_string(&music_info_content, music_info_width)
    };

    // 第二遍：把真实信息填入占位符
    display_text_unpadded = format!(
        "{}[{}][{}][{}][{}/{}][{:.0}%]",
        track_count_str, play_mode_str, ext, music_info, current_time_str, total_duration_str, volume * 100.0
    );

    // 不足终端宽度的部分用空格补齐，覆盖上一帧的残留字符
    let new_len = display_text_unpadded.as_str().width();
    let padding_needed = terminal_width.saturating_sub(new_len);
    let padding = " ".repeat(padding_needed);
    let display_text = format!("{}{}", display_text_unpadded, padding);

    // 回到行首原地重写，实现"单行刷新"效果
    execute!(stdout, cursor::MoveToColumn(0))?;
    print!("{}", display_text);
    stdout.flush()?;
    Ok(())
}

/// 调整音量：在当前值基础上增减 `delta`，并夹紧到 [0.0, 1.0] 区间。
fn adjust_volume(sink: &Sink, delta: f32) {
    let current_volume = sink.volume();
    let new_volume = (current_volume + delta).clamp(0.0, 1.0);
    sink.set_volume(new_volume);
}


// ===============================================
// MAIN 函数
// ===============================================
//
// 流程总览：
//   1. 解析命令行参数 → 构建播放列表（随机模式则洗牌）
//   2. 终端初始化（清屏 / 设定窗口尺寸 / 改标题 / 进入 raw mode / 隐藏光标）
//   3. 初始化音频输出（rodio OutputStream + Sink）并设置初始音量
//   4. 建立 mpsc 通道，预加载第一首歌
//   5. 'outer 主循环：等待预加载结果 → 播放 → 'inner 播放循环
//   6. 退出时统一走 graceful_exit 恢复终端

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // 如果没有提供文件参数，显示帮助信息
    // （clap 遇到 --help 会打印帮助并直接退出进程，后面的 return 只是兜底）
    let input_path_str = match &args.file {
        Some(path) => path,
        None => {
            Args::parse_from(&["mddplayer", "--help"]);
            return Ok(());
        }
    };

    let is_simple_mode = args.clean;          // -s 纯净模式
    let is_random_enabled = args.random;      // -r 随机播放
    let is_loop_enabled = args.is_loop;       // -l 列表循环
    let initial_volume = args.volume as f32 / 100.0; // -v 音量（1-100 → 0.0-1.0）

    // 根据输入类型（文件/目录/TXT 播放列表/通配符）构建播放列表
    let mut playlist = match get_playlist_from_input(input_path_str) {
        Ok(p) => p,
        Err(_e) => {
            eprintln!("[错误]处理输入路径 '{}' 时失败", input_path_str);
            return Ok(());
        }
    };

    if playlist.is_empty() {
        eprintln!("[错误]在指定的路径中未找到支持的音频文件。");
        return Ok(());
    }

    // 随机模式：直接打乱整个播放列表顺序
    if is_random_enabled {
        let mut rng = rand::thread_rng();
        playlist.shuffle(&mut rng);
    }

    // ----------------------------------------------------
    // --- 核心播放逻辑：初始化 ---
    // ----------------------------------------------------

    let mut stdout = io::stdout();

    // 终端初始化：清屏、设定窗口尺寸（完整模式 60x8，纯净模式 60x1）、改标题
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    if !is_simple_mode {
        execute!(stdout, SetSize(60, 8))?;
    } else {
        execute!(stdout, SetSize(60, 1))?;
    }
    let mut initial_title = format!("{} - v{}", cli::NAME, cli::VERSION);
    execute!(stdout, SetTitle(initial_title.clone()))?;
    // raw mode：关闭行缓冲与信号处理，实现逐键响应（退出前必须 disable）
    enable_raw_mode()?;
    execute!(stdout, cursor::Hide)?;

    // 初始化音频输出和 Sink
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    // 初始设置音量（夹紧到 0.0-1.0，防止用户传超大数字）
    sink.set_volume(initial_volume.min(1.0).max(0.0));

    // 显示界面信息（非纯净模式下）
    if !is_simple_mode {
        println!("=====================【 {} 】======================", NAME);
        println!(" 版本:v{}          主页:{}", VERSION, URL);
        println!("===========================================================");
        println!("====================【 控 制 说 明 】======================");
        println!(" [P]静音/取消静音   [空格]暂停/播放    [Q/Ctrl+C]退出播放");
        println!(" [←]上一首      [→]下一首    [↑]音量增    [↓]音量减");
        println!("============================================================");
    }

    // --- 异步初始化和预加载设置 ---
    let (tx, rx): (Sender<PreloadResult>, Receiver<PreloadResult>) = channel();
    let total_tracks = playlist.len();
    let mut current_track_index: usize = 0;

    // 🌟 启动第一首歌的预加载
    start_preload_if_valid(&playlist, 0, &tx);

    let mut index_offset: i32 = 0; // 手动切歌的方向：+1 下一首 / -1 上一首
    let mut last_skip_time = Instant::now() - MIN_SKIP_INTERVAL; // 切歌防抖计时器
    let mut muted_volume: Option<f32> = None; // 静音状态（放在外层循环外，跨歌曲保留；Some 记录静音前的音量）
    // 注：初始值设为 now - MIN_SKIP_INTERVAL，保证第一首就能立即切歌

    // --- 主循环：迭代播放列表 ---
    'outer: loop {
        // 🌟 关键：在进入阻塞等待前，快速检查是否有 Q/Ctrl+C 按下。
        // 否则下面的 recv_timeout 最多阻塞 5 秒，期间按键会完全无响应。
        // （poll 0ms = 只在有事件时立即读取，不等待）
        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key_event) = event::read()? {
                // 退出判定：Q/q 直接退出；c/C 必须配合 Ctrl 修饰键（修复普通字母 c 误触退出）
                let should_exit = match key_event.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => true,
                    KeyCode::Char('c') | KeyCode::Char('C') => {
                        key_event.modifiers.contains(KeyModifiers::CONTROL)
                    }
                    _ => false,
                };
                if should_exit {
                    graceful_exit(&mut stdout)?;
                    return Ok(());
                }
            }
        }

        // 循环播放检查 (如果当前索引超限，则回到开头或退出)
        if current_track_index >= total_tracks {
            if is_loop_enabled {
                current_track_index = 0;
                start_preload_if_valid(&playlist, 0, &tx);
            } else {
                break;
            }
        }

        // --- 5. 等待预加载结果（文件加载、解码都在后台线程完成） ---
        let (preloaded_data, _preloaded_index) = loop {
            // 等待预加载结果，最多 5 秒（大文件或慢盘也有充足时间）
            match rx.recv_timeout(Duration::from_secs(5)) {
                // ✅ 接收到成功结果
                Ok(PreloadResult::Success(data, index)) => {
                    if index == current_track_index {
                        // 正是我们等待的那首 → 装载
                        break (data, index);
                    } else {
                        // 旧索引的"过期"结果（对应已被跳过的曲目）→ 直接丢弃
                        continue;
                    }
                },
                // ❌ 接收到失败结果
                Ok(PreloadResult::Failure(index, err_type, filename)) => {
                    if index == current_track_index {
                        // 当前这首加载失败 → 显示错误，索引前移并预加载下一首
                        display_error_and_wait(&mut stdout, current_track_index, total_tracks, &err_type, &filename)?;
                        current_track_index += 1;
                        start_preload_if_valid(&playlist, current_track_index, &tx);
                        continue 'outer;
                    } else {
                        // 过期失败消息 → 丢弃
                        continue;
                    }
                },
                // ⏱️ 等待超时（5 秒内没有任何结果，可能是文件极大或磁盘异常）
                Err(e) if e == std::sync::mpsc::RecvTimeoutError::Timeout => {
                    display_error_and_wait(&mut stdout, current_track_index, total_tracks, "加载超时", "")?;
                    current_track_index += 1;
                    start_preload_if_valid(&playlist, current_track_index, &tx);
                    continue 'outer;
                }
                // 所有预加载线程都已结束且通道关闭 → 致命错误
                Err(_) => {
                    eprintln!("\n[致命错误] 预加载通道关闭，退出播放器...");
                    break 'outer;
                }
            }
        };

        // 装载歌曲：先清掉 Sink 里可能残留的音频源，再追加新解码器
        let track_path_str = playlist[current_track_index].to_string_lossy().to_string();
        sink.clear();
        sink.append(preloaded_data.decoder);

        // -----------------------------------------------------------------
        // 🌟 切歌后重新应用静音状态
        // -----------------------------------------------------------------
        if let Some(_vol) = muted_volume {
            // 如果处于静音状态，新歌继续保持静音（音量 0.0）
            sink.set_volume(0.0);
        } else {
            // 非静音状态：保持 sink 当前音量即可（adjust_volume 已实时更新过它）
        }
        // -----------------------------------------------------------------

        // 上一首若处于暂停状态，新歌自动开始播放（避免切歌后卡在暂停）
        if sink.is_paused() {
            sink.play();
        }

        // 从预加载数据中取出歌曲信息
        let title = preloaded_data.title;
        let artist = preloaded_data.artist;
        let total_duration = preloaded_data.total_duration;

        // 更新窗口标题为「歌名-艺术家-播放器版本」，静音时附加 [静音] 前缀
        let current_initial_title = format!("{}-{}-{}v{}", title, artist, NAME, VERSION);
        let display_title = if muted_volume.is_some() {
            format!("[静音]{}", current_initial_title)
        } else {
            current_initial_title.clone()
        };
        execute!(stdout, SetTitle(display_title))?;
        // 同步到外层 initial_title，供内部循环的暂停/静音/恢复标题使用
        initial_title = current_initial_title;

        // 🌟 立即预加载下一首（提前解码，实现切歌零等待）
        let next_index = (current_track_index + 1) % total_tracks;
        // 条件说明：单曲循环时 next_index == 当前索引，无需预加载自己；
        // 非循环模式且已在最后一首时，后面没有下一首，也不预加载
        if next_index != current_track_index && (is_loop_enabled || current_track_index < total_tracks.saturating_sub(1)) {
            start_preload_if_valid(&playlist, next_index, &tx);
        }

        // --- 本首歌的播放状态 ---
        let start_time = Instant::now();                  // 本首歌开始时间
        let mut paused_duration = Duration::from_secs(0); // 累计暂停时长（从已播放时间中扣除）
        let mut last_pause_time: Option<Instant> = None;  // 本次暂停的起始时刻（None = 正在播放）
        let mut last_running_time = Duration::from_secs(0); // 进入暂停那一刻的已播放时长（冻结显示用）
        let mut last_progress_update = Instant::now();    // 上次刷新进度条的时刻（控制 1s 刷新频率）
        let mut forced_stop = false;                      // 是否为手动切歌（区别于自然播完）
        let mut last_toggle_time = Instant::now() - TOGGLE_DEBOUNCE; // 开关型按键防抖计时器

        // 8. 内部播放循环：歌曲未播完（Sink 还有音频源）就一直循环
        'inner: while !sink.empty() {
            // --- 暂停状态的时间会计 ---
            if sink.is_paused() {
                // 刚进入暂停：记录暂停起始时刻，并冻结"当前已播放时长"
                if last_pause_time.is_none() {
                    last_pause_time = Some(Instant::now());
                    last_running_time = start_time.elapsed().saturating_sub(paused_duration);
                }
            } else {
                // 刚从暂停恢复：把这段暂停时长累加进 paused_duration
                if let Some(pause_start) = last_pause_time.take() {
                    paused_duration += pause_start.elapsed();
                }
            }
            // 当前已播放时长 = 总流逝时间 - 暂停时长（暂停中则用冻结值）
            let current_time = if sink.is_paused() {
                last_running_time
            } else {
                start_time.elapsed().saturating_sub(paused_duration)
            };

            // --- 进度显示刷新（每秒一次） ---
            if last_progress_update.elapsed() >= UPDATE_INTERVAL {
                // 静音时显示 0% 音量，否则显示实际音量
                let display_volume = if muted_volume.is_some() {
                    0.0
                } else {
                    sink.volume()
                };

                update_progress_display(
                    &mut stdout,
                    current_track_index,
                    total_tracks,
                    is_random_enabled,
                    is_loop_enabled,
                    &title,
                    &artist,
                    &track_path_str,
                    current_time,
                    total_duration,
                    display_volume, // 使用修复后的音量
                )?;
                last_progress_update = Instant::now();
            }

            // --- 用户输入处理（非阻塞，100ms 轮询周期） ---
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    match key_event.code {
                        // P键：静音/取消静音（带防抖，过滤重复上报）
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            if last_toggle_time.elapsed() < TOGGLE_DEBOUNCE { continue; }
                            last_toggle_time = Instant::now();
                            if let Some(vol) = muted_volume {
                                // 取消静音：恢复静音前的音量
                                sink.set_volume(vol);
                                muted_volume = None;
                                execute!(stdout, SetTitle(initial_title.clone()))?;
                            } else {
                                // 静音：记住当前音量，置 0
                                muted_volume = Some(sink.volume());
                                sink.set_volume(0.0);
                                let mute_title = format!("[静音]{}", initial_title);
                                execute!(stdout, SetTitle(mute_title))?;
                            }
                        }
                        // 空格键：暂停/播放（带防抖）
                        KeyCode::Char(' ') => {
                            if last_toggle_time.elapsed() < TOGGLE_DEBOUNCE { continue; }
                            last_toggle_time = Instant::now();
                            if sink.is_paused() {
                                sink.play();
                                // 恢复播放：标题恢复正常（静音状态则保留 [静音] 前缀）
                                let display_title = if muted_volume.is_some() {
                                    format!("[静音]{}", initial_title)
                                } else {
                                    initial_title.clone()
                                };
                                execute!(stdout, SetTitle(display_title))?;
                            } else {
                                sink.pause();
                                let pause_title = format!("[暂停]{}", initial_title);
                                execute!(stdout, SetTitle(pause_title))?;
                            }
                        }
                        // ↑键：音量增加（处于静音时先自动取消静音）
                        KeyCode::Up => {
                            if let Some(vol) = muted_volume.take() {
                                // 先恢复到静音前的音量
                                sink.set_volume(vol);
                                execute!(stdout, SetTitle(initial_title.clone()))?;
                            }
                            adjust_volume(&sink, VOLUME_STEP);
                        },
                        // ↓键：音量减少（处于静音时先自动取消静音）
                        KeyCode::Down => {
                            if let Some(vol) = muted_volume.take() {
                                // 先恢复到静音前的音量
                                sink.set_volume(vol);
                                execute!(stdout, SetTitle(initial_title.clone()))?;
                            }
                            adjust_volume(&sink, -VOLUME_STEP);
                        },
                        // →键：下一首（250ms 防抖；非循环模式下最后一首不能前进）
                        KeyCode::Right => {
                            if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                            if current_track_index < total_tracks.saturating_sub(1) || is_loop_enabled {
                                sink.stop(); index_offset = 1; forced_stop = true; last_skip_time = Instant::now(); break 'inner; }
                        }
                        // ←键：上一首（250ms 防抖；非循环模式下第一首不能后退，循环时回绕到末尾）
                        KeyCode::Left => {
                            if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                            if current_track_index > 0 || is_loop_enabled {
                                sink.stop(); index_offset = -1; forced_stop = true; last_skip_time = Instant::now(); break 'inner; }
                        }
                        // 退出：Q/q 直接退出
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            graceful_exit(&mut stdout)?;
                            return Ok(());
                        }
                        // 退出：Ctrl+C（必须按住 Ctrl 修饰键；普通字母 c 不再误触退出）
                        KeyCode::Char('c') | KeyCode::Char('C')
                            if key_event.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            graceful_exit(&mut stdout)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        } // 内部播放循环结束

        // 9. 索引更新逻辑（区分"自然播完"与"手动切歌"两种情况）
        if forced_stop {
            // 手动切歌：按 index_offset 方向调整索引（上一首在 0 处回绕到最后一首）
            if index_offset > 0 {
                current_track_index = (current_track_index + 1) % total_tracks;
            } else if index_offset < 0 {
                current_track_index = if current_track_index == 0 { total_tracks.saturating_sub(1) } else { current_track_index - 1 };
            }
            index_offset = 0;
            // -----------------------------------------------------------------
            // 🌟 修复：手动切歌后，必须立即启动新目标歌曲的预加载
            // -----------------------------------------------------------------
            start_preload_if_valid(&playlist, current_track_index, &tx);
        } else {
            // 自然播完：清掉当前行残留的进度条，索引前移
            execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
            current_track_index += 1;
        }
    } // 主循环结束 'outer

    // 10. 播放列表结束后的清理工作
    graceful_exit(&mut stdout)?;

    Ok(())
}

// ===============================================
// 测试
// ===============================================

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source;

    /// 生成一段最小的 PCM16 WAV 字节（立体声 44100Hz，无标签），用于冒烟测试。
    fn make_test_wav(frames: usize) -> Vec<u8> {
        const SAMPLE_RATE: u32 = 44100;
        const CHANNELS: u16 = 2;
        let data_len = (frames * CHANNELS as usize * 2) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data_len).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());                                // fmt 块大小
        buf.extend_from_slice(&1u16.to_le_bytes());                                 // PCM
        buf.extend_from_slice(&CHANNELS.to_le_bytes());
        buf.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        buf.extend_from_slice(&(SAMPLE_RATE * CHANNELS as u32 * 2).to_le_bytes());  // 字节率
        buf.extend_from_slice(&(CHANNELS * 2).to_le_bytes());                       // 块对齐
        buf.extend_from_slice(&16u16.to_le_bytes());                                // 位深
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            // 440Hz 正弦波，左右声道相同
            let s = ((i as f32) * 2.0 * std::f32::consts::PI * 440.0 / SAMPLE_RATE as f32).sin() * 8000.0;
            let s = s as i16;
            buf.extend_from_slice(&s.to_le_bytes());
            buf.extend_from_slice(&s.to_le_bytes());
        }
        buf
    }

    /// 冒烟测试：验证「打开文件 → 克隆句柄探测时长 → 解码」的完整预加载链路，
    /// 同时覆盖统一到 symphonia 后端后的基础解码能力。
    #[test]
    fn preload_pipeline_smoke_test() {
        // 生成 0.5 秒的测试 WAV 并写入临时文件
        let mut path = std::env::temp_dir();
        path.push(format!("mddplayer_smoke_{}.wav", std::process::id()));
        std::fs::write(&path, make_test_wav(22050)).unwrap();

        // 启动预加载线程并等待结果
        let (tx, rx) = channel();
        start_preloader_thread(path.clone(), 0, tx);
        let result = rx.recv_timeout(Duration::from_secs(10));

        match result {
            Ok(PreloadResult::Success(mut data, 0)) => {
                // 时长探测正确（symphonia 按 n_frames/采样率 计算）
                assert_eq!(data.total_duration, Duration::from_millis(500));
                // 无标签文件回退到默认文案
                assert_eq!(data.title, "未知");
                assert_eq!(data.artist, "未知");
                // 解码器能完整解出样本（22050 帧 × 2 声道，流式迭代）
                assert_eq!(data.decoder.by_ref().count(), 22050 * 2);
                assert_eq!(data.decoder.channels(), 2);
                assert_eq!(data.decoder.sample_rate(), 44100);
            }
            Ok(_) => panic!("预加载返回了非预期的结果"),
            Err(e) => panic!("预加载失败: {}", e),
        }

        std::fs::remove_file(&path).ok();
    }
}
