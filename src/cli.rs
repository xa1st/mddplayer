// 引入 clap 库的 Parser 宏，用于自动生成命令行解析逻辑
use clap::Parser;

// --- 常量定义 ---
pub const NAME: &str = "东东播放器";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const URL: &str = "github.com/xa1st/mddplayer";

/// 命令行参数结构体
/// 使用 #[derive(Parser)] 自动从结构体定义生成解析器
#[derive(Parser, Debug)]
// 设置程序信息，用于 --help 或 -V
#[clap(author, version = VERSION, about = NAME, long_about = None)]
// 命令行参数定义
pub struct Args {
    /// 这里直接改为输出文件，不再带-f选项
    #[arg(index = 1)]
    pub file: String,
    
    /// 启用纯净模式，不显示程序说明模式（如操作指南）
    #[clap(short = 's', long = "simple")]
    pub clean: bool,

    /// 启用随机模式，不使用则为顺序模式
    #[clap(short = 'r', long = "random")]
    pub random: bool,
    
    /// 是否循环播放
    #[clap(short = 'l', long = "loop")] 
    pub is_loop: bool, 
    
    /// 播放音量
    #[clap(short = 'v', long = "volume", default_value = "75")]
    pub volume: u8,
}