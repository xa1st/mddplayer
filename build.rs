fn main() -> std::io::Result<()> {
    // 只有在目标是 Windows 平台时才执行此操作
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        // 设置图标
        res.set_icon("app.ico");
        // 设置产品名称
        res.set("ProductName", "猫东东的音乐播放器"); 
        // 设置文件描述
        res.set("FileDescription", "一个简约的命令行音乐播放器。"); 
        // 设置文件名
        res.set("OriginalFilename", "mddplayer.exe");
        // 设置公司名称
        res.set("CompanyName", "猫东东 https://bsay.de"); 
        // 设置版权
        res.set("LegalCopyright", "Copyright © 2025 猫东东. All rights reserved."); 
        // 编译资源并链接到最终的 EXE
        res.compile()?;
    }
    Ok(())
}