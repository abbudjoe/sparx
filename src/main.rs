use sparx::{render_file, RenderConfig};

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let Some(image_path) = args.next() else {
        return Err(usage());
    };

    let mut width = None;
    let mut threshold: u8 = 128;
    let mut color = true;

    let remaining: Vec<String> = args.collect();
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--width" => {
                let value = remaining.get(i + 1).ok_or_else(usage)?;
                width = Some(parse_u32(value, "width")?);
                i += 2;
            }
            "--threshold" => {
                let value = remaining.get(i + 1).ok_or_else(usage)?;
                threshold = parse_u8(value, "threshold")?;
                i += 2;
            }
            "--no-color" => {
                color = false;
                i += 1;
            }
            _ => return Err(format!("Unknown argument: {}\n{}", remaining[i], usage())),
        }
    }

    let cfg = RenderConfig {
        width,
        threshold,
        color,
    };

    let rendered = render_file(&image_path, &cfg).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}

fn parse_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("Invalid {name}: {value}"))
}

fn parse_u8(value: &str, name: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|_| format!("Invalid {name}: {value}"))
}

fn usage() -> String {
    "Usage: sparx <image_path> [--width N] [--threshold N] [--no-color]".to_string()
}
