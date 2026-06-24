name := 'cosmic-comic'
export APPID := 'com.tsingel.CosmicComic'

rootdir := ''
prefix := '/usr'
base-dir := absolute_path(clean(rootdir / prefix))

bin-src := 'target' / 'release' / name
bin-dst := base-dir / 'bin' / name

desktop-src := 'res' / (APPID + '.desktop')
desktop-dst := base-dir / 'share' / 'applications' / (APPID + '.desktop')

icon-src := 'res' / 'icons' / (APPID + '.svg')
icon-dst := base-dir / 'share' / 'icons' / 'hicolor' / 'scalable' / 'apps' / (APPID + '.svg')

# Default recipe: build in release mode
default: build-release

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Run the app locally without installing
run *args:
    cargo run --release {{args}}

# Installs the binary, desktop entry, and icon (defaults to /usr, override with `just prefix=/usr/local install`)
install:
    install -Dm0755 {{bin-src}} {{bin-dst}}
    install -Dm0644 {{desktop-src}} {{desktop-dst}}
    install -Dm0644 {{icon-src}} {{icon-dst}}

# Removes installed files
uninstall:
    rm -f {{bin-dst}} {{desktop-dst}} {{icon-dst}}

# Runs `cargo clean`
clean:
    cargo clean
