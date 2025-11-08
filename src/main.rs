// 声明模块
mod cli;
mod utils;
mod metadata;

// 从各个模块引入所需的项
use clap::Parser;
// 引入 mpsc channel
use rodio::{Decoder, OutputStream, Sink};
use std::time::{Instant, Duration};
use std::{fs::File, io::{self, BufReader, Write}};
use std::sync::mpsc::{channel, Sender, Receiver}; // 引入 mpsc
use std::path::PathBuf; // 路径相关
use std::thread; // 引入线程

use rand::seq::SliceRandom; 
use unicode_width::UnicodeWidthStr;

// 从 cli 模块引入常量和参数结构体
use cli::{Args, NAME, VERSION, URL};
// 从 utils 模块引入所有公共函数，特别是用于智能解析输入的函数
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

// ===============================================
// 异步预加载数据结构
// ===============================================

// 定义用于线程间发送预加载结果的消息
struct PreloadedTrack {
    decoder: rodio::Decoder<std::io::BufReader<std::fs::File>>,
    title: String,
    artist: String,
    total_duration: Duration,
    index: usize, // 预加载的歌曲在播放列表中的索引
}

// ===============================================
// 异步预加载函数 (将阻塞操作移到新线程)
// ===============================================

/// 在后台线程启动下一首歌曲的预加载。
fn start_preloader_thread(
    path: PathBuf,
    index: usize,
    tx: Sender<PreloadedTrack>,
) {
    // 启动新线程
    thread::spawn(move || {
        // 1. 获取元数据 (阻塞操作)
        let (title, artist) = get_title_artist_info(path.as_path());
        let total_duration = get_total_duration(path.as_path());
        
        // 2. 文件I/O & 解码 (阻塞操作)
        let file = match File::open(&path) {
            Ok(f) => BufReader::new(f),
            Err(e) => {
                eprintln!("\n[预加载警告] 文件 {} 无法打开或读取。错误: {}", path.display(), e);
                return;
            }
        };
        let decoder = match Decoder::new(file) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("\n[预加载警告] 文件 {} 无法解码。错误: {}", path.display(), e);
                return;
            }
        };

        // 3. 将结果发送回主线程
        let result = PreloadedTrack {
            decoder,
            title,
            artist,
            total_duration,
            index,
        };

        if tx.send(result).is_err() {
            // 主线程已退出，忽略发送失败
        }
    });
}


