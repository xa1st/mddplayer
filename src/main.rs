use clap::Parser;
// 核心库
use rodio::{Decoder, OutputStream, Sink};
use std::time::{Instant, Duration};
use std::{fs::File, io::{BufReader, Write}};
use std::path::Path;

// Trait 导入，解决 E0599
// use rodio::Source;
use id3::TagLike; 

// 终端交互
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, ClearType},
    cursor,
};

// 引入 symphonia 的核心组件
use symphonia::core::{
    formats::FormatOptions, meta::MetadataOptions, probe::Hint,
    io::{MediaSource, MediaSourceStream},
};


// ===============================================
// 辅助函数 1: 使用 Symphonia 获取总时长 (Duration)
// ===============================================

/// 使用 Symphonia 尝试获取音频文件的总时长
/// 使用 Symphonia 尝试获取音频文件的总时长
fn get_total_duration(path: &Path) -> Duration {
    // 创建文件读取器 (source 是 Box<File>)
    let source = match std::fs::File::open(path) {
        Ok(file) => Box::new(file) as Box<dyn MediaSource>,
        Err(_) => return Duration::from_secs(0),
    };
    // 将媒体源封装在 MediaSourceStream 中 (修复 E0308)
    // symphonia 要求 MediaSource 必须被包装起来，以便内部处理寻址。
    let media_source_stream = MediaSourceStream::new(source, Default::default());
    // 探测媒体格式
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    // 将封装后的 stream 传递给 format 方法
    let probe_result = match symphonia::default::get_probe().format(&hint, media_source_stream, &FormatOptions::default(), &MetadataOptions::default())
    {
        Ok(result) => result,
        Err(_) => return Duration::from_secs(0),
    };
    // 计算总时长
    if let Some(track) = probe_result.format.default_track() {
        if let (Some(n_frames), Some(sample_rate)) = (track.codec_params.n_frames, track.codec_params.sample_rate) {
            let seconds = (n_frames as f64) / (sample_rate as f64);
            return Duration::from_secs_f64(seconds);
        }
    }
    Duration::from_secs(0)
}

const NAME: &str = "猫东东的音乐播放器";
const VERSION: &str = "1.0.1";
const URL: &str = "https://github.com/xa1st/music-player-cli";

// ===============================================
// 命令行参数结构体
// ===============================================
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// 要播放的音乐文件路径
    #[clap(short, long)]
    file: String, // 音乐文件路径
    /// 启用纯净模式,
    #[clap(short, long)]
    clean: bool,
}

// ===============================================
// MAIN 函数
// ===============================================
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let file_path = args.file;
    // 初始化音频输出和解码
    let (_stream, stream_handle) = OutputStream::try_default()?;
    // 创建 Sink
    let sink = Sink::try_new(&stream_handle)?;
    // 创建 BufReader
    let file = BufReader::new(File::open(&file_path)?);
    // 创建解码器
    let source = Decoder::new(file)?;
    // 添加源
    sink.append(source);
    // 获取元数据和总时长
    // ID3 标签 (音乐名/作者)
    let (title, artist) = match id3::Tag::read_from_path(&file_path) {
        Ok(tag) => (
            tag.title().unwrap_or("未知音乐名").to_string(),
            tag.artist().unwrap_or("未知作者").to_string(),
        ),
        Err(_) => ("未知音乐名".to_string(), "未知作者".to_string()),
    };
    // 总时长 (Symphonia)
    let total_duration = get_total_duration(Path::new(&file_path));
    // 格式化总时长字符串
    let total_duration_str = if total_duration.as_secs() > 0 {
        format!("{:02}:{:02}", total_duration.as_secs() / 60, total_duration.as_secs() % 60)
    } else {
        "??:??".to_string()
    };
    // 计时器和显示控制
    let start_time = Instant::now();
    let mut current_time = Duration::from_secs(0);
    let mut paused_duration = Duration::from_secs(0); 
    let mut last_pause_time: Option<Instant> = None; 
    let mut last_progress_update = Instant::now();
    let update_interval = Duration::from_millis(1000); // 每 1 秒刷新一次，减少闪烁

    // --- 重点新增代码：清屏操作 ---
    let mut stdout = std::io::stdout();
    // 使用 ClearType::All 清除整个屏幕
    execute!(stdout, crossterm::terminal::Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
    // 启用 Raw Mode
    enable_raw_mode()?; 
    let mut stdout = std::io::stdout();
    // 隐藏光标以减少闪烁
    execute!(stdout, cursor::Hide)?;

    if !args.clean { 
        // 播放时显示的界面
        println!("\n=======================================================");
        // 使用格式化宏 {NAME:<40} 来确保 NAME 后面有足够的空格，保持右侧对齐
        println!("  {} (v.{})", NAME, VERSION);
        println!("  主页: {}", URL);
        println!("=======================================================");
        println!("==================【🕹️ 控 制 说 明】===================");
        println!("  [P] 键: ......................... 暂停播放");
        println!("  [空格] 键: ...................... 恢复播放");
        println!("  [Q] 键: ......................... 退出播放");
        println!("=======================================================");
        // 留白一行给进度条
        // println!("\n");
    }
    loop {
        // 时间计算
        if sink.is_paused() {
            if last_pause_time.is_none() {
                last_pause_time = Some(Instant::now()); 
            }
        } else {
            // 只有在播放时，时间才流逝
            current_time = start_time.elapsed() - paused_duration;
        }
        // 刷新显示
        if last_progress_update.elapsed() >= update_interval {
            // 格式化当前时间字符串
            let current_time_str = format!("{:02}:{:02}", current_time.as_secs() / 60, current_time.as_secs() % 60);
            // 构建要求的显示字符串
            let display_text = format!("🎝 正在播放: [{} - {}] - [{}-{}]", title, artist, current_time_str, total_duration_str);
            // 打印时间信息，使用 \r 和 ClearType::CurrentLine 确保覆盖
            execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
            print!("{}", display_text);
            stdout.flush()?; 
            last_progress_update = Instant::now();
        }
        // 用户输入处理
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key_event) = event::read()? {
                match key_event.code {
                    // 暂停播放 (P)
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        if !sink.is_paused() {
                            sink.pause();
                            // 暂停：记录暂停开始时间
                            last_pause_time = Some(Instant::now());
                        }
                    }
                    // 恢复播放(空格)
                    KeyCode::Char(' ') => {
                        if sink.is_paused() { // 只有当前处于暂停时才播放
                            sink.play();
                            // 恢复播放：更新暂停补偿时长
                            if let Some(pause_start) = last_pause_time.take() {
                                paused_duration += pause_start.elapsed();
                            }
                        }
                    }
                    // 退出 (Q)
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        // 清除进度行
                        execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
                        println!("👋 播放器退出。");
                        break; 
                    }
                    _ => {}
                }
            }
        }
        // 播放完毕检查
        if sink.empty() {
            execute!(stdout, crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(ClearType::CurrentLine))?;
            println!("🎶 歌曲播放完毕。");
            break;
        }
    }
    // 清理和退出
    disable_raw_mode()?;
    // 非常重要，必须在退出前恢复光标，不然没光标了
    execute!(stdout, cursor::Show)?;
    // 打完收工
    Ok(())
}