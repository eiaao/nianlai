use focus_matrix::{cli, preflight, scenario};

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(match run() {
        Ok(code) => code,
        Err(error) => {
            if std::env::args().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::json!({"ready":false,"error":error,"exit_code":2})
                );
            } else {
                eprintln!("用法错误：{error}");
            }
            2
        }
    })
}

fn run() -> Result<u8, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "helper-window") {
        #[cfg(windows)]
        return focus_matrix::win::helper(&args[1..]).map(|()| 0);
        #[cfg(not(windows))]
        return Err("helper-window 只支持 Windows".into());
    }
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!("{}", cli::HELP);
        return Ok(0);
    }
    let options = cli::Options::parse(args)?;
    let source = if let Some(path) = &options.scenarios {
        std::fs::read_to_string(path).map_err(|e| format!("场景文件无法读取：{e}"))?
    } else {
        scenario::DEFAULT.into()
    };
    let config: scenario::Config =
        serde_json::from_str(&source).map_err(|e| format!("场景 JSON：{e}"))?;
    config.validate()?;
    let selected = config.select(&options.only)?;
    let preflight = preflight::inspect(&options, &config, &selected);
    if !options.real_input {
        if options.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&preflight).map_err(|e| e.to_string())?
            );
        } else {
            println!("{}", preflight.chinese());
        }
        return Ok(u8::from(
            !preflight.ready || preflight.plan.iter().any(|p| p.status == "blocked"),
        ));
    }
    if !preflight.ready {
        eprintln!("{}", preflight.chinese());
        return Ok(1);
    }
    #[cfg(windows)]
    return Ok(focus_matrix::win::run(options, config, source, preflight));
    #[cfg(not(windows))]
    {
        eprintln!("真跑只支持 Windows；场景校验已完成。");
        Ok(1)
    }
}
