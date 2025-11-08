// src/main.rs (最终版本，解决卡死问题)

// 声明模块
mod cli;
mod utils;
mod metadata;

// 从各个模块引入所需的项
use clap::Parser;
use rodio::{Decoder, OutputStream, Sink};
use std::time::{Instant, Duration};
use std::{fs::File, io::{self, BufReader, Write}};
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
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode, ClearType, SetTitle, SetSize},
    cursor,
};

// --- 常量定义 ---
const MIN_SKIP_INTERVAL: Duration = Duration::from_millis(250); // 最小切歌间隔
const VOLUME_STEP: f32 = 0.01; // 音量调节步长
const UPDATE_INTERVAL: Duration = Duration::from_millis(1000); // 进度更新频率
const ERROR_WAIT_DURATION: Duration = Duration::from_secs(5);

// ===============================================
// 异步预加载数据结构
// ===============================================

// 定义用于线程间发送成功加载结果的数据结构
struct PreloadedData {
    decoder: rodio::Decoder<std::io::BufReader<std::fs::File>>,
    title: String,
    artist: String,
    total_duration: Duration,
}

// 定义用于线程间发送预加载结果的消息
enum PreloadResult {
    Success(PreloadedData, usize), // (数据, 预加载的歌曲在播放列表中的索引)
    Failure(usize, String, String), // (索引, 错误信息类型, 文件名)
}

// 统一的退出清理逻辑，确保终端恢复正常
fn graceful_exit(stdout: &mut io::Stdout) -> Result<(), Box<dyn std::error::Error>> {
    // 尝试清空可视区域并移到 (0, 0) (类似 cls 的效果)
    // execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::All))?;
    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
    println!("👋 播放器退出。");
    disable_raw_mode()?;
    execute!(stdout, cursor::Show)?;
    Ok(())
}

// 在后台线程启动下一首歌曲的预加载。
fn start_preloader_thread(
    path: PathBuf,
    index: usize,
    tx: Sender<PreloadResult>,
) {
    let filename_display = path.file_name().map_or_else(
        || path.as_os_str().to_string_lossy().into_owned(),
        |os_str| os_str.to_string_lossy().into_owned(),
    );

    thread::spawn(move || {
        let (title, artist) = get_title_artist_info(path.as_path());
        let total_duration = get_total_duration(path.as_path());

        let file = match File::open(&path) {
            Ok(f) => BufReader::new(f),
            Err(_e) => {
                if tx.send(PreloadResult::Failure(index, "无法打开或读取".to_string(), filename_display)).is_err() {}
                return;
            }
        };
        let decoder = match Decoder::new(file) {
            Ok(d) => d,
            Err(_e) => {
                if tx.send(PreloadResult::Failure(index, "解码失败".to_string(), filename_display)).is_err() {}
                return;
            }
        };

        let data = PreloadedData{decoder, title, artist, total_duration};
        if tx.send(PreloadResult::Success(data, index)).is_err() {
            // 主线程已退出，忽略发送失败
        }
    });
}


