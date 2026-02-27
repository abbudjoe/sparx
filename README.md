# sparx 🔥

Minimal image-to-terminal renderer. Braille + truecolor ANSI.

## Usage

```bash
cargo install sparx
sparx logo.png
sparx logo.png --width 60 --threshold 100
```

## Library

```rust
use sparx::{render_file, RenderConfig};

let output = render_file("logo.png", &RenderConfig::default()).unwrap();
print!("{}", output);
```

## License

MIT
