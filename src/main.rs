use clap::Parser;
// 核心音频库：用于输出流、音频解码器和播放控制 (Sink)
use rodio::{Decoder, OutputStream, Sink};
// 标准库：时间处理
use std::time::{Instant, Duration};
// 标准库：文件系统操作、I/O 缓冲和写入
use std::{fs::{self, File}, io::{self, BufReader, Write}};
// 标准库：路径处理
use std::path::{Path, PathBuf};
// ID3 标签库：用于读取音频文件的元数据（歌名、作者）
use id3::TagLike; 
// 终端交互库：用于控制终端（raw mode, 键入事件, 光标/清屏）
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode, ClearType}, // 引入 terminal::size
    cursor,
};
// symphonia 核心组件：用于更精确地获取音频文件的总时长
use symphonia::core::{
    formats::FormatOptions, meta::MetadataOptions, probe::Hint,
    io::{MediaSource, MediaSourceStream},
};
// 随机数库：用于随机播放模式下的列表洗牌
use rand::seq::SliceRandom; 

// --- 常量定义 ---
const NAME: &str = "东东播放器";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const URL: &str = "https://github.com/xa1st/mddplayer";

// ===============================================
// 【新增辅助函数】：安全地截断 UTF-8 字符串
// ===============================================
/// 将字符串截断到最大宽度 (以字符数计)，并在末尾添加 "..." (如果发生截断)。
fn truncate_string(s: &str, max_width: usize) -> String {
    // 留出 3 个字符给 "..."
    if max_width < 3 { return String::new(); } 
    // 获取终端大小
    let max_len_no_ellipsis = max_width - 3;
    // 截断字符串
    if s.chars().count() > max_width {
        // 使用 chars().take() 安全地截断 UTF-8 字符
        let truncated: String = s.chars().take(max_len_no_ellipsis).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}
// ===============================================
// 辅助函数 1: 使用 Symphonia 获取总时长 (Duration)
// ===============================================
fn get_total_duration(path: &Path) -> Duration {
    // 尝试打开文件并创建媒体源
    let source = match std::fs::File::open(path) {
        // 使用 as Box<dyn Trait> 修复编译错误
        Ok(file) => Box::new(file) as Box<dyn MediaSource>,
        Err(_) => return Duration::from_secs(0), // 无法打开则返回 0
    };
    // 创建媒体
    let media_source_stream = MediaSourceStream::new(source, Default::default());
    // 准备文件格式提示 (Hint)，加速探测
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    // 使用 symphonia 探测格式
    let probe_result = match symphonia::default::get_probe().format(&hint, media_source_stream, &FormatOptions::default(), &MetadataOptions::default())
    {
        Ok(result) => result,
        Err(_) => return Duration::from_secs(0),
    };
    // 从默认音轨参数中计算总秒数
    if let Some(track) = probe_result.format.default_track() {
        if let (Some(n_frames), Some(sample_rate)) = (track.codec_params.n_frames, track.codec_params.sample_rate) {
            let seconds = (n_frames as f64) / (sample_rate as f64);
            return Duration::from_secs_f64(seconds);
        }
    }
    Duration::from_secs(0)
}

// ===============================================
// 辅助函数 2: 扫描音频文件（单个文件或目录）
// ===============================================
fn scan_audio_files(input_path: &Path) -> io::Result<Vec<PathBuf>> {
    // 确保输入路径有效
    let mut files = Vec::new();
    // 检查是否是单个文件
    if input_path.is_file() {
        files.push(input_path.to_path_buf());
        return Ok(files);
    }
    // 如果是目录，则遍历
    if input_path.is_dir() {
        for entry in fs::read_dir(input_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext = ext.to_lowercase();
                    // 仅添加支持的音频格式（可根据需要添加更多）
                    if ext == "mp3" || ext == "flac" || ext == "wav" { 
                        files.push(path);
                    }
                }
            }
        }
    }

    Ok(files)
}

