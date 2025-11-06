
use std::{fs, io, path::{Path, PathBuf}};
use std::time::Duration;
use unicode_width::{UnicodeWidthStr, UnicodeWidthChar}; 

/// 根据终端显示宽度截断字符串，并在末尾添加 "..."。
pub fn truncate_string(s: &str, max_width: usize) -> String {
    // 1. 保留 3 个列宽给 "..."
    let ellipsis_width = 3;
    if max_width < ellipsis_width { return String::new(); }
    // 1. 获取最大显示宽度
    let max_content_width = max_width.saturating_sub(ellipsis_width);
    // 2. 检查原始字符串的显示宽度 (使用 .width() 替代 UnicodeWidthChar::width)
    let original_display_width = s.width(); // 🌟 直接在 &str 上调用 .width()
    // 如果原始字符串的显示宽度已经小于等于最大内容宽度，则直接返回
    if original_display_width <= max_width {
        return s.to_string();
    }
    // 3. 截断逻辑：基于宽度迭代
    let mut current_width = 0; // 🎯 修复 E0425：声明并初始化宽度变量
    let mut truncated_string = String::new();
    for c in s.chars() {
        // 现在直接在 char 上调用 .width()
        let char_width = c.width().unwrap_or(0);
        // 如果加上这个字符后超过了可容纳的最大内容宽度，则停止
        if current_width + char_width > max_content_width {
            break; 
        }
        truncated_string.push(c);
        current_width += char_width;
    }
    
    // 4. 返回截断后的字符串并加上省略号
    format!("{}...", truncated_string)
}

/// 递归/非递归扫描指定路径，返回支持的音频文件列表。
pub fn scan_audio_files(input_path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    
    // 如果是单个文件，直接添加
    if input_path.is_file() {
        // 在此处也可以添加扩展名检查，但为简化逻辑，假设用户直接指定的文件是音频文件
        files.push(input_path.to_path_buf());
        return Ok(files);
    }
    
    // 如果是目录，遍历并筛选文件
    if input_path.is_dir() {
        for entry in fs::read_dir(input_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext = ext.to_lowercase();
                    // 核心筛选逻辑：仅添加支持的音频格式
                    if ext == "mp3" || ext == "ogg" || ext == "flac" || ext == "aac" || ext == "m4a" || ext == "wav" { 
                        files.push(path);
                    }
                }
            }
        }
    }

    Ok(files)
}
/// 从 .txt 文件中读取播放列表路径，每行一个路径。
pub fn read_playlist_file(path: &Path) -> io::Result<Vec<PathBuf>> {
    // 尝试将整个文件内容读取为字符串
    let content = fs::read_to_string(path)?;
    
    let files: Vec<PathBuf> = content
        .lines()              // 按行迭代
        .map(|line| line.trim()) // 移除每行首尾空白
        .filter(|line| !line.is_empty()) // 忽略空行
        .map(|line| PathBuf::from(line)) // 将字符串转换为 PathBuf
        .collect();
    
    if files.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "播放列表文件为空或不包含有效路径。"));
    }
    
    Ok(files)
}

/// 将 Duration 格式化为 "MM:SS" 字符串。
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs > 0 {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    } else {
        "??:??".to_string()
    }
}