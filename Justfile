# Build, package and install the native CLI and GUI (Linux/macOS).
[positional-arguments]
install profile="release" destination="":
    python3 scripts/install_desktop.py --profile "$1" --destination "$2"

# Install only the native GUI, using the platform's default application directory.
[positional-arguments]
install-gui profile="release" destination="":
    python3 scripts/install_desktop.py --gui-only --profile "$1" --destination "$2"