// ===============================================
// 辅助函数 3: 读取播放列表文件（.txt）
// ===============================================
fn read_playlist_file(path: &Path) -> io::Result<Vec<PathBuf>> {
    let content = fs::read_to_string(path)?;
    let files: Vec<PathBuf> = content
        .lines()
        .map(|line| line.trim()) // 移除每行路径周围的空白
        .filter(|line| !line.is_empty()) // 忽略空行
        .map(|line| PathBuf::from(line))
        .collect();
    
    if files.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "播放列表文件为空或不包含有效路径。"));
    }
    
    Ok(files)
}


// ===============================================
// 命令行参数结构体
// ===============================================

#[derive(Parser, Debug)]
#[clap(author, version = VERSION, about = NAME, long_about = None)]
// 关键：定义参数组，要求用户必须提供其中一个输入源（文件/目录 或 播放列表文件）
#[clap(group(
    clap::ArgGroup::new("input_source")
        .required(true) 
        .args(&["file_or_dir", "playlist_config"]),
))]
struct Args {
    // 【选项一：文件或目录路径】
    /// 要播放的单个音乐文件或包含音乐文件的目录路径
    #[clap(short = 'f', long, group = "input_source")] 
    file_or_dir: Option<PathBuf>, 
    
    // 【选项二：播放列表配置文件 (.txt)】
    /// 播放列表配置文件 (.txt, 一行一个路径) 路径
    #[clap(long = "list", group = "input_source")] 
    playlist_config: Option<PathBuf>, 
    
    /// 启用纯净模式，不显示程序说明模式
    #[clap(short = 'c', long)]
    clean: bool,
    
    /// 播放模式: 1 (顺序), 2 (倒序), 3 (随机)
    #[clap(short = 'm', long, default_value = "1")] 
    mode: u8, 
    
    /// 播放列表播放完毕后是否循环播放 (Loop Play)
    #[clap(long = "loop")]
    loop_play: bool,
}