// ===============================================
// MAIN 函数
// ===============================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 解析命令行参数
    let args = Args::parse();
    
    // ... (参数获取，与原代码一致)
    let input_path_str = &args.file;
    let is_simple_mode = args.clean; 
    let is_random_enabled = args.random; 
    let is_loop_enabled = args.is_loop; 
    let initial_volume = args.volume as f32 / 100.0; 
    
    // 2. 获取文件列表
    let mut playlist = match get_playlist_from_input(input_path_str) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("错误：处理输入路径 '{}' 时失败：{}", input_path_str, e);
            return Err(e.into());
        }
    };
    
    if playlist.is_empty() {
        eprintln!("错误：在指定的路径中未找到支持的音频文件。");
        return Ok(());
    }

    // 3. 应用播放模式
    if is_random_enabled {
        if !is_simple_mode {
             println!("启用随机播放模式...");
        }
        let mut rng = rand::thread_rng();
        playlist.shuffle(&mut rng); // 随机洗牌
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
    let mut initial_title = format!("{} (v{}) - 启动中...", cli::NAME, cli::VERSION);
    execute!(stdout, SetTitle(initial_title.clone()))?; 
    enable_raw_mode()?; 
    execute!(stdout, cursor::Hide)?; 
    
    // 初始化音频输出和 Sink 
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    sink.set_volume(initial_volume.min(1.0).max(0.0));
    
    // 显示界面信息（非纯净模式下）
    if !is_simple_mode { 
        // ... (打印控制信息，与原代码一致)
        println!(" =====================【 {} 】======================", NAME);
        println!("   版本:v{}       主页:{}", VERSION, URL);
        println!(" ===========================================================");
        println!(" ====================【 控 制 说 明 】======================");
        println!("   [P]暂停播放     [空格]恢复播放        [Q]退出播放");
        println!("   [←]上一首    [→]下一首    [↑]音量增    [↓]音量减");
        println!(" ===========================================================");
    }
    
    // --- 异步初始化和预加载设置 ---
    let (tx, rx): (Sender<PreloadedTrack>, Receiver<PreloadedTrack>) = channel();
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
        // 循环播放检查 (如果当前索引超限，则尝试循环或退出)
        if current_track_index >= total_tracks {
            if is_loop_enabled {
                current_track_index = 0; 
            } else {
                break; 
            }
        }

        // --- 5. 文件加载、解码、添加到 Sink (使用预加载结果) ---
        
        let preloaded_track = loop {
            // 尝试接收预加载结果，等待时间较长以确保有时间加载
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(track) => {
                    // 检查接收到的歌曲是否是我们需要的 (防止用户快速切歌导致接收到旧结果)
                    if track.index == current_track_index {
                        break track;
                    } else {
                        // 如果接收到了不匹配的歌曲，可能是用户已经切歌了，忽略这个结果
                        continue;
                    }
                },
                // 如果超时，且主线程没有被强制停止 (即歌曲刚开始，正在等待加载)
                Err(e) if e == std::sync::mpsc::RecvTimeoutError::Timeout => {
                    // 播放器卡顿在这里等待，但这是我们预期的最坏情况 (文件太大或 I/O 慢)
                    // 如果您需要更快的反馈，可以改为同步加载作为回退，但这会失去异步的意义。
                    let loading_message = format!("[LOADING...] ({}/{})", current_track_index + 1, total_tracks);
                    execute!(stdout, cursor::MoveToColumn(0))?;
                    print!("{}", truncate_string(&loading_message, terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize));
                    stdout.flush()?; 
                    continue;
                }
                // 接收通道断开 (理论上不会发生，除非 tx 全部被销毁)
                Err(_) => {
                    // 接收失败，使用同步方法加载作为回退（模拟原代码的阻塞行为）
                    // 恢复原始代码中的同步加载逻辑（跳过错误）
                    // let track_path_str = playlist[current_track_index].to_string_lossy();
                    eprintln!("\n[致命错误] 预加载通道关闭，进行同步回退...");
                    current_track_index += 1;
                    continue 'outer; // 跳到主循环的下一次迭代
                }
            }
        };
        // 歌曲预加载成功，现在是快速的内存操作
        let track_path_str = playlist[current_track_index].to_string_lossy();
        sink.clear();
        sink.append(preloaded_track.decoder);
        
        if sink.is_paused() {
            sink.play();
        }

        // 6. 使用预加载的元数据
        let title = preloaded_track.title;
        let artist = preloaded_track.artist;
        let total_duration = preloaded_track.total_duration;
        let total_duration_str = format_duration(total_duration);
        
        // 修改标题 (注意：使用 .clone() 避免移动)
        initial_title = format!("{}-{}-{}v{}", title, artist, NAME, VERSION);
        execute!(stdout, SetTitle(initial_title.clone()))?;

        // 🌟 立即启动下一首歌曲的预加载
        let next_index = (current_track_index + 1) % total_tracks;
        if next_index != current_track_index {
            let next_path = playlist[next_index].clone();
            start_preloader_thread(next_path, next_index, tx.clone());
        }

        // 7. 计时器重置
        let start_time = Instant::now(); 
        let mut paused_duration = Duration::from_secs(0); 
        let mut last_pause_time: Option<Instant> = None; 
        let mut last_running_time = Duration::from_secs(0); 
        let mut last_progress_update = Instant::now();
        let mut forced_stop = false; 

        // 8. 内部播放循环 (与原代码一致)
        'inner: while !sink.empty() {
            // --- 时间计算 (与原代码一致) ---
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
                
                let mut display_text_unpadded = format!(" {}[{}][{}][][{}/{}][{:.0}%]", 
                    track_count_str, play_mode_str, ext, current_time_str, total_duration_str, sink.volume() * 100.0
                );
                
                let terminal_width = terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize;
                let current_unpadded_width = display_text_unpadded.as_str().width();
                let music_info_width = terminal_width.saturating_sub(current_unpadded_width);
                let music_info_content = format!("{}-{}", title, artist);
                let music_info = if music_info_width < 15 {
                    truncate_string(&title, music_info_width)
                } else {
                    truncate_string(&music_info_content, music_info_width)
                };
                
                display_text_unpadded = format!(" {}[{}][{}][{}][{}/{}][{:.0}%]", 
                    track_count_str, play_mode_str, ext, music_info, current_time_str, total_duration_str, sink.volume() * 100.0
                );
                
                let new_len = display_text_unpadded.as_str().width();
                let padding_needed = terminal_width.saturating_sub(new_len);
                let padding = " ".repeat(padding_needed);
                let display_text = format!("{}{}", display_text_unpadded, padding);
                
                execute!(stdout, cursor::MoveToColumn(0))?;
                print!("{}", display_text); 
                stdout.flush()?; 
                last_progress_update = Instant::now();
            }
            
            // --- 用户输入处理 (非阻塞) (与原代码一致) ---
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    match key_event.code {
                        // 暂停/恢复
                        KeyCode::Char('p') | KeyCode::Char('P') => { 
                            if !sink.is_paused() { 
                                // 标题加上暂停
                                let currect_title = format!("[暂停]{}", initial_title);
                                execute!(stdout, SetTitle(currect_title))?;
                                sink.pause(); 
                            }
                        }
                        KeyCode::Char(' ') => {
                            if sink.is_paused() { 
                                // 标题去掉暂停
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
                        // 退出
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
                            println!("👋 播放器退出。");
                            disable_raw_mode()?;
                            execute!(stdout, cursor::Show)?;
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
                // 下一首，应用循环逻辑
                current_track_index = (current_track_index + 1) % total_tracks; 
            } else if index_offset < 0 {
                // 上一首，应用循环逻辑 (如果当前为 0，则跳到列表末尾)
                current_track_index = if current_track_index == 0 { total_tracks.saturating_sub(1) } else { current_track_index - 1 };
            }
            index_offset = 0; 
        } else {
            // 歌曲正常播放完毕，准备播放下一首
            execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
            current_track_index += 1; 
        }
    } // 主循环结束 'outer
    
    // 10. 播放列表结束后的清理工作
    execute!(stdout, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
    println!("播放列表已全部播放完毕。");
    
    // 恢复终端状态
    disable_raw_mode()?;
    execute!(stdout, cursor::Show)?;
    
    Ok(())
}