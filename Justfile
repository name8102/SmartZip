# Install the SmartZip CLI into Cargo's user-wide bin directory.
install:
    cargo install --path crates/smartzip-cli --bin smartzip --locked --force

# Build, package and install the macOS GUI (release by default).
[positional-arguments]
install-gui profile="release" destination="~/Applications/SmartZip.app":
    python3 scripts/install-macos.py --profile "$1" --destination "$2"