// ===============================================
// MAIN 函数
// ===============================================
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let play_mode: u8 = args.mode;
    let is_loop_enabled = args.loop_play; 

    // 1. 根据命令行参数获取文件列表
    let mut playlist = if let Some(path) = args.file_or_dir {
        // 模式一：文件或目录
        match scan_audio_files(path.as_path()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("错误：无法读取路径或文件：{}", e);
                return Err(e.into());
            }
        }
    } else if let Some(config_path) = args.playlist_config {
        // 模式二：播放列表文件
        match read_playlist_file(config_path.as_path()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("错误：无法读取播放列表配置文件 {:?}：{}", config_path, e);
                return Err(e.into());
            }
        }
    } else {
        // 理论上不可能到达这里，因为 clap 要求必须提供输入源
        unreachable!(); 
    };

    if playlist.is_empty() {
        eprintln!("错误：在指定的路径中未找到支持的音频文件 (.mp3, .flac, .wav)。");
        return Ok(());
    }

    // 2. 应用播放模式：排序或洗牌
    match play_mode {
        2 => playlist.reverse(), // 倒序
        3 => {
            let mut rng = rand::thread_rng();
            playlist.shuffle(&mut rng); // 随机洗牌
        },
        1 | _ => { 
            /* 1 或其他值：默认顺序，无需操作，同时处理了无效输入*/ 
        }
    }

    // ----------------------------------------------------
    // --- 核心播放逻辑：初始化和播放循环 ---
    // ----------------------------------------------------

    let mut stdout = std::io::stdout();
    
    // 终端初始化：清屏、进入 Raw Mode（实现实时按键监听）、隐藏光标
    execute!(stdout, crossterm::terminal::Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
    enable_raw_mode()?; 
    execute!(stdout, cursor::Hide)?;
    
    // 初始化音频输出和 Sink（Rodio 核心组件）
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    
    // 设置默认音量为 75%
    const DEFAULT_VOLUME: f32 = 0.75;
    sink.set_volume(DEFAULT_VOLUME);

    // 显示界面信息（非纯净模式下）
    if !args.clean {
        // ... (省略打印控制说明的代码，因为它没有变化) ...
        println!("\n=======================================================");
        println!("  {} (v.{})", NAME, VERSION);
        println!("  主页: {}", URL);
        println!("=======================================================");
        println!("==================【🕹️ 控 制 说 明】===================");
        println!("  [P] 键: ...... 暂停播放  [空格] 键: ...... 恢复播放");
        println!("  [←] 键: ...... 上一首    [→] 键: ...... 下一首");
        println!("  [↑] 键: ...... 放大音量  [↓] 键: ...... 减少音量");
        println!("  [Q] 键: ...... 退出播放");
        println!("=======================================================");
    }

    // --- 主循环：迭代播放列表 ---
    let total_tracks = playlist.len();
    let mut current_track_index: usize = 0;
    let mut index_offset: i32 = 0; 
    
    const MIN_SKIP_INTERVAL: Duration = Duration::from_millis(250); 
    let mut last_skip_time = Instant::now() - MIN_SKIP_INTERVAL; 
    
    const VOLUME_STEP: f32 = 0.05; 
    
    // 循环开始
    loop { 
        // 循环播放逻辑
        if current_track_index >= total_tracks {
            if is_loop_enabled {
                current_track_index = 0; // 重置到第一首
            } else {
                break; // 退出整个播放循环
            }
        }

        // 【✅ 新增：获取终端宽度】
        let terminal_width = terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize;
        // 预留给固定文本（符号、计数、时间、音量）的宽度
        // 🎝 正在播放: [X/Y] [ - ] - [MM:SS / MM:SS] (音量: 100%)
        // 估算固定文本约 50-60 个字符 (取决于数字位数)
        const FIXED_TEXT_OVERHEAD: usize = 65; 
        let available_width = terminal_width.saturating_sub(FIXED_TEXT_OVERHEAD);
        // 分配给 标题 和 艺术家 的宽度，假设大致对半
        let title_artist_width = available_width / 3;
        
        // ... (文件加载、解码、元数据获取等代码保持不变) ...
        
        let track_path = &playlist[current_track_index];
        let track_path_str = track_path.to_string_lossy();
        
        // 1. 文件加载、解码、添加到 Sink
        let file = match File::open(&track_path) {
            Ok(f) => BufReader::new(f),
            Err(e) => {
                eprintln!("\n⚠️ 跳过文件 {}: 无法打开或读取。错误: {}", track_path_str, e);
                current_track_index += 1; // 切换到下一首
                continue; // 跳过后续逻辑，进入下一轮 loop 循环
            }
        };
        
        sink.clear();
        sink.append(Decoder::new(file)?);
        
        if sink.is_paused() {
            sink.play();
        }

        // 2. 获取元数据和总时长
        let (mut title, mut artist) = match id3::Tag::read_from_path(&track_path) {
            Ok(tag) => (
                tag.title().unwrap_or("未知音乐名").to_string(),
                tag.artist().unwrap_or("未知作者").to_string(),
            ),
            Err(_) => ("未知音乐名".to_string(), "未知作者".to_string()),
        };
        
        // 【✅ 应用截断逻辑】
        title = truncate_string(&title, title_artist_width);
        artist = truncate_string(&artist, title_artist_width);

        let total_duration = get_total_duration(track_path.as_path());
        let total_duration_str = if total_duration.as_secs() > 0 {
            format!("{:02}:{:02}", total_duration.as_secs() / 60, total_duration.as_secs() % 60)
        } else {
            "??:??".to_string()
        };
        
        // 3. 计时器重置
        let start_time = Instant::now();
        let mut paused_duration = Duration::from_secs(0); 
        let mut last_pause_time: Option<Instant> = None; 
        let mut last_progress_update = Instant::now();
        const UPDATE_INTERVAL: Duration = Duration::from_millis(1000); 
        
        let mut forced_stop = false; 

        // 4. 内部播放循环 (当前歌曲播放循环)
        while !sink.empty() {
            // --- 时间计算 ---
            let mut current_time = Duration::from_secs(0);
            if sink.is_paused() {
                if last_pause_time.is_none() { last_pause_time = Some(Instant::now()); }
            } else {
                current_time = start_time.elapsed() - paused_duration;
                if let Some(pause_start) = last_pause_time.take() {
                    paused_duration += pause_start.elapsed();
                }
            }
            
            // --- 刷新显示 ---
            if last_progress_update.elapsed() >= UPDATE_INTERVAL {
                let current_time_str = format!("{:02}:{:02}", current_time.as_secs() / 60, current_time.as_secs() % 60);
                
                let track_count_str = format!("[{}/{}]", current_track_index + 1, total_tracks); 
                
                let display_text = format!("🎝 正在播放: {} [{}][{} - {}] - [{} / {}] (音量: {:.0}%)", 
                    track_count_str, 
                    track_path_str.split('.').last().unwrap_or("未知").to_uppercase(),
                    title, // 使用已截断的标题
                    artist, // 使用已截断的艺术家
                    current_time_str, 
                    total_duration_str,
                    sink.volume() * 100.0
                );
                // 移动光标到行首，清空当前行，并打印进度信息
                // 此处清空的是逻辑上的第一行，但因为我们已经限制了长度，所以不会折行，清空有效。
                execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
                // 刷新显示
                print!("{}", display_text);
                // 刷新标准输出
                stdout.flush()?; 
                // 更新上次进度更新时间
                last_progress_update = Instant::now();
            }
            // --- 用户输入处理 (保持不变) ---
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    match key_event.code {
                        // 暂停/恢复
                        KeyCode::Char('p') | KeyCode::Char('P') => { if !sink.is_paused() { sink.pause(); last_pause_time = Some(Instant::now()); } }
                        KeyCode::Char(' ') => { if sink.is_paused() { sink.play(); last_pause_time = None; } }
                        // 音量控制
                        KeyCode::Up => { let current_volume = sink.volume(); let new_volume = (current_volume + VOLUME_STEP).min(1.0); sink.set_volume(new_volume); }
                        KeyCode::Down => { let current_volume = sink.volume(); let new_volume = (current_volume - VOLUME_STEP).max(0.0); sink.set_volume(new_volume); }
                        // 切歌
                        KeyCode::Right => { if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                                            if current_track_index < total_tracks - 1 || is_loop_enabled {
                                                sink.stop(); index_offset = 1; forced_stop = true; last_skip_time = Instant::now(); break; } }
                        KeyCode::Left => { if last_skip_time.elapsed() < MIN_SKIP_INTERVAL { continue; }
                                            if current_track_index > 0 || is_loop_enabled {
                                                sink.stop(); index_offset = -1; forced_stop = true; last_skip_time = Instant::now(); break; } }
                        // 退出
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
                            println!("👋 播放器退出。");
                            disable_raw_mode()?;
                            execute!(stdout, cursor::Show)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        } // 内部 while 循环结束
        // 【索引更新逻辑 (保持不变) 】
        if forced_stop {
            if index_offset > 0 {
                current_track_index = (current_track_index + 1) % total_tracks; 
            } else if index_offset < 0 {
                current_track_index = if current_track_index == 0 { total_tracks - 1 } else { current_track_index - 1 };
            }
            index_offset = 0; 
        } else {
            execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
            current_track_index += 1; 
        }
    } // 主 loop 循
    // 播放列表已全部播放完毕
    execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
    // 播放列表已全部播放完毕
    println!("播放列表已全部播放完毕。");
    // 恢复终端
    disable_raw_mode()?;
    // 显示光标
    execute!(stdout, cursor::Show)?;
    // 打完收工
    Ok(())
}