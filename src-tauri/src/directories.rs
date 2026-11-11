//! 数据、日志、设置文件所在的目录；纯数据，供 settings 解析、log 和 history 使用。
use std::path::PathBuf;

#[derive(Clone)]
pub struct Directories {
    pub settings_file: PathBuf,
    pub data: PathBuf,
    pub logs: PathBuf,
    pub data_source: &'static str,
    pub test_data_dir: bool,
}