// ===============================================
// MAIN 函数
// ===============================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    let input_path_str = &args.file;
    let is_simple_mode = args.clean;
    let is_random_enabled = args.random;
    let is_loop_enabled = args.is_loop;
    let initial_volume = args.volume as f32 / 100.0;

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

    if is_random_enabled {
        let mut rng = rand::thread_rng();
        playlist.shuffle(&mut rng);
    }

    // ----------------------------------------------------
    // --- 核心播放逻辑：初始化 ---
    // ----------------------------------------------------

    let mut stdout = io::stdout();

    // 终端初始化
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    if !is_simple_mode {
        execute!(stdout, SetSize(60, 8))?;
    } else {
        execute!(stdout, SetSize(60, 1))?;
    }
    let mut initial_title = format!("{} - v{}", cli::NAME, cli::VERSION);
    execute!(stdout, SetTitle(initial_title.clone()))?;
    enable_raw_mode()?;
    execute!(stdout, cursor::Hide)?;

    // 初始化音频输出和 Sink
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    sink.set_volume(initial_volume.min(1.0).max(0.0));

    // 显示界面信息（非纯净模式下）
    if !is_simple_mode {
        println!(" =====================【 {} 】======================", NAME);
        println!("  版本:v{}    主页:{}", VERSION, URL);
        println!(" ===========================================================");
        println!(" ====================【 控 制 说 明 】======================");
        println!("  [P]暂停播放   [空格]恢复播放    [Q/Ctrl+C]退出播放");
        println!("  [←]上一首  [→]下一首  [↑]音量增  [↓]音量减");
        println!(" ===========================================================");
    }

    // --- 异步初始化和预加载设置 ---
    let (tx, rx): (Sender<PreloadResult>, Receiver<PreloadResult>) = channel();
    let total_tracks = playlist.len();
    let mut current_track_index: usize = 0;

    // 🌟 启动第一首歌的预加载
    if let Some(path) = playlist.get(0).cloned() {
        start_preloader_thread(path, 0, tx.clone());
    }

    let mut index_offset: i32 = 0;
    let mut last_skip_time = Instant::now() - MIN_SKIP_INTERVAL;

    // --- 主循环：迭代播放列表 ---
    'outer: loop {
        // 🌟 关键修正：在进入阻塞等待前，快速检查是否有 Ctrl+C/Q 按下
        if event::poll(Duration::from_millis(0))? {
             if let Event::Key(key_event) = event::read()? {
                 if key_event.code == KeyCode::Char('q') || key_event.code == KeyCode::Char('Q') || key_event.code == KeyCode::Char('c') {
                    graceful_exit(&mut stdout)?;
                    return Ok(());
                }
             }
        }
        
        // 循环播放检查 (如果当前索引超限，则尝试循环或退出)
        if current_track_index >= total_tracks {
            if is_loop_enabled {
                current_track_index = 0;
                if total_tracks > 0 {
                    let next_path = playlist[0].clone();
                    start_preloader_thread(next_path, 0, tx.clone());
                }
            } else {
                break;
            }
        }

        // --- 5. 文件加载、解码、添加到 Sink (使用预加载结果) ---
        let (preloaded_data, _preloaded_index) = loop {
            // 尝试接收预加载结果，等待时间较长以确保有时间加载
            match rx.recv_timeout(Duration::from_secs(5)) {
                // ⚠️ 接收到成功结果
                Ok(PreloadResult::Success(data, index)) => {
                    if index == current_track_index {
                        break (data, index);
                    } else {
                        continue;
                    }
                },
                // ⚠️ 接收到失败结果
                Ok(PreloadResult::Failure(index, err_type, filename)) => {
                    if index == current_track_index {
                        execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;

                        let track_info = format!("[{}/{}]", current_track_index + 1, total_tracks);
                        let error_msg_truncated = truncate_string(&filename, 30); 
                        eprint!("{} [错误:{}]: {} -> 跳过...", track_info, err_type, error_msg_truncated);

                        thread::sleep(ERROR_WAIT_DURATION);
                        execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
                        
                        current_track_index += 1;
                        if current_track_index < total_tracks {
                            let next_path = playlist[current_track_index].clone();
                            start_preloader_thread(next_path, current_track_index, tx.clone());
                        }
                        continue 'outer;
                    } else {
                        continue;
                    }
                },
                // 如果超时...
                Err(e) if e == std::sync::mpsc::RecvTimeoutError::Timeout => {
                    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
                    
                    let track_info = format!("[{}/{}]", current_track_index + 1, total_tracks);
                    eprint!("{} [错误:加载超时] -> 跳过...", track_info);

                    thread::sleep(ERROR_WAIT_DURATION);
                    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;

                    current_track_index += 1;

                    if current_track_index < total_tracks {
                        let next_index_to_load = current_track_index;
                        let next_path = playlist[next_index_to_load].clone();
                        start_preloader_thread(next_path, next_index_to_load, tx.clone());
                    }

                    continue 'outer;
                }
                // 接收通道断开
                Err(_) => {
                    eprintln!("\n[致命错误] 预加载通道关闭，退出播放器...");
                    break 'outer;
                }
            }
        };
        // ... (歌曲预加载成功后的逻辑，与原代码一致)
        let track_path_str = playlist[current_track_index].to_string_lossy().to_string();
        sink.clear();
        sink.append(preloaded_data.decoder);

        if sink.is_paused() {
            sink.play();
        }

        let title = preloaded_data.title;
        let artist = preloaded_data.artist;
        let total_duration = preloaded_data.total_duration;
        let total_duration_str = format_duration(total_duration);

        initial_title = format!("{}-{}-{}v{}", title, artist, NAME, VERSION);
        execute!(stdout, SetTitle(initial_title.clone()))?;

        let next_index = (current_track_index + 1) % total_tracks;

        if next_index != current_track_index && (is_loop_enabled || current_track_index < total_tracks.saturating_sub(1)) {
            let next_path = playlist[next_index].clone();
            start_preloader_thread(next_path, next_index, tx.clone());
        }

        let start_time = Instant::now();
        let mut paused_duration = Duration::from_secs(0);
        let mut last_pause_time: Option<Instant> = None;
        let mut last_running_time = Duration::from_secs(0);
        let mut last_progress_update = Instant::now();
        let mut forced_stop = false;

        // 8. 内部播放循环 
        'inner: while !sink.empty() {
            // ... (时间计算)

            if sink.is_paused() {
                if last_pause_time.is_none() {
                    last_pause_time = Some(Instant::now());
                    last_running_time = start_time.elapsed().saturating_sub(paused_duration);
                }
            } else {
                if let Some(pause_start) = last_pause_time.take() {
                    paused_duration += pause_start.elapsed();
                }
            }
            let current_time = if sink.is_paused() {
                last_running_time
            } else {
                start_time.elapsed().saturating_sub(paused_duration)
            };

            // 刷新显示 (与原代码一致)
            if last_progress_update.elapsed() >= UPDATE_INTERVAL {
                let current_time_str = format_duration(current_time);
                let track_count_str = format!("[{}/{}]", current_track_index + 1, total_tracks);
                let ext = track_path_str.split('.').last().unwrap_or("未知").to_uppercase();
                let random_str = if is_random_enabled { "随" } else { "顺" };
                let loop_str = if is_loop_enabled { "循" } else { "单" };
                let play_mode_str = format!("{}|{}", random_str, loop_str);

                let mut display_text_unpadded = format!("{}[{}][{}][][{}/{}][{:.0}%]", track_count_str, play_mode_str, ext, current_time_str, total_duration_str, sink.volume() * 100.0);

                let terminal_width = terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize;
                let current_unpadded_width = display_text_unpadded.as_str().width();
                let music_info_width = terminal_width.saturating_sub(current_unpadded_width);
                let music_info_content = format!("{}-{}", title, artist);
                let music_info = if music_info_width < 15 {
                    truncate_string(&title, music_info_width)
                } else {
                    truncate_string(&music_info_content, music_info_width)
                };
                display_text_unpadded = format!("{}[{}][{}][{}][{}/{}][{:.0}%]", track_count_str, play_mode_str, ext, music_info, current_time_str, total_duration_str, sink.volume() * 100.0);

                let new_len = display_text_unpadded.as_str().width();
                let padding_needed = terminal_width.saturating_sub(new_len);
                let padding = " ".repeat(padding_needed);
                let display_text = format!("{}{}", display_text_unpadded, padding);

                execute!(stdout, cursor::MoveToColumn(0))?;
                print!("{}", display_text);
                stdout.flush()?;
                last_progress_update = Instant::now();
            }

            // --- 用户输入处理 (非阻塞) ---
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    match key_event.code {
                        // 暂停/恢复
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            if !sink.is_paused() {
                                let currect_title = format!("[暂停]{}", initial_title);
                                execute!(stdout, SetTitle(currect_title))?;
                                sink.pause();
                            }
                        }
                        KeyCode::Char(' ') => {
                            if sink.is_paused() {
                                execute!(stdout, SetTitle(initial_title.clone()))?;
                                sink.play();
                            }
                        }
                        // 音量控制
                        KeyCode::Up => { let current_volume = sink.volume(); let new_volume = (current_volume + VOLUME_STEP).min(1.0); sink.set_volume(new_volume); }
                        KeyCode::Down => { let current_volume = sink.volume(); let new_volume = (current_volume - VOLUME_STEP).max(0.0); sink.set_volume(new_volume); }
                        // 切歌：下一首
                        KeyCode::Right => {
                            if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                            if current_track_index < total_tracks.saturating_sub(1) || is_loop_enabled {
                                sink.stop(); index_offset = 1; forced_stop = true; last_skip_time = Instant::now(); break 'inner; }
                        }
                        // 切歌：上一首
                        KeyCode::Left => {
                            if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                            if current_track_index > 0 || is_loop_enabled {
                                sink.stop(); index_offset = -1; forced_stop = true; last_skip_time = Instant::now(); break 'inner; }
                        }
                        // 退出 (Q/q 或 Ctrl+C)
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('c') => {
                            graceful_exit(&mut stdout)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        } // 内部播放循环结束

        // 9. 索引更新逻辑 (处理自动播放和强制切歌)
        if forced_stop {
            if index_offset > 0 {
                current_track_index = (current_track_index + 1) % total_tracks;
            } else if index_offset < 0 {
                current_track_index = if current_track_index == 0 { total_tracks.saturating_sub(1) } else { current_track_index - 1 };
            }
            index_offset = 0;
        } else {
            execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
            current_track_index += 1;
        }
    } // 主循环结束 'outer

    // 10. 播放列表结束后的清理工作
    graceful_exit(&mut stdout)?;

    Ok(())
}